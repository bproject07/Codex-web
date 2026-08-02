use std::{
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use url::Url;

use codex_web_terminal::{
    agents::build_agent_profiles,
    auth::{AuthState, generate_token},
    config::{Config, static_directory},
    filesystem::DirectoryBrowser,
    peer::PeerBroker,
    peer_cli, peer_routes,
    registry::SessionRegistry,
    routes::{AppState, build_router},
    update_bootstrap,
    updater::{UpdateActivation, UpdateManager},
    workspaces::{WorkspaceStore, prepare_state_directory_sync},
};

const SERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
type ServerTaskResult = std::result::Result<io::Result<()>, JoinError>;

enum StopCause {
    Signal(io::Result<()>),
    Public(ServerTaskResult),
    Peer(ServerTaskResult),
    Update(UpdateActivation),
    Restart,
    UpdateChannelClosed,
    RestartChannelClosed,
}

fn main() -> Result<()> {
    if peer_cli::try_run_from_environment()? {
        return Ok(());
    }

    let worker_context = update_bootstrap::take_worker_context()?;
    let mut config = Config::load()?;
    let token = config.token.take().unwrap_or(generate_token()?);
    // SAFETY: the synchronous outer main has not created the Tokio runtime or
    // any application threads yet. The parsed token remains in owned memory.
    unsafe {
        std::env::remove_var("CODEX_WEB_TOKEN");
    }
    prepare_state_directory_sync(&config.state_dir).with_context(|| {
        format!(
            "failed to prepare the protected state directory {}",
            config.state_dir.display()
        )
    })?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to initialize the Tokio runtime")?;
    runtime.block_on(run(config, token, worker_context))
}

async fn run(
    config: Config,
    token: String,
    worker_context: update_bootstrap::WorkerContext,
) -> Result<()> {
    let previous_executable =
        std::env::current_exe().context("failed to locate the running executable")?;
    if !worker_context.supervised
        && update_bootstrap::supervise_startup(&config, &token, &previous_executable).await?
    {
        return Ok(());
    }
    let supervised_worker = worker_context.supervised;
    let readiness_nonce = worker_context.readiness_nonce;
    let server_restart_supported = worker_context.server_restart_supported;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_new(&config.log_level)
                .with_context(|| format!("invalid log filter: {}", config.log_level))?,
        )
        .with_target(false)
        .init();

    let static_directory = static_directory();
    if static_directory.is_none() {
        tracing::warn!(
            "frontend build not found; use the Vite development server or build the web package"
        );
    }

    let peer_listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .context("failed to bind the private peer bridge")?;
    let peer_endpoint = peer_listener
        .local_addr()
        .context("failed to resolve the private peer bridge address")?;
    let helper_path =
        std::env::current_exe().context("failed to resolve the peer helper executable")?;
    let peers = PeerBroker::new(peer_endpoint, helper_path);

    let agent_profiles = build_agent_profiles(&config);
    let agent_catalog = agent_profiles.catalog.clone();
    let sessions = SessionRegistry::with_agent_configs_and_peer_broker_with_max_sessions(
        agent_profiles.primary,
        agent_profiles.new_session,
        agent_profiles.additional,
        peers.clone(),
        config.max_sessions,
    );
    let directories = DirectoryBrowser::new(config.project_dir.clone());
    let workspaces = WorkspaceStore::open(config.state_dir.clone())
        .await
        .with_context(|| {
            format!(
                "failed to initialize workspace state in {}",
                config.state_dir.display()
            )
        })?;
    let (update_tx, mut update_rx) = mpsc::channel(1);
    let (restart_tx, mut restart_rx) = mpsc::channel(1);
    let updates = UpdateManager::new(config.state_dir.clone(), config.update_policy, update_tx)?;

    let bind_address = SocketAddr::new(config.host, config.port);
    let listener = TcpListener::bind(bind_address)
        .await
        .with_context(|| format!("failed to bind HTTP server to {bind_address}"))?;

    if !config.host.is_loopback() {
        tracing::warn!(
            address = %bind_address,
            "server is reachable beyond localhost; use only on a trusted network with HTTPS or Tailscale"
        );
    }

    match sessions.start_primary().await {
        Ok(()) => {
            let primary = sessions.primary().snapshot();
            if let Err(error) = workspaces
                .record_recent(directories.describe(&config.project_dir), primary.agent)
                .await
            {
                tracing::warn!(
                    %error,
                    "primary terminal started but Recent workspace state could not be saved"
                );
            }
        }
        Err(error) => {
            tracing::error!(
                %error,
                agent = config.primary_agent.label(),
                "primary agent is unavailable; the web server will remain available for diagnostics and restart"
            );
        }
    }

    let browser_url = print_startup_urls(&config, &token);
    tracing::info!(
        address = %bind_address,
        project = %config.project_dir.display(),
        "Codex Web Terminal server started"
    );

    let public_shutdown = CancellationToken::new();
    let peer_shutdown = CancellationToken::new();
    let state = AppState {
        config: Arc::new(config.clone()),
        auth: AuthState::new(token.clone()),
        sessions: sessions.clone(),
        peers: peers.clone(),
        agents: agent_catalog,
        directories,
        workspaces,
        updates: updates.clone(),
        restart_tx,
        server_restart_supported,
        shutdown: public_shutdown.clone(),
        readiness_nonce,
    };
    let app = build_router(state, static_directory);
    let peer_app = peer_routes::internal_router(peers.clone());
    updates.spawn_background_checks(public_shutdown.clone());

    if !config.no_open_browser
        && let Err(error) = webbrowser::open(&browser_url)
    {
        tracing::warn!(%error, "failed to open the default browser");
    }

    let public_shutdown_signal = public_shutdown.clone();
    let mut public_server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(public_shutdown_signal.cancelled_owned())
        .await
    });
    let peer_shutdown_signal = peer_shutdown.clone();
    let mut peer_server = tokio::spawn(async move {
        axum::serve(
            peer_listener,
            peer_app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(peer_shutdown_signal.cancelled_owned())
        .await
    });

    let stop_cause = wait_for_stop_cause(
        tokio::signal::ctrl_c(),
        &mut public_server,
        &mut peer_server,
        &mut update_rx,
        &mut restart_rx,
    )
    .await;

    tracing::info!("shutdown requested");
    coordinated_shutdown(
        &public_shutdown,
        &peers,
        sessions.shutdown(),
        &peer_shutdown,
    )
    .await;

    match stop_cause {
        StopCause::Signal(signal) => {
            let public_result = await_server_task(&mut public_server, "HTTP server").await;
            let peer_result = await_server_task(&mut peer_server, "private peer bridge").await;

            signal.context("failed to monitor Ctrl+C")?;
            public_result?;
            peer_result?;
            Ok(())
        }
        StopCause::Public(result) => {
            let public_result = completed_server_task(result, "HTTP server");
            if let Err(error) = await_server_task(&mut peer_server, "private peer bridge").await {
                tracing::error!(%error, "private peer bridge cleanup failed");
            }

            match public_result {
                Ok(()) => bail!("HTTP server stopped unexpectedly"),
                Err(error) => Err(error.context("HTTP server stopped unexpectedly")),
            }
        }
        StopCause::Peer(result) => {
            let peer_result = completed_server_task(result, "private peer bridge");
            if let Err(error) = await_server_task(&mut public_server, "HTTP server").await {
                tracing::error!(%error, "HTTP server cleanup failed");
            }

            match peer_result {
                Ok(()) => bail!("private peer bridge stopped unexpectedly"),
                Err(error) => Err(error.context("private peer bridge stopped unexpectedly")),
            }
        }
        StopCause::Update(activation) => {
            await_server_task(&mut public_server, "HTTP server").await?;
            await_server_task(&mut peer_server, "private peer bridge").await?;
            if supervised_worker {
                update_bootstrap::verify_pending_activation(&config.state_dir, &activation)?;
                std::process::exit(update_bootstrap::UPDATE_RESTART_EXIT_CODE);
            }
            update_bootstrap::activate_and_supervise(
                &activation,
                &config,
                &token,
                &previous_executable,
            )
            .await
        }
        StopCause::Restart => {
            await_server_task(&mut public_server, "HTTP server").await?;
            await_server_task(&mut peer_server, "private peer bridge").await?;
            if supervised_worker {
                std::process::exit(update_bootstrap::SERVER_RESTART_EXIT_CODE);
            }
            update_bootstrap::restart_and_supervise(
                &config,
                &token,
                &previous_executable,
            )
            .await
        }
        StopCause::UpdateChannelClosed => {
            if let Err(error) = await_server_task(&mut public_server, "HTTP server").await {
                tracing::error!(%error, "HTTP server cleanup failed");
            }
            if let Err(error) = await_server_task(&mut peer_server, "private peer bridge").await {
                tracing::error!(%error, "private peer bridge cleanup failed");
            }
            bail!("update control channel closed unexpectedly")
        }
        StopCause::RestartChannelClosed => {
            if let Err(error) = await_server_task(&mut public_server, "HTTP server").await {
                tracing::error!(%error, "HTTP server cleanup failed");
            }
            if let Err(error) = await_server_task(&mut peer_server, "private peer bridge").await {
                tracing::error!(%error, "private peer bridge cleanup failed");
            }
            bail!("restart control channel closed unexpectedly")
        }
    }
}

async fn wait_for_stop_cause<S>(
    signal: S,
    public_server: &mut JoinHandle<io::Result<()>>,
    peer_server: &mut JoinHandle<io::Result<()>>,
    update_rx: &mut mpsc::Receiver<UpdateActivation>,
    restart_rx: &mut mpsc::Receiver<()>,
) -> StopCause
where
    S: Future<Output = io::Result<()>>,
{
    tokio::pin!(signal);
    tokio::select! {
        result = &mut signal => StopCause::Signal(result),
        result = public_server => StopCause::Public(result),
        result = peer_server => StopCause::Peer(result),
        activation = update_rx.recv() => match activation {
            Some(activation) => StopCause::Update(activation),
            None => StopCause::UpdateChannelClosed,
        },
        restart = restart_rx.recv() => match restart {
            Some(()) => StopCause::Restart,
            None => StopCause::RestartChannelClosed,
        },
    }
}

async fn coordinated_shutdown<F>(
    public_shutdown: &CancellationToken,
    peers: &PeerBroker,
    session_shutdown: F,
    peer_shutdown: &CancellationToken,
) where
    F: Future<Output = ()>,
{
    // Stop accepting public work first. The private capability bridge remains
    // available until every managed PTY has terminated and revoked its active
    // generation.
    public_shutdown.cancel();
    peers.begin_shutdown();
    session_shutdown.await;
    // A launch that was already inside its blocking startup section may have
    // activated a generation while shutdown waited for that section. Sweep
    // once more so termination failures cannot leave a usable capability.
    peers.begin_shutdown();
    peer_shutdown.cancel();
}

async fn await_server_task(
    task: &mut JoinHandle<io::Result<()>>,
    label: &'static str,
) -> Result<()> {
    let joined = match timeout(SERVER_SHUTDOWN_TIMEOUT, &mut *task).await {
        Ok(joined) => joined,
        Err(_) => {
            task.abort();
            let _ = task.await;
            bail!(
                "{label} did not stop within {} seconds",
                SERVER_SHUTDOWN_TIMEOUT.as_secs()
            );
        }
    };
    completed_server_task(joined, label)
}

fn completed_server_task(result: ServerTaskResult, label: &'static str) -> Result<()> {
    result
        .with_context(|| format!("{label} task failed"))?
        .with_context(|| format!("{label} failed"))
}

fn print_startup_urls(config: &Config, token: &str) -> String {
    let browser_host = if config.host.is_unspecified() {
        match config.host {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        }
    } else {
        config.host
    };
    let browser_url = authenticated_url(browser_host, config.port, token);

    println!();
    println!("Codex Web Terminal started");
    println!();

    if browser_host.is_loopback() {
        println!("Local URL:");
    } else {
        println!("Network URL:");
    }
    println!("{browser_url}");

    if config.host.is_unspecified()
        && let Some(network_host) = discover_network_address(config.host)
    {
        println!();
        println!("Network URL:");
        println!("{}", authenticated_url(network_host, config.port, token));
    }

    if !config.host.is_loopback() {
        println!();
        println!("Warning: Use only on a trusted network or through Tailscale.");
    }
    println!();

    browser_url
}

fn authenticated_url(host: IpAddr, port: u16, token: &str) -> String {
    let mut url = Url::parse(&format!("http://{}/", SocketAddr::new(host, port)))
        .expect("socket addresses form valid URLs");
    url.query_pairs_mut().append_pair("token", token);
    url.into()
}

fn discover_network_address(bind_host: IpAddr) -> Option<IpAddr> {
    match bind_host {
        IpAddr::V4(_) => {
            let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
            socket.connect((Ipv4Addr::new(1, 1, 1, 1), 80)).ok()?;
            Some(socket.local_addr().ok()?.ip())
        }
        IpAddr::V6(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use codex_web_terminal::peer::{CWT_PEER_CAPABILITY_ENV, SessionPurpose};
    use tokio::sync::Notify;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn an_unexpected_private_bridge_exit_is_detected() {
        let mut public_server = tokio::spawn(std::future::pending::<io::Result<()>>());
        let mut peer_server = tokio::spawn(async { Ok(()) });
        let (_update_tx, mut update_rx) = mpsc::channel(1);
        let (_restart_tx, mut restart_rx) = mpsc::channel(1);

        let cause = timeout(
            Duration::from_secs(1),
            wait_for_stop_cause(
                std::future::pending(),
                &mut public_server,
                &mut peer_server,
                &mut update_rx,
                &mut restart_rx,
            ),
        )
        .await
        .expect("private bridge exit detected");

        match cause {
            StopCause::Peer(result) => {
                completed_server_task(result, "private peer bridge")
                    .expect("private bridge task result");
            }
            _ => panic!("private bridge exit was not selected"),
        }

        public_server.abort();
        let _ = public_server.await;
    }

    #[tokio::test]
    async fn an_authenticated_restart_signal_selects_controlled_restart() {
        let mut public_server = tokio::spawn(std::future::pending::<io::Result<()>>());
        let mut peer_server = tokio::spawn(std::future::pending::<io::Result<()>>());
        let (_update_tx, mut update_rx) = mpsc::channel(1);
        let (restart_tx, mut restart_rx) = mpsc::channel(1);
        restart_tx.send(()).await.expect("queue restart");

        let cause = timeout(
            Duration::from_secs(1),
            wait_for_stop_cause(
                std::future::pending(),
                &mut public_server,
                &mut peer_server,
                &mut update_rx,
                &mut restart_rx,
            ),
        )
        .await
        .expect("restart selected");

        assert!(matches!(cause, StopCause::Restart));
        public_server.abort();
        peer_server.abort();
        let _ = public_server.await;
        let _ = peer_server.await;
    }

    #[tokio::test]
    async fn coordinated_shutdown_revokes_before_releasing_private_listener() {
        let public_shutdown = CancellationToken::new();
        let peer_shutdown = CancellationToken::new();
        let broker = PeerBroker::new(
            "127.0.0.1:43123".parse().expect("loopback endpoint"),
            PathBuf::from("peer-helper"),
        );
        let activation = broker
            .activate_session(Uuid::new_v4(), Uuid::new_v4(), &SessionPurpose::Interactive)
            .expect("activate capability");
        let capability = activation
            .environment()
            .iter()
            .find_map(|(name, value)| {
                (name.to_str() == Some(CWT_PEER_CAPABILITY_ENV))
                    .then(|| value.to_string_lossy().into_owned())
            })
            .expect("capability environment");
        let shutdown_started = Arc::new(Notify::new());
        let allow_shutdown = Arc::new(Notify::new());

        let public_for_task = public_shutdown.clone();
        let public_for_session = public_shutdown.clone();
        let peer_for_task = peer_shutdown.clone();
        let peer_for_session = peer_shutdown.clone();
        let broker_for_task = broker.clone();
        let broker_for_session = broker.clone();
        let capability_for_session = capability.clone();
        let started_for_session = shutdown_started.clone();
        let allow_for_session = allow_shutdown.clone();
        let task = tokio::spawn(async move {
            coordinated_shutdown(
                &public_for_task,
                &broker_for_task,
                async move {
                    assert!(public_for_session.is_cancelled());
                    assert!(!peer_for_session.is_cancelled());
                    assert!(
                        broker_for_session
                            .authenticate_capability(&capability_for_session)
                            .is_err()
                    );
                    assert!(
                        broker_for_session
                            .activate_session(
                                Uuid::new_v4(),
                                Uuid::new_v4(),
                                &SessionPurpose::Interactive,
                            )
                            .is_err()
                    );
                    started_for_session.notify_one();
                    allow_for_session.notified().await;
                },
                &peer_for_task,
            )
            .await;
        });

        timeout(Duration::from_secs(1), shutdown_started.notified())
            .await
            .expect("session shutdown started");
        assert!(public_shutdown.is_cancelled());
        assert!(!peer_shutdown.is_cancelled());
        assert!(broker.authenticate_capability(&capability).is_err());

        allow_shutdown.notify_one();
        task.await.expect("coordinated shutdown task");
        assert!(peer_shutdown.is_cancelled());
    }
}
