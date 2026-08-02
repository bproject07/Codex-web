use std::{
    env,
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use reqwest::{Client, Url, redirect::Policy};
use semver::Version;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::Config,
    process_tree::ManagedProcess,
    update_fs::{
        UpdateFileLock, ensure_private_directory, is_link_or_reparse, safe_remove_tree,
        validate_regular_directory, validate_regular_file,
    },
    update_manifest::{current_release_target, executable_name, validate_package_layout},
    updater::{UpdateActivation, validate_release_executable},
};

pub const SUPERVISED_WORKER_ENV: &str = "CWT_INTERNAL_SUPERVISED_WORKER";
pub const READINESS_NONCE_ENV: &str = "CWT_INTERNAL_READINESS_NONCE";
pub const SERVER_RESTART_CAPABILITY_ENV: &str = "CWT_INTERNAL_SERVER_RESTART";
pub const UPDATE_RESTART_EXIT_CODE: i32 = 75;
pub const SERVER_RESTART_EXIT_CODE: i32 = 76;

const ACTIVE_POINTER_SCHEMA_VERSION: u32 = 2;
const PENDING_POINTER_SCHEMA_VERSION: u32 = 1;
const ACTIVE_POINTER_NAME: &str = "active.json";
const PENDING_POINTER_NAME: &str = "pending.json";
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const READINESS_INTERVAL: Duration = Duration::from_millis(250);
const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const GRACEFUL_CHILD_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_POINTER_BYTES: u64 = 16 * 1024;
const MAX_HEALTH_RESPONSE_BYTES: usize = 16 * 1024;
const REQUIRED_CONSECUTIVE_READINESS_PROBES: u8 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActiveRelease {
    schema_version: u32,
    version: String,
    previous_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingActivation {
    schema_version: u32,
    request_id: String,
    source_version: String,
    target_version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HealthProbe {
    status: String,
    server_version: String,
    readiness_nonce: Option<String>,
}

#[derive(Clone)]
struct GenerationSpec {
    version: Version,
    executable: PathBuf,
}

struct ManagedGeneration {
    spec: GenerationSpec,
    process: ManagedProcess,
}

enum Readiness {
    Ready,
    Interrupted,
}

enum GenerationExit {
    Status(ExitStatus),
    SupervisorInterrupted,
}

#[derive(Debug, Clone)]
pub struct WorkerContext {
    pub supervised: bool,
    pub readiness_nonce: Option<String>,
    pub server_restart_supported: bool,
}

pub fn take_worker_context() -> Result<WorkerContext> {
    let marker = env::var_os(SUPERVISED_WORKER_ENV);
    let readiness_nonce = env::var(READINESS_NONCE_ENV).ok();
    let server_restart_capability = env::var_os(SERVER_RESTART_CAPABILITY_ENV);
    // SAFETY: this function is called by the synchronous outer main before a
    // Tokio runtime or any application threads are created.
    unsafe {
        env::remove_var(SUPERVISED_WORKER_ENV);
        env::remove_var(READINESS_NONCE_ENV);
        env::remove_var(SERVER_RESTART_CAPABILITY_ENV);
    }

    let supervised = match marker {
        None => false,
        Some(value) if value == "1" => true,
        Some(_) => bail!("the internal supervised-worker marker is invalid"),
    };
    match (supervised, readiness_nonce.as_deref()) {
        (false, None) => {}
        (false, Some(_)) => bail!("an internal readiness nonce requires a supervised worker"),
        (true, Some(value)) => {
            Uuid::parse_str(value).context("the internal readiness nonce is invalid")?;
        }
        (true, None) => bail!("a supervised worker is missing its readiness nonce"),
    }
    let server_restart_supported = match (supervised, server_restart_capability) {
        (false, None) => true,
        (false, Some(_)) => {
            bail!("the internal server-restart capability requires a supervised worker")
        }
        (true, None) => false,
        (true, Some(value)) if value == "1" => true,
        (true, Some(_)) => bail!("the internal server-restart capability is invalid"),
    };

    Ok(WorkerContext {
        supervised,
        readiness_nonce,
        server_restart_supported,
    })
}

pub async fn supervise_startup(
    config: &Config,
    token: &str,
    root_executable: &Path,
) -> Result<bool> {
    let root_version =
        Version::parse(env!("CARGO_PKG_VERSION")).context("the launcher version is invalid")?;
    let root = GenerationSpec {
        version: root_version.clone(),
        executable: root_executable.to_path_buf(),
    };
    let active = read_active_release(&config.state_dir)?;
    let mut recovering_active = false;
    let mut retained_fallback_version = None;
    let previous = match active.as_ref() {
        Some(active) => {
            let version =
                Version::parse(&active.version).context("active update version is invalid")?;
            if version > root_version {
                let fallback_version = Version::parse(&active.previous_version)
                    .context("active update previous version is invalid")?;
                retained_fallback_version = Some(fallback_version.clone());
                match release_generation(&config.state_dir, &version) {
                    Ok(generation) => generation,
                    Err(active_error) => {
                        let fallback =
                            generation_for_version(&config.state_dir, &root, &fallback_version)
                                .with_context(|| {
                                    format!(
                                        "active release {version} is invalid ({active_error:#}) and its retained fallback is unavailable"
                                    )
                                })?;
                        eprintln!(
                            "Warning: active release {version} is invalid ({active_error:#}); recovering release {}.",
                            fallback.version
                        );
                        recovering_active = true;
                        fallback
                    }
                }
            } else {
                root.clone()
            }
        }
        None => root.clone(),
    };

    let pending = match read_pending_activation(&config.state_dir) {
        Ok(pending) => pending,
        Err(error) => {
            eprintln!("Warning: invalid pending update was quarantined ({error}).");
            if let Err(quarantine_error) = quarantine_pending_activation(&config.state_dir) {
                eprintln!(
                    "Warning: invalid pending update could not be quarantined ({quarantine_error})."
                );
            }
            None
        }
    };

    if let Some(pending) = pending {
        let target = pending_target(&pending)?;
        if target == previous.version {
            if let Err(error) = remove_pending_activation(&config.state_dir, &pending.request_id) {
                eprintln!("Warning: a committed update marker could not be removed ({error:#}).");
            }
        } else if pending_source(&pending)? == previous.version && target > previous.version {
            let updates = ensure_updates_root(&config.state_dir)?;
            let _lock = UpdateFileLock::acquire(&updates)?;
            let Some(generation) = activate_or_rollback(
                config,
                token,
                previous.clone(),
                pending,
                !config.no_open_browser,
            )
            .await?
            else {
                return Ok(true);
            };
            if recovering_active && generation.spec.version == previous.version {
                commit_recovered_active_release(
                    &config.state_dir,
                    active.as_ref(),
                    &generation.spec.version,
                    &root_version,
                )?;
            }
            drop(_lock);
            supervise_generation(config, token, generation).await?;
            return Ok(true);
        } else {
            eprintln!(
                "Warning: stale pending update does not match the active generation and was quarantined."
            );
            quarantine_pending_activation(&config.state_dir)?;
        }
    }

    if previous.version > root_version || recovering_active {
        let mut selected = previous.clone();
        let launched = match launch_generation(
            config,
            token,
            selected.clone(),
            !config.no_open_browser,
        )
        .await
        {
            Ok(launched) => launched,
            Err(active_error) if !recovering_active => {
                let fallback_version = retained_fallback_version
                    .as_ref()
                    .context("the active release failed readiness without a retained fallback")?;
                selected = generation_for_version(&config.state_dir, &root, fallback_version)
                        .with_context(|| {
                            format!(
                                "active release {} failed readiness ({active_error:#}) and its retained fallback is unavailable",
                                previous.version
                            )
                        })?;
                recovering_active = true;
                let launched = launch_generation(
                        config,
                        token,
                        selected.clone(),
                        !config.no_open_browser,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "active release {} failed readiness ({active_error:#}) and retained fallback {} also failed",
                            previous.version, selected.version
                        )
                    })?;
                if launched.is_some() {
                    eprintln!(
                        "Warning: active release {} failed readiness ({active_error:#}); recovering release {}.",
                        previous.version, selected.version
                    );
                }
                launched
            }
            Err(error) => return Err(error),
        };
        let Some(generation) = launched else {
            return Ok(true);
        };
        if recovering_active {
            commit_recovered_active_release(
                &config.state_dir,
                active.as_ref(),
                &generation.spec.version,
                &root_version,
            )?;
        }
        supervise_generation(config, token, generation).await?;
        return Ok(true);
    }

    Ok(false)
}

fn commit_recovered_active_release(
    state_dir: &Path,
    expected_active: Option<&ActiveRelease>,
    recovered_version: &Version,
    root_version: &Version,
) -> Result<()> {
    if recovered_version > root_version {
        write_active_release(state_dir, recovered_version, root_version)
            .context("failed to commit recovered active release")?;
    } else {
        remove_active_release(state_dir, expected_active)
            .context("failed to remove the unusable active release pointer")?;
    }
    if let Err(error) = cleanup_old_releases(state_dir, recovered_version, root_version) {
        eprintln!("Warning: invalid active update packages could not be cleaned up ({error:#}).");
    }
    Ok(())
}

pub fn persist_pending_activation(state_dir: &Path, activation: &UpdateActivation) -> Result<()> {
    validate_activation(state_dir, activation)?;
    let pending = PendingActivation {
        schema_version: PENDING_POINTER_SCHEMA_VERSION,
        request_id: activation.request_id.to_string(),
        source_version: activation.source_version.to_string(),
        target_version: activation.version.to_string(),
    };
    if let Some(existing) = read_pending_activation(state_dir)? {
        if existing.request_id == pending.request_id
            && existing.source_version == pending.source_version
            && existing.target_version == pending.target_version
        {
            return Ok(());
        }
        bail!("a different update activation is already pending");
    }
    let durable = write_json_pointer(state_dir, PENDING_POINTER_NAME, ".pending", &pending)?;
    if !durable {
        eprintln!("Warning: pending update was committed but its directory could not be synced.");
    }
    Ok(())
}

pub fn verify_pending_activation(state_dir: &Path, activation: &UpdateActivation) -> Result<()> {
    validate_activation(state_dir, activation)?;
    let pending =
        read_pending_activation(state_dir)?.context("the pending update activation is missing")?;
    if pending.request_id != activation.request_id.to_string()
        || pending.source_version != activation.source_version.to_string()
        || pending.target_version != activation.version.to_string()
    {
        bail!("the pending update activation does not match the prepared release");
    }
    Ok(())
}

pub fn remove_matching_pending_activation(state_dir: &Path, request_id: &Uuid) -> Result<()> {
    remove_pending_activation(state_dir, &request_id.to_string())
}

pub async fn activate_and_supervise(
    activation: &UpdateActivation,
    config: &Config,
    token: &str,
    previous_executable: &Path,
) -> Result<()> {
    verify_pending_activation(&config.state_dir, activation)?;
    let pending = read_pending_activation(&config.state_dir)?
        .context("the pending update disappeared before activation")?;
    if pending.request_id != activation.request_id.to_string() {
        bail!("the pending update request changed before activation");
    }

    let updates = ensure_updates_root(&config.state_dir)?;
    let _lock = UpdateFileLock::acquire(&updates)?;
    let previous = GenerationSpec {
        version: activation.source_version.clone(),
        executable: previous_executable.to_path_buf(),
    };
    let Some(generation) = activate_or_rollback(config, token, previous, pending, false).await?
    else {
        return Ok(());
    };
    drop(_lock);
    supervise_generation(config, token, generation).await
}

pub async fn restart_and_supervise(
    config: &Config,
    token: &str,
    current_executable: &Path,
) -> Result<()> {
    let version =
        Version::parse(env!("CARGO_PKG_VERSION")).context("the running version is invalid")?;
    let spec = GenerationSpec {
        version,
        executable: current_executable.to_path_buf(),
    };
    let Some(generation) = launch_generation(config, token, spec, false).await? else {
        return Ok(());
    };
    supervise_generation(config, token, generation).await
}

async fn supervise_generation(
    config: &Config,
    token: &str,
    mut generation: ManagedGeneration,
) -> Result<()> {
    loop {
        match wait_for_generation_exit(&mut generation.process).await? {
            GenerationExit::SupervisorInterrupted => return Ok(()),
            GenerationExit::Status(status) if status.code() == Some(UPDATE_RESTART_EXIT_CODE) => {
                generation.process.terminate_descendants();
                let pending = read_pending_activation(&config.state_dir)?.context(
                    "a supervised worker requested update restart without a pending activation",
                )?;
                if pending_source(&pending)? != generation.spec.version {
                    bail!("the pending update source does not match the supervised worker");
                }
                let updates = ensure_updates_root(&config.state_dir)?;
                let _lock = UpdateFileLock::acquire(&updates)?;
                let previous = generation.spec.clone();
                let Some(next) =
                    activate_or_rollback(config, token, previous, pending, false).await?
                else {
                    return Ok(());
                };
                generation = next;
            }
            GenerationExit::Status(status) if status.code() == Some(SERVER_RESTART_EXIT_CODE) => {
                generation.process.terminate_descendants();
                let current = generation.spec.clone();
                let Some(next) = launch_generation(config, token, current, false).await? else {
                    return Ok(());
                };
                generation = next;
            }
            GenerationExit::Status(status) if status.success() => {
                generation.process.terminate_descendants();
                return Ok(());
            }
            GenerationExit::Status(status) => {
                generation.process.terminate_descendants();
                bail!(
                    "supervised Codex Web generation {} exited with {}",
                    generation.spec.version,
                    status
                );
            }
        }
    }
}

async fn activate_or_rollback(
    config: &Config,
    token: &str,
    previous: GenerationSpec,
    pending: PendingActivation,
    allow_browser: bool,
) -> Result<Option<ManagedGeneration>> {
    let source = pending_source(&pending)?;
    let target = pending_target(&pending)?;
    if source != previous.version || target <= source {
        bail!("the pending update does not describe a forward transition");
    }
    let candidate = match release_generation(&config.state_dir, &target) {
        Ok(candidate) => candidate,
        Err(candidate_error) => {
            if let Err(error) = remove_pending_activation(&config.state_dir, &pending.request_id) {
                eprintln!("Warning: failed update marker could not be removed ({error:#}).");
            }
            let rollback = launch_rollback(config, token, previous, allow_browser)
                .await
                .with_context(|| {
                    format!(
                        "candidate package validation failed ({candidate_error:#}) and rollback also failed"
                    )
                })?;
            eprintln!(
                "Update candidate package validation failed ({candidate_error:#}); the previous release is running."
            );
            return Ok(Some(rollback));
        }
    };

    match launch_generation(config, token, candidate, allow_browser).await {
        Ok(Some(mut next)) => {
            let active_durable = match write_active_release(
                &config.state_dir,
                &target,
                &previous.version,
            ) {
                Ok(durable) => durable,
                Err(error) => {
                    next.process.terminate_and_wait();
                    if let Err(remove_error) =
                        remove_pending_activation(&config.state_dir, &pending.request_id)
                    {
                        eprintln!(
                            "Warning: failed update marker could not be removed ({remove_error:#})."
                        );
                    }
                    let rollback = launch_rollback(config, token, previous, allow_browser).await?;
                    eprintln!(
                        "Update activation failed before commit ({error:#}); the previous release is running."
                    );
                    return Ok(Some(rollback));
                }
            };
            if active_durable {
                if let Err(error) =
                    remove_pending_activation(&config.state_dir, &pending.request_id)
                {
                    eprintln!(
                        "Warning: committed update pending marker could not be removed ({error:#})."
                    );
                }
            } else {
                eprintln!(
                    "Warning: active update pointer was committed without a directory sync; the pending recovery marker was retained."
                );
            }
            if let Err(error) = cleanup_old_releases(&config.state_dir, &target, &previous.version)
            {
                eprintln!("Warning: old update packages could not be cleaned up ({error:#}).");
            }
            Ok(Some(next))
        }
        Ok(None) => Ok(None),
        Err(candidate_error) => {
            if let Err(error) = remove_pending_activation(&config.state_dir, &pending.request_id) {
                eprintln!("Warning: failed update marker could not be removed ({error:#}).");
            }
            let rollback = launch_rollback(config, token, previous, allow_browser)
                .await
                .with_context(|| {
                    format!(
                        "candidate failed readiness ({candidate_error:#}) and rollback also failed"
                    )
                })?;
            eprintln!(
                "Update candidate failed readiness ({candidate_error:#}); the previous release is running."
            );
            Ok(Some(rollback))
        }
    }
}

async fn launch_rollback(
    config: &Config,
    token: &str,
    previous: GenerationSpec,
    allow_browser: bool,
) -> Result<ManagedGeneration> {
    launch_generation(config, token, previous, allow_browser)
        .await?
        .context("rollback was interrupted before readiness")
}

async fn launch_generation(
    config: &Config,
    token: &str,
    spec: GenerationSpec,
    allow_browser: bool,
) -> Result<Option<ManagedGeneration>> {
    let readiness_nonce = Uuid::new_v4().to_string();
    let mut process = start_server(
        &spec.executable,
        config,
        token,
        allow_browser,
        &readiness_nonce,
    )
    .with_context(|| format!("failed to start Codex Web {}", spec.version))?;
    match wait_until_ready(&mut process, config, token, &spec.version, &readiness_nonce).await {
        Ok(Readiness::Ready) => Ok(Some(ManagedGeneration { spec, process })),
        Ok(Readiness::Interrupted) => {
            stop_generation_gracefully(&mut process).await;
            Ok(None)
        }
        Err(error) => {
            process.terminate_and_wait();
            Err(error)
        }
    }
}

fn start_server(
    executable: &Path,
    config: &Config,
    token: &str,
    allow_browser: bool,
    readiness_nonce: &str,
) -> Result<ManagedProcess> {
    let mut arguments = config.restart_arguments();
    if allow_browser && !config.no_open_browser {
        arguments.retain(|argument| argument != OsStr::new("--no-open-browser"));
    }

    let configured_environment = env::vars_os().filter(|(name, _)| {
        !is_internal_environment(name) && !name.eq_ignore_ascii_case(OsStr::new("CODEX_WEB_TOKEN"))
    });
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .env_clear()
        .envs(configured_environment)
        .env("CODEX_WEB_TOKEN", token)
        .env(SUPERVISED_WORKER_ENV, "1")
        .env(READINESS_NONCE_ENV, readiness_nonce)
        .env(SERVER_RESTART_CAPABILITY_ENV, "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    ManagedProcess::spawn(&mut command)
        .with_context(|| format!("failed to launch {}", executable.display()))
}

fn is_internal_environment(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.eq_ignore_ascii_case(SUPERVISED_WORKER_ENV)
        || name.eq_ignore_ascii_case(READINESS_NONCE_ENV)
        || name.eq_ignore_ascii_case(SERVER_RESTART_CAPABILITY_ENV)
        || name.eq_ignore_ascii_case("CODEX_THREAD_ID")
        || name.eq_ignore_ascii_case("CLAUDECODE")
        || name
            .get(.."CWT_PEER_".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("CWT_PEER_"))
}

async fn wait_until_ready(
    process: &mut ManagedProcess,
    config: &Config,
    token: &str,
    expected_version: &Version,
    expected_nonce: &str,
) -> Result<Readiness> {
    let url = readiness_url(config)?;
    let client = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .context("failed to initialize the local readiness client")?;
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let mut consecutive_ready = 0_u8;

    loop {
        if let Some(status) = process
            .try_wait()
            .context("failed to inspect the staged server process")?
        {
            bail!("staged server exited before readiness with {status}");
        }

        let request = client.get(url.clone()).bearer_auth(token).send();
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to monitor Ctrl+C during update handoff")?;
                process.interrupt();
                return Ok(Readiness::Interrupted);
            }
            response = request => {
                let ready = match response {
                    Ok(response) if response.status().is_success() => {
                        read_health_probe(response)
                            .await
                            .is_some_and(|health| {
                                health_is_ready(&health, expected_version, expected_nonce)
                            })
                    }
                    _ => false,
                };
                if ready {
                    consecutive_ready = consecutive_ready.saturating_add(1);
                    if consecutive_ready >= REQUIRED_CONSECUTIVE_READINESS_PROBES {
                        return Ok(Readiness::Ready);
                    }
                } else {
                    consecutive_ready = 0;
                }
            }
        }

        if Instant::now() >= deadline {
            bail!(
                "staged server did not report version {} within {} seconds",
                expected_version,
                READINESS_TIMEOUT.as_secs()
            );
        }
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to monitor Ctrl+C during update handoff")?;
                process.interrupt();
                return Ok(Readiness::Interrupted);
            }
            _ = tokio::time::sleep(READINESS_INTERVAL) => {}
        }
    }
}

fn health_is_ready(health: &HealthProbe, expected_version: &Version, expected_nonce: &str) -> bool {
    health.status == "ok"
        && health.server_version == expected_version.to_string()
        && health.readiness_nonce.as_deref() == Some(expected_nonce)
}

async fn read_health_probe(response: reqwest::Response) -> Option<HealthProbe> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HEALTH_RESPONSE_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt as _;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if body.len().saturating_add(chunk.len()) > MAX_HEALTH_RESPONSE_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).ok()
}

async fn stop_generation_gracefully(process: &mut ManagedProcess) {
    process.interrupt();
    let deadline = Instant::now() + GRACEFUL_CHILD_TIMEOUT;
    while Instant::now() < deadline {
        if process.try_wait().ok().flatten().is_some() {
            process.terminate_descendants();
            return;
        }
        tokio::time::sleep(CHILD_EXIT_POLL_INTERVAL).await;
    }
    process.terminate_and_wait();
}

async fn wait_for_generation_exit(process: &mut ManagedProcess) -> Result<GenerationExit> {
    loop {
        if let Some(status) = process
            .try_wait()
            .context("failed to inspect the supervised server process")?
        {
            return Ok(GenerationExit::Status(status));
        }
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to monitor Ctrl+C while supervising the server")?;
                process.interrupt();
                let deadline = Instant::now() + GRACEFUL_CHILD_TIMEOUT;
                while Instant::now() < deadline {
                    if process.try_wait()
                        .context("failed to wait for the supervised server")?
                        .is_some()
                    {
                        return Ok(GenerationExit::SupervisorInterrupted);
                    }
                    tokio::time::sleep(CHILD_EXIT_POLL_INTERVAL).await;
                }
                process.terminate_and_wait();
                return Ok(GenerationExit::SupervisorInterrupted);
            }
            _ = tokio::time::sleep(CHILD_EXIT_POLL_INTERVAL) => {}
        }
    }
}

fn readiness_url(config: &Config) -> Result<Url> {
    let host = if config.host.is_unspecified() {
        match config.host {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        }
    } else {
        config.host
    };
    Url::parse(&format!(
        "http://{}/api/health",
        SocketAddr::new(host, config.port)
    ))
    .context("failed to construct the local readiness URL")
}

fn validate_activation(state_dir: &Path, activation: &UpdateActivation) -> Result<()> {
    if activation.version <= activation.source_version
        || !activation.version.pre.is_empty()
        || !activation.version.build.is_empty()
        || !activation.source_version.pre.is_empty()
        || !activation.source_version.build.is_empty()
    {
        bail!("update activation must describe a stable forward transition");
    }
    let expected_root = release_root(state_dir, &activation.version);
    let expected = dunce::canonicalize(&expected_root)
        .with_context(|| format!("failed to resolve {}", expected_root.display()))?;
    let actual = dunce::canonicalize(&activation.package_root)
        .with_context(|| format!("failed to resolve {}", activation.package_root.display()))?;
    if actual != expected {
        bail!("update activation package does not match its deterministic release directory");
    }
    validate_package_layout(&actual, &activation.version, current_release_target()?)?;
    validate_release_executable(&actual, &activation.version)?;
    Ok(())
}

fn release_generation(state_dir: &Path, version: &Version) -> Result<GenerationSpec> {
    let package_root = release_root(state_dir, version);
    validate_package_layout(&package_root, version, current_release_target()?)?;
    validate_release_executable(&package_root, version)?;
    Ok(GenerationSpec {
        version: version.clone(),
        executable: package_root.join(executable_name()),
    })
}

fn generation_for_version(
    state_dir: &Path,
    root: &GenerationSpec,
    version: &Version,
) -> Result<GenerationSpec> {
    if version <= &root.version {
        Ok(root.clone())
    } else {
        release_generation(state_dir, version)
    }
}

fn updates_root(state_dir: &Path) -> PathBuf {
    state_dir.join("updates")
}

fn ensure_updates_root(state_dir: &Path) -> Result<PathBuf> {
    if !state_dir.exists() {
        fs::create_dir_all(state_dir)
            .with_context(|| format!("failed to create {}", state_dir.display()))?;
    }
    validate_regular_directory(state_dir)?;
    let root = updates_root(state_dir);
    ensure_private_directory(&root)?;
    Ok(root)
}

fn release_root(state_dir: &Path, version: &Version) -> PathBuf {
    updates_root(state_dir)
        .join("releases")
        .join(format!("v{version}"))
}

fn active_pointer_path(state_dir: &Path) -> PathBuf {
    updates_root(state_dir).join(ACTIVE_POINTER_NAME)
}

fn pending_pointer_path(state_dir: &Path) -> PathBuf {
    updates_root(state_dir).join(PENDING_POINTER_NAME)
}

fn read_active_release(state_dir: &Path) -> Result<Option<ActiveRelease>> {
    let Some(bytes) = read_pointer(state_dir, ACTIVE_POINTER_NAME)? else {
        return Ok(None);
    };
    let active: ActiveRelease =
        serde_json::from_slice(&bytes).context("active update pointer is invalid")?;
    if active.schema_version != ACTIVE_POINTER_SCHEMA_VERSION {
        bail!(
            "unsupported active update pointer schema {}",
            active.schema_version
        );
    }
    let current = stable_version(&active.version, "active update pointer")?;
    let previous = stable_version(
        &active.previous_version,
        "active update previous-version pointer",
    )?;
    if previous >= current {
        bail!("active update pointer must identify an older previous version");
    }
    Ok(Some(active))
}

fn read_pending_activation(state_dir: &Path) -> Result<Option<PendingActivation>> {
    let Some(bytes) = read_pointer(state_dir, PENDING_POINTER_NAME)? else {
        return Ok(None);
    };
    let pending: PendingActivation =
        serde_json::from_slice(&bytes).context("pending update pointer is invalid")?;
    if pending.schema_version != PENDING_POINTER_SCHEMA_VERSION {
        bail!(
            "unsupported pending update pointer schema {}",
            pending.schema_version
        );
    }
    Uuid::parse_str(&pending.request_id).context("pending update request ID is invalid")?;
    let source = pending_source(&pending)?;
    let target = pending_target(&pending)?;
    if target <= source {
        bail!("pending update must identify a newer target version");
    }
    Ok(Some(pending))
}

fn existing_updates_root(state_dir: &Path) -> Result<Option<PathBuf>> {
    validate_regular_directory(state_dir)?;
    let root = updates_root(state_dir);
    match fs::symlink_metadata(&root) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_link_or_reparse(&metadata) {
                bail!("update path is not a regular directory: {}", root.display());
            }
            Ok(Some(root))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", root.display())),
    }
}

fn read_pointer(state_dir: &Path, pointer_name: &str) -> Result<Option<Vec<u8>>> {
    let Some(root) = existing_updates_root(state_dir)? else {
        return Ok(None);
    };
    let path = root.join(pointer_name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file()
                || is_link_or_reparse(&metadata)
                || metadata.len() > MAX_POINTER_BYTES
            {
                bail!(
                    "update pointer is not a safe regular file: {}",
                    path.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }
    fs::read(&path)
        .with_context(|| format!("failed to read {}", path.display()))
        .map(Some)
}

fn pending_source(pending: &PendingActivation) -> Result<Version> {
    stable_version(&pending.source_version, "pending update source")
}

fn pending_target(pending: &PendingActivation) -> Result<Version> {
    stable_version(&pending.target_version, "pending update target")
}

fn stable_version(value: &str, label: &str) -> Result<Version> {
    let version = Version::parse(value).with_context(|| format!("{label} is invalid"))?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        bail!("{label} must identify a stable version");
    }
    Ok(version)
}

fn write_active_release(
    state_dir: &Path,
    version: &Version,
    previous_version: &Version,
) -> Result<bool> {
    if previous_version >= version {
        bail!("active update pointer requires an older previous version");
    }
    let pointer = ActiveRelease {
        schema_version: ACTIVE_POINTER_SCHEMA_VERSION,
        version: version.to_string(),
        previous_version: previous_version.to_string(),
    };
    write_json_pointer(state_dir, ACTIVE_POINTER_NAME, ".active", &pointer)
}

fn write_json_pointer(
    state_dir: &Path,
    destination_name: &str,
    temporary_prefix: &str,
    value: &impl Serialize,
) -> Result<bool> {
    let root = ensure_updates_root(state_dir)?;
    let destination = root.join(destination_name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            validate_regular_file(&destination, Some(MAX_POINTER_BYTES))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", destination.display()));
        }
    }
    let bytes = serde_json::to_vec_pretty(value).context("failed to encode update pointer")?;
    if bytes.len() as u64 + 1 > MAX_POINTER_BYTES {
        bail!("update pointer is too large");
    }
    let temporary = root.join(format!("{temporary_prefix}-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        file.write_all(b"\n")
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, &destination)
            .context("failed to atomically replace update pointer")?;
        let durable = match sync_directory(&root) {
            Ok(()) => true,
            Err(error) => {
                // The rename is the commit point. Rolling back after this
                // warning would make process state disagree with the pointer.
                eprintln!(
                    "Warning: update pointer was committed but its directory could not be synced ({error:#})."
                );
                false
            }
        };
        Ok(durable)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn remove_pending_activation(state_dir: &Path, request_id: &str) -> Result<()> {
    let Some(pending) = read_pending_activation(state_dir)? else {
        return Ok(());
    };
    if pending.request_id != request_id {
        bail!("refusing to remove a different pending update request");
    }
    let root = updates_root(state_dir);
    validate_regular_directory(&root)?;
    let path = pending_pointer_path(state_dir);
    validate_regular_file(&path, Some(MAX_POINTER_BYTES))?;
    fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    if let Err(error) = sync_directory(&root) {
        eprintln!(
            "Warning: pending update was removed but its directory could not be synced ({error:#})."
        );
    }
    Ok(())
}

fn remove_active_release(state_dir: &Path, expected: Option<&ActiveRelease>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let Some(current) = read_active_release(state_dir)? else {
        return Ok(());
    };
    if current.version != expected.version || current.previous_version != expected.previous_version
    {
        bail!("refusing to remove a different active update pointer");
    }
    let root = updates_root(state_dir);
    validate_regular_directory(&root)?;
    let path = active_pointer_path(state_dir);
    validate_regular_file(&path, Some(MAX_POINTER_BYTES))?;
    fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    if let Err(error) = sync_directory(&root) {
        eprintln!(
            "Warning: recovered active pointer was removed but its directory could not be synced ({error:#})."
        );
    }
    Ok(())
}

fn quarantine_pending_activation(state_dir: &Path) -> Result<()> {
    let Some(root) = existing_updates_root(state_dir)? else {
        return Ok(());
    };
    let path = root.join(PENDING_POINTER_NAME);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to inspect pending update"),
    };
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        bail!("unsafe pending update pointer cannot be quarantined");
    }
    let quarantine = root.join(format!(".pending-invalid-{}.json", Uuid::new_v4()));
    fs::rename(&path, &quarantine).context("failed to quarantine pending update")?;
    if let Err(error) = sync_directory(&root) {
        eprintln!(
            "Warning: invalid pending update was quarantined but its directory could not be synced ({error:#})."
        );
    }
    Ok(())
}

fn cleanup_old_releases(state_dir: &Path, current: &Version, previous: &Version) -> Result<()> {
    let releases = updates_root(state_dir).join("releases");
    let metadata = match fs::symlink_metadata(&releases) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to inspect update releases"),
    };
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        bail!("update releases path is not a regular directory");
    }
    for entry in fs::read_dir(&releases).context("failed to list update releases")? {
        let entry = entry.context("failed to inspect an update release")?;
        let name = entry.file_name();
        let Some(text) = name.to_str() else {
            continue;
        };
        let Some(version_text) = text.strip_prefix('v') else {
            continue;
        };
        let Ok(version) = Version::parse(version_text) else {
            continue;
        };
        if &version == current || &version == previous {
            continue;
        }
        safe_remove_tree(&entry.path(), &releases)
            .with_context(|| format!("failed to remove old release {}", entry.path().display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(directory: &Path) -> Config {
        Config {
            host: "0.0.0.0".parse().expect("host"),
            port: 8789,
            max_sessions: 20,
            project_dir: directory.to_path_buf(),
            state_dir: directory.join("state"),
            shell: crate::config::ShellKind::Powershell,
            command: None,
            primary_agent: crate::config::AgentKind::Codex,
            new_session_command: None,
            codex_command: None,
            claude_command: None,
            claude_dangerously_skip_permissions: false,
            agy_command: None,
            no_agent_auto_detect: false,
            agy_dangerously_skip_permissions: false,
            token: None,
            no_open_browser: true,
            log_level: "info".to_owned(),
            update_policy: crate::config::UpdatePolicy::Notify,
        }
    }

    #[test]
    fn readiness_url_uses_loopback_for_unspecified_bind() {
        let directory = tempfile::tempdir().expect("temp directory");
        let config = test_config(directory.path());
        assert_eq!(
            readiness_url(&config).expect("URL").as_str(),
            "http://127.0.0.1:8789/api/health"
        );
    }

    #[test]
    fn readiness_requires_the_exact_per_launch_nonce_and_version() {
        let expected_version = Version::new(1, 2, 3);
        let health = HealthProbe {
            status: "ok".to_owned(),
            server_version: expected_version.to_string(),
            readiness_nonce: Some("expected-nonce".to_owned()),
        };
        assert!(health_is_ready(
            &health,
            &expected_version,
            "expected-nonce"
        ));
        assert!(!health_is_ready(
            &health,
            &expected_version,
            "foreign-nonce"
        ));
        assert!(!health_is_ready(
            &health,
            &Version::new(1, 2, 4),
            "expected-nonce"
        ));
    }

    #[test]
    fn missing_update_pointers_are_not_errors() {
        let directory = tempfile::tempdir().expect("temp directory");
        assert!(
            read_active_release(directory.path())
                .expect("missing active")
                .is_none()
        );
        assert!(
            read_pending_activation(directory.path())
                .expect("missing pending")
                .is_none()
        );
    }

    #[test]
    fn active_pointer_can_be_atomically_replaced() {
        let directory = tempfile::tempdir().expect("temp directory");
        write_active_release(
            directory.path(),
            &Version::new(1, 2, 3),
            &Version::new(1, 2, 2),
        )
        .expect("first pointer");
        write_active_release(
            directory.path(),
            &Version::new(1, 2, 4),
            &Version::new(1, 2, 3),
        )
        .expect("replacement pointer");
        let active = read_active_release(directory.path())
            .expect("read pointer")
            .expect("active pointer");
        assert_eq!(active.version, "1.2.4");
    }

    #[test]
    fn recovered_active_pointer_retains_only_the_recovered_release_and_root() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = Version::new(1, 2, 2);
        write_active_release(
            directory.path(),
            &Version::new(1, 2, 4),
            &Version::new(1, 2, 3),
        )
        .expect("active pointer");
        let expected = read_active_release(directory.path())
            .expect("read active")
            .expect("active");

        commit_recovered_active_release(
            directory.path(),
            Some(&expected),
            &Version::new(1, 2, 3),
            &root,
        )
        .expect("commit recovery");

        let recovered = read_active_release(directory.path())
            .expect("read recovered active")
            .expect("recovered active");
        assert_eq!(recovered.version, "1.2.3");
        assert_eq!(recovered.previous_version, "1.2.2");
    }

    #[test]
    fn recovery_to_root_removes_the_active_pointer() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = Version::new(1, 2, 2);
        write_active_release(directory.path(), &Version::new(1, 2, 3), &root)
            .expect("active pointer");
        let expected = read_active_release(directory.path())
            .expect("read active")
            .expect("active");

        commit_recovered_active_release(directory.path(), Some(&expected), &root, &root)
            .expect("commit root recovery");

        assert!(
            read_active_release(directory.path())
                .expect("read removed active")
                .is_none()
        );
    }

    #[test]
    fn pointer_reads_reject_a_non_directory_updates_path() {
        let directory = tempfile::tempdir().expect("temp directory");
        fs::write(directory.path().join("updates"), b"not a directory").expect("fixture");

        assert!(read_active_release(directory.path()).is_err());
        assert!(read_pending_activation(directory.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn pointer_reads_reject_an_updates_directory_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temp directory");
        let outside = directory.path().join("outside");
        ensure_private_directory(&outside).expect("outside");
        fs::write(
            outside.join(ACTIVE_POINTER_NAME),
            br#"{"schemaVersion":2,"version":"1.2.4","previousVersion":"1.2.3"}"#,
        )
        .expect("external pointer");
        symlink(&outside, directory.path().join("updates")).expect("updates symlink");

        assert!(read_active_release(directory.path()).is_err());
    }

    #[test]
    fn pending_pointer_round_trips_and_requires_the_matching_request() {
        let directory = tempfile::tempdir().expect("temp directory");
        let config = test_config(directory.path());
        let updates = ensure_updates_root(&config.state_dir).expect("updates root");
        ensure_private_directory(&updates.join("releases")).expect("releases");
        let request_id = Uuid::new_v4();
        let pending = PendingActivation {
            schema_version: PENDING_POINTER_SCHEMA_VERSION,
            request_id: request_id.to_string(),
            source_version: "1.2.3".to_owned(),
            target_version: "1.2.4".to_owned(),
        };
        write_json_pointer(
            &config.state_dir,
            PENDING_POINTER_NAME,
            ".pending",
            &pending,
        )
        .expect("pending pointer");

        assert_eq!(
            read_pending_activation(&config.state_dir)
                .expect("read pending")
                .expect("pending")
                .request_id,
            request_id.to_string()
        );
        assert!(remove_pending_activation(&config.state_dir, &Uuid::new_v4().to_string()).is_err());
        remove_pending_activation(&config.state_dir, &request_id.to_string())
            .expect("matching removal");
        assert!(
            read_pending_activation(&config.state_dir)
                .expect("read removed pending")
                .is_none()
        );
    }

    #[test]
    fn pending_pointer_rejects_unknown_fields_and_non_forward_versions() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = directory.path().join("state");
        let root = ensure_updates_root(&state).expect("updates root");
        fs::write(
            root.join(PENDING_POINTER_NAME),
            br#"{"schemaVersion":1,"requestId":"00000000-0000-4000-8000-000000000000","sourceVersion":"2.0.0","targetVersion":"1.0.0","path":"forbidden"}"#,
        )
        .expect("fixture");
        assert!(read_pending_activation(&state).is_err());
    }
}
