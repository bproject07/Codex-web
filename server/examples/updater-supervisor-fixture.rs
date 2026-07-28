use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{Json, Router, extract::State, routing::get};
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use codex_web_terminal::{
    config::Config,
    update_bootstrap,
    update_manifest::{PACKAGE_MANIFEST_NAME, ReleasePackageManifest},
    workspaces::prepare_state_directory_sync,
};

const CONTROL_DIRECTORY_NAME: &str = ".updater-supervisor-regression";
const CONTROL_FILE_NAME: &str = "control.json";
const ROOT_FILE_NAME: &str = "root.json";
const FAIL_READINESS_MARKER: &str = "fixture-fail-readiness";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootRecord {
    process_id: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlRequest {
    action: String,
    version: String,
}

#[derive(Clone)]
struct FixtureState {
    version: Arc<str>,
    root_process_id: u32,
    readiness_nonce: Arc<str>,
    supervised_worker: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
    server_version: String,
    process_id: u32,
    root_process_id: u32,
    supervised_worker: bool,
    readiness_nonce: String,
}

fn main() -> Result<()> {
    let executable = env::current_exe().context("failed to locate fixture executable")?;
    let version = executable_version(&executable)?;
    if env::args_os()
        .skip(1)
        .any(|argument| argument == "--version")
    {
        println!("codex-web {version}");
        return Ok(());
    }

    let worker_context = update_bootstrap::take_worker_context()?;
    let mut config = Config::load()?;
    let token = config
        .token
        .take()
        .context("the regression fixture requires an explicit synthetic token")?;
    // SAFETY: the fixture has not created its Tokio runtime or any threads.
    unsafe {
        env::remove_var("CODEX_WEB_TOKEN");
    }
    prepare_state_directory_sync(&config.state_dir)
        .context("failed to prepare the fixture state directory")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create fixture runtime")?;
    runtime.block_on(run_fixture(
        executable,
        version,
        config,
        token,
        worker_context,
    ))
}

async fn run_fixture(
    executable: PathBuf,
    version: Version,
    config: Config,
    token: String,
    worker_context: update_bootstrap::WorkerContext,
) -> Result<()> {
    let control_directory = config.state_dir.join(CONTROL_DIRECTORY_NAME);

    if !worker_context.supervised {
        fs::create_dir_all(&control_directory).with_context(|| {
            format!(
                "failed to create fixture control directory {}",
                control_directory.display()
            )
        })?;
        let root = RootRecord {
            process_id: process::id(),
        };
        fs::write(
            control_directory.join(ROOT_FILE_NAME),
            serde_json::to_vec(&root).context("failed to encode fixture root record")?,
        )
        .context("failed to write fixture root record")?;

        if update_bootstrap::supervise_startup(&config, &token, &executable).await? {
            return Ok(());
        }
    }

    if executable
        .parent()
        .is_some_and(|parent| parent.join(FAIL_READINESS_MARKER).is_file())
    {
        process::exit(42);
    }

    let root_process_id = read_fixture_root_process_id(&control_directory)?;
    let state = FixtureState {
        version: Arc::from(version.to_string()),
        root_process_id,
        readiness_nonce: Arc::from(
            worker_context
                .readiness_nonce
                .context("fixture worker is missing its readiness nonce")?,
        ),
        supervised_worker: worker_context.supervised,
    };
    spawn_control_watcher(control_directory, state.version.clone());

    let app = Router::new()
        .route("/api/health", get(health))
        .with_state(state);
    let listener = TcpListener::bind((config.host, config.port))
        .await
        .with_context(|| format!("fixture failed to bind {}:{}", config.host, config.port))?;
    axum::serve(listener, app)
        .await
        .context("fixture HTTP server failed")
}

fn read_fixture_root_process_id(control_directory: &Path) -> Result<u32> {
    let root_path = control_directory.join(ROOT_FILE_NAME);
    match fs::read(&root_path) {
        Ok(bytes) => {
            let root: RootRecord =
                serde_json::from_slice(&bytes).context("failed to decode fixture root record")?;
            Ok(root.process_id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A real packaged codex-web root deliberately does not know about this
            // fixture's control files. Zero is an explicit fixture-only sentinel;
            // the regression still observes and verifies the real root PID.
            Ok(0)
        }
        Err(error) => Err(error).context("failed to read fixture root record"),
    }
}

async fn health(State(state): State<FixtureState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        server_version: state.version.to_string(),
        process_id: process::id(),
        root_process_id: state.root_process_id,
        supervised_worker: state.supervised_worker,
        readiness_nonce: state.readiness_nonce.to_string(),
    })
}

fn spawn_control_watcher(control_directory: PathBuf, version: Arc<str>) {
    tokio::spawn(async move {
        let control_path = control_directory.join(CONTROL_FILE_NAME);
        loop {
            if let Ok(bytes) = tokio::fs::read(&control_path).await
                && let Ok(request) = serde_json::from_slice::<ControlRequest>(&bytes)
                && request.action == "restart"
                && request.version == version.as_ref()
            {
                let _ = tokio::fs::remove_file(&control_path).await;
                process::exit(update_bootstrap::UPDATE_RESTART_EXIT_CODE);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
}

fn executable_version(executable: &Path) -> Result<Version> {
    let package_root = executable
        .parent()
        .context("fixture executable has no parent directory")?;
    if package_root.join(PACKAGE_MANIFEST_NAME).is_file() {
        let manifest = ReleasePackageManifest::load(package_root)?;
        return Version::parse(&manifest.version)
            .context("fixture release marker contains an invalid version");
    }
    Version::parse(env!("CARGO_PKG_VERSION")).context("fixture build version is invalid")
}
