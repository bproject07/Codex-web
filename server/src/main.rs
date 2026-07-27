use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    sync::Arc,
};

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use url::Url;

use codex_web_terminal::{
    agents::build_agent_profiles,
    auth::{AuthState, generate_token},
    config::{Config, static_directory},
    registry::SessionRegistry,
    routes::{AppState, build_router},
};

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::load()?;
    let token = config.token.take().unwrap_or(generate_token()?);

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

    let agent_profiles = build_agent_profiles(&config);
    let agent_catalog = agent_profiles.catalog.clone();
    let sessions = SessionRegistry::with_agent_configs(
        agent_profiles.primary,
        agent_profiles.new_session,
        agent_profiles.additional,
    );

    if let Err(error) = sessions.start_primary().await {
        tracing::error!(
            %error,
            agent = config.primary_agent.label(),
            "primary agent is unavailable; the web server will remain available for diagnostics and restart"
        );
    }

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

    let browser_url = print_startup_urls(&config, &token);
    tracing::info!(
        address = %bind_address,
        project = %config.project_dir.display(),
        "Codex Web Terminal server started"
    );

    let shutdown = CancellationToken::new();
    let state = AppState {
        config: Arc::new(config.clone()),
        auth: AuthState::new(token),
        sessions: sessions.clone(),
        agents: agent_catalog,
        shutdown: shutdown.clone(),
    };
    let app = build_router(state, static_directory);

    if !config.no_open_browser
        && let Err(error) = webbrowser::open(&browser_url)
    {
        tracing::warn!(%error, "failed to open the default browser");
    }

    let shutdown_sessions = sessions.clone();
    let shutdown_signal = async move {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl+C handler");
        }
        tracing::info!("shutdown requested");
        shutdown.cancel();
        shutdown_sessions.shutdown().await;
    };

    let server_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal)
    .await;

    sessions.shutdown().await;
    server_result.context("HTTP server failed")
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
