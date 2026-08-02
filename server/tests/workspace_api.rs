use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use codex_web_terminal::{
    agents::build_agent_profiles,
    auth::AuthState,
    config::{AgentKind, Config, DEFAULT_MAX_SESSIONS, ShellKind, UpdatePolicy},
    filesystem::{DirectoryBrowser, encode_directory_id},
    peer::PeerBroker,
    registry::SessionRegistry,
    routes::{AppState, build_router},
    updater::UpdateManager,
    workspaces::WorkspaceStore,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const TOKEN: &str = "workspace-api-test-token";

#[tokio::test]
async fn health_reports_the_configured_session_capacity() {
    let fixture = tempfile::tempdir().expect("temporary project");
    let (app, _, _) = test_router_with_options(fixture.path(), None, 7, true).await;

    let health = json_response(
        app.oneshot(request("GET", "/api/health", None, true))
            .await
            .expect("health response"),
    )
    .await;

    assert_eq!(health["sessionCount"], 1);
    assert_eq!(health["maxSessions"], 7);
    assert_eq!(health["serverRestartSupported"], true);
    assert_eq!(health["serverVersion"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn update_status_is_authenticated_and_reports_disabled_policy() {
    let fixture = tempfile::tempdir().expect("temporary project");
    let app = test_router(fixture.path()).await;

    let unauthorized = app
        .clone()
        .oneshot(request("GET", "/api/update", None, false))
        .await
        .expect("unauthorized response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let status = json_response(
        app.oneshot(request("GET", "/api/update", None, true))
            .await
            .expect("update status response"),
    )
    .await;
    assert_eq!(status["state"], "disabled");
    assert_eq!(status["currentVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status["installSupported"], false);
}

#[tokio::test]
async fn server_restart_requires_authentication_and_explicit_confirmation() {
    let fixture = tempfile::tempdir().expect("temporary project");
    let app = test_router(fixture.path()).await;

    let unauthorized = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/server/restart",
            Some(json!({ "confirmSessionTermination": true })),
            false,
        ))
        .await
        .expect("unauthorized restart response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let unconfirmed = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/server/restart",
            Some(json!({ "confirmSessionTermination": false })),
            true,
        ))
        .await
        .expect("unconfirmed restart response");
    assert_eq!(unconfirmed.status(), StatusCode::CONFLICT);

    let accepted = app
        .oneshot(request(
            "POST",
            "/api/server/restart",
            Some(json!({ "confirmSessionTermination": true })),
            true,
        ))
        .await
        .expect("accepted restart response");
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);

    let unsupported_fixture = tempfile::tempdir().expect("unsupported project");
    let (unsupported_app, _, _) =
        test_router_with_options(unsupported_fixture.path(), None, 7, false).await;
    let unsupported = unsupported_app
        .oneshot(request(
            "POST",
            "/api/server/restart",
            Some(json!({ "confirmSessionTermination": true })),
            true,
        ))
        .await
        .expect("unsupported restart response");
    assert_eq!(unsupported.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn session_creation_reports_the_configured_capacity_conflict() {
    let fixture = tempfile::tempdir().expect("temporary project");
    let (app, sessions, _) = test_router_with_options(fixture.path(), None, 1, true).await;

    let response = app
        .oneshot(request(
            "POST",
            "/api/sessions",
            Some(json!({ "agent": "codex" })),
            true,
        ))
        .await
        .expect("capacity response");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(response).await["error"],
        "Managed terminal session capacity has been reached."
    );
    assert_eq!(sessions.session_count(), 1);
}

#[tokio::test]
async fn peer_provisioning_rolls_back_when_session_capacity_is_full() {
    let fixture = tempfile::tempdir().expect("temporary project");
    let command = write_long_running_agent_fixture(fixture.path());
    let (app, sessions, peers) = test_router_with_options(
        fixture.path(),
        Some(command.to_string_lossy().into_owned()),
        1,
        true,
    )
    .await;
    sessions
        .start_primary()
        .await
        .expect("start source terminal");
    let source = sessions.primary().snapshot();

    let response = app
        .oneshot(request(
            "POST",
            "/api/peer/threads",
            Some(json!({
                "sourceTerminalId": source.terminal_id,
                "targetAgent": "codex",
                "action": "review",
                "instruction": "Verify capacity rollback.",
                "sourceReady": true
            })),
            true,
        ))
        .await
        .expect("peer capacity response");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(sessions.session_count(), 1);
    assert!(peers.list_threads().is_empty());
    sessions.shutdown().await;
}

#[tokio::test]
async fn filesystem_and_workspace_apis_are_authenticated_and_directory_only() {
    let fixture = tempfile::tempdir().expect("temporary project");
    std::fs::create_dir(fixture.path().join("project-a")).expect("create child directory");
    std::fs::write(fixture.path().join("not-a-directory.txt"), "file").expect("write child file");
    let app = test_router(fixture.path()).await;

    let unauthorized = app
        .clone()
        .oneshot(request("GET", "/api/filesystem/roots", None, false))
        .await
        .expect("unauthorized response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let roots = json_response(
        app.clone()
            .oneshot(request("GET", "/api/filesystem/roots", None, true))
            .await
            .expect("roots response"),
    )
    .await;
    let default_id = roots["defaultDirectory"]["id"]
        .as_str()
        .expect("default directory ID")
        .to_owned();

    let listing = json_response(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/filesystem/list",
                Some(json!({ "directoryId": default_id })),
                true,
            ))
            .await
            .expect("listing response"),
    )
    .await;
    let names = listing["directories"]
        .as_array()
        .expect("directory array")
        .iter()
        .map(|entry| entry["name"].as_str().expect("directory name"))
        .collect::<Vec<_>>();
    assert!(names.contains(&"project-a"));
    assert!(!names.contains(&"not-a-directory.txt"));
    assert_eq!(listing["truncated"], false);
    assert!(
        listing["breadcrumbs"]
            .as_array()
            .is_some_and(|breadcrumbs| !breadcrumbs.is_empty())
    );

    let favorite = json_response(
        app.clone()
            .oneshot(request(
                "PUT",
                "/api/workspaces/favorites",
                Some(json!({
                    "directoryId": listing["current"]["id"],
                    "label": "API project",
                    "preferredAgent": "claude"
                })),
                true,
            ))
            .await
            .expect("favorite response"),
    )
    .await;
    assert_eq!(favorite["label"], "API project");
    assert_eq!(favorite["preferredAgent"], "claude");

    let library = json_response(
        app.clone()
            .oneshot(request("GET", "/api/workspaces", None, true))
            .await
            .expect("workspace response"),
    )
    .await;
    assert_eq!(library["version"], 1);
    assert_eq!(library["favorites"].as_array().expect("favorites").len(), 1);

    let stale_delete = app
        .oneshot(request(
            "DELETE",
            "/api/workspaces/favorites/00000000-0000-0000-0000-000000000000",
            None,
            true,
        ))
        .await
        .expect("stale favorite response");
    assert_eq!(stale_delete.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn session_api_rejects_invalid_directory_ids_and_browser_arguments() {
    let fixture = tempfile::tempdir().expect("temporary project");
    let app = test_router(fixture.path()).await;

    let invalid_directory = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/sessions",
            Some(json!({ "directoryId": "forged" })),
            true,
        ))
        .await
        .expect("invalid directory response");
    assert_eq!(invalid_directory.status(), StatusCode::BAD_REQUEST);

    let oversized = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/sessions",
            Some(json!({ "directoryId": "x".repeat(300 * 1024) })),
            true,
        ))
        .await
        .expect("oversized request response");
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let browser_arguments = app
        .oneshot(request(
            "POST",
            "/api/sessions",
            Some(json!({
                "agent": "codex",
                "arguments": ["--dangerously-skip-permissions"]
            })),
            true,
        ))
        .await
        .expect("argument rejection response");
    assert_eq!(browser_arguments.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn successful_session_creation_records_the_selected_directory_and_agent() {
    let fixture = tempfile::tempdir().expect("temporary project");
    let selected = fixture.path().join("selected");
    std::fs::create_dir(&selected).expect("create selected directory");
    let selected = dunce::canonicalize(selected).expect("canonical selected directory");
    let command = write_agent_fixture(fixture.path());
    let (app, sessions, _) =
        test_router_with_command(fixture.path(), Some(command.to_string_lossy().into_owned()))
            .await;
    let directory_id = encode_directory_id(&selected);

    let created = json_response(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/sessions",
                Some(json!({
                    "agent": "codex",
                    "directoryId": directory_id
                })),
                true,
            ))
            .await
            .expect("session response"),
    )
    .await;
    assert_eq!(created["directoryId"], directory_id);
    assert_eq!(created["project"], selected.to_string_lossy().as_ref());

    let library = json_response(
        app.oneshot(request("GET", "/api/workspaces", None, true))
            .await
            .expect("workspace response"),
    )
    .await;
    assert_eq!(library["recent"][0]["directoryId"], directory_id);
    assert_eq!(library["recent"][0]["lastAgent"], "codex");
    sessions.shutdown().await;
}

#[tokio::test]
async fn peer_api_creates_only_a_fresh_linked_reviewer_and_closes_it_cleanly() {
    let fixture = tempfile::tempdir().expect("temporary project");
    let reviewer_project = fixture.path().join("reviewer-project");
    std::fs::create_dir(&reviewer_project).expect("create reviewer project");
    let reviewer_project =
        dunce::canonicalize(reviewer_project).expect("canonical reviewer project");
    let reviewer_directory_id = encode_directory_id(&reviewer_project);
    let command = write_long_running_agent_fixture(fixture.path());
    let (app, sessions, _) =
        test_router_with_command(fixture.path(), Some(command.to_string_lossy().into_owned()))
            .await;
    sessions
        .start_primary()
        .await
        .expect("start source terminal");
    let source = sessions.primary().snapshot();

    let unauthorized = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/peer/threads",
            Some(json!({
                "sourceTerminalId": source.terminal_id,
                "targetAgent": "codex",
                "action": "review",
                "instruction": "Review without assuming Git.",
                "sourceReady": true
            })),
            false,
        ))
        .await
        .expect("unauthorized peer response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let readiness_not_acknowledged = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/peer/threads",
            Some(json!({
                "sourceTerminalId": source.terminal_id,
                "targetAgent": "codex",
                "action": "review",
                "instruction": "Review without assuming Git.",
                "sourceReady": false
            })),
            true,
        ))
        .await
        .expect("readiness response");
    assert_eq!(readiness_not_acknowledged.status(), StatusCode::BAD_REQUEST);
    assert_eq!(sessions.session_count(), 1);

    let invalid_reviewer_directory = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/peer/threads",
            Some(json!({
                "sourceTerminalId": source.terminal_id,
                "directoryId": "forged",
                "targetAgent": "codex",
                "action": "review",
                "instruction": "Reject the invalid reviewer directory.",
                "sourceReady": true
            })),
            true,
        ))
        .await
        .expect("invalid reviewer directory response");
    assert_eq!(invalid_reviewer_directory.status(), StatusCode::BAD_REQUEST);
    assert_eq!(sessions.session_count(), 1);

    let oversized_reviewer_directory = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/peer/threads",
            Some(json!({
                "sourceTerminalId": source.terminal_id,
                "directoryId": "x".repeat(300 * 1024),
                "targetAgent": "codex",
                "action": "review",
                "instruction": "Reject the oversized peer request.",
                "sourceReady": true
            })),
            true,
        ))
        .await
        .expect("oversized peer request response");
    assert_eq!(
        oversized_reviewer_directory.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(sessions.session_count(), 1);

    let created = json_response(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/peer/threads",
                Some(json!({
                    "sourceTerminalId": source.terminal_id,
                    "directoryId": reviewer_directory_id,
                    "targetAgent": "codex",
                    "action": "review",
                    "instruction": "Review without assuming Git.",
                    "sourceReady": true
                })),
                true,
            ))
            .await
            .expect("peer create response"),
    )
    .await;
    let thread_id = created["id"].as_str().expect("thread id");
    let reviewer_id = created["reviewerTerminalId"]
        .as_str()
        .expect("reviewer terminal id");
    assert_eq!(created["sourceTerminalId"], source.terminal_id.to_string());
    assert_eq!(created["status"], "preparing_handoff");
    assert_ne!(reviewer_id, source.terminal_id.to_string());
    assert_eq!(sessions.session_count(), 2);

    let listed = json_response(
        app.clone()
            .oneshot(request("GET", "/api/sessions", None, true))
            .await
            .expect("session list"),
    )
    .await;
    let reviewer = listed
        .as_array()
        .expect("session array")
        .iter()
        .find(|session| session["terminalId"] == reviewer_id)
        .expect("dedicated reviewer");
    assert_eq!(reviewer["purpose"]["kind"], "peer");
    assert_eq!(
        reviewer["purpose"]["parentTerminalId"],
        source.terminal_id.to_string()
    );
    assert_eq!(reviewer["purpose"]["threadId"], thread_id);
    assert_eq!(reviewer["directoryId"], reviewer_directory_id);
    assert_eq!(
        reviewer["project"],
        reviewer_project.to_string_lossy().as_ref()
    );

    let premature_follow_up = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/peer/threads/{thread_id}/turns"),
            Some(json!({
                "action": "recheck",
                "instruction": "This must reuse only a completed peer thread.",
                "sourceReady": true
            })),
            true,
        ))
        .await
        .expect("premature follow-up response");
    assert_eq!(premature_follow_up.status(), StatusCode::CONFLICT);

    let closed = app
        .oneshot(request(
            "DELETE",
            &format!("/api/peer/threads/{thread_id}"),
            None,
            true,
        ))
        .await
        .expect("peer close response");
    assert_eq!(closed.status(), StatusCode::NO_CONTENT);
    assert_eq!(sessions.session_count(), 1);
    assert!(sessions.primary().is_running());
    sessions.shutdown().await;
}

#[tokio::test]
async fn peer_provision_and_close_finish_after_request_cancellation() {
    let fixture = tempfile::tempdir().expect("temporary project");
    let command = write_slow_start_agent_fixture(fixture.path());
    let (app, sessions, peers) =
        test_router_with_command(fixture.path(), Some(command.to_string_lossy().into_owned()))
            .await;
    sessions
        .start_primary()
        .await
        .expect("start source terminal");
    let source = sessions.primary().snapshot();

    let create_app = app.clone();
    let create_request = request(
        "POST",
        "/api/peer/threads",
        Some(json!({
            "sourceTerminalId": source.terminal_id,
            "targetAgent": "codex",
            "action": "review",
            "instruction": "Complete provisioning after the browser request disappears.",
            "sourceReady": true
        })),
        true,
    );
    let create_task = tokio::spawn(async move { create_app.oneshot(create_request).await });

    let reservation_deadline = Instant::now() + Duration::from_secs(3);
    while sessions.session_count() < 2 {
        assert!(
            Instant::now() < reservation_deadline,
            "dedicated reviewer was not reserved"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let provisioning_thread = peers
        .list_threads()
        .into_iter()
        .next()
        .expect("provisioning thread");
    let reserved_reviewer_id = provisioning_thread
        .reviewer_terminal_id
        .expect("provisioning thread already owns the reserved reviewer");
    assert!(sessions.get(reserved_reviewer_id).is_some());
    let premature_close = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/peer/threads/{}", provisioning_thread.id),
            None,
            true,
        ))
        .await
        .expect("close during provisioning response");
    assert_eq!(premature_close.status(), StatusCode::CONFLICT);
    assert!(sessions.get(reserved_reviewer_id).is_some());
    assert!(peers.get_thread(provisioning_thread.id).is_ok());

    create_task.abort();
    let _ = create_task.await;

    let provision_deadline = Instant::now() + Duration::from_secs(5);
    let provisioned = loop {
        let thread = peers.list_threads().into_iter().next();
        if let Some(thread) = thread
            && let Some(reviewer_id) = thread.reviewer_terminal_id
            && sessions
                .get(reviewer_id)
                .is_some_and(|reviewer| reviewer.is_running())
            && peers.bind_reviewer(thread.id, reviewer_id).is_ok()
        {
            break thread;
        }
        assert!(
            Instant::now() < provision_deadline,
            "owned peer provisioning did not finish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let close_app = app.clone();
    let reviewer_shutdown = sessions
        .get(
            provisioned
                .reviewer_terminal_id
                .expect("provisioned reviewer terminal"),
        )
        .expect("reviewer remains registered")
        .shutdown_signal();
    let close_request = request(
        "DELETE",
        &format!("/api/peer/threads/{}", provisioned.id),
        None,
        true,
    );
    let close_task = tokio::spawn(async move { close_app.oneshot(close_request).await });
    let close_started_deadline = Instant::now() + Duration::from_secs(3);
    while !reviewer_shutdown.is_cancelled() {
        assert!(
            Instant::now() < close_started_deadline,
            "dedicated reviewer close did not start"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    close_task.abort();
    let _ = close_task.await;

    let close_deadline = Instant::now() + Duration::from_secs(5);
    while !peers.list_threads().is_empty() || sessions.session_count() != 1 {
        assert!(
            Instant::now() < close_deadline,
            "owned peer close did not finish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(sessions.primary().is_running());
    sessions.shutdown().await;
}

async fn test_router(project: &Path) -> axum::Router {
    test_router_with_command(project, None).await.0
}

async fn test_router_with_command(
    project: &Path,
    command: Option<String>,
) -> (axum::Router, SessionRegistry, PeerBroker) {
    test_router_with_options(project, command, DEFAULT_MAX_SESSIONS, true).await
}

async fn test_router_with_options(
    project: &Path,
    command: Option<String>,
    max_sessions: usize,
    server_restart_supported: bool,
) -> (axum::Router, SessionRegistry, PeerBroker) {
    let project = dunce::canonicalize(project).expect("canonical project");
    let state_dir = project.join("state");
    let config = Arc::new(Config {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 8787,
        max_sessions,
        project_dir: project.clone(),
        state_dir: state_dir.clone(),
        shell: ShellKind::Powershell,
        command,
        primary_agent: AgentKind::Codex,
        new_session_command: None,
        codex_command: None,
        claude_command: None,
        claude_dangerously_skip_permissions: false,
        agy_command: None,
        no_agent_auto_detect: true,
        agy_dangerously_skip_permissions: false,
        token: None,
        no_open_browser: true,
        log_level: "info".to_owned(),
        update_policy: UpdatePolicy::Off,
    });
    let profiles = build_agent_profiles(&config);
    let peers = PeerBroker::new(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 45_001)),
        "codex-web".into(),
    );
    let sessions = SessionRegistry::with_agent_configs_and_peer_broker_with_max_sessions(
        profiles.primary,
        profiles.new_session,
        profiles.additional,
        peers.clone(),
        max_sessions,
    );
    let updates = UpdateManager::new(
        state_dir.clone(),
        UpdatePolicy::Off,
        tokio::sync::mpsc::channel(1).0,
    )
    .expect("update manager");
    let (restart_tx, mut restart_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        while restart_rx.recv().await.is_some() {}
    });
    let state = AppState {
        config,
        auth: AuthState::new(TOKEN.to_owned()),
        sessions: sessions.clone(),
        peers: peers.clone(),
        agents: profiles.catalog,
        directories: DirectoryBrowser::new(project),
        workspaces: WorkspaceStore::open(state_dir)
            .await
            .expect("workspace store"),
        updates,
        restart_tx,
        server_restart_supported,
        shutdown: CancellationToken::new(),
        readiness_nonce: None,
    };
    (build_router(state, None), sessions, peers)
}

fn request(method: &str, uri: &str, body: Option<Value>, authenticated: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 45_000))));
    if authenticated {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    }
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("request")
}

async fn json_response(response: axum::response::Response) -> Value {
    assert!(
        response.status().is_success(),
        "status: {}",
        response.status()
    );
    response_json(response).await
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

#[cfg(windows)]
fn write_agent_fixture(directory: &Path) -> std::path::PathBuf {
    let command = directory.join("workspace-api-agent.cmd");
    std::fs::write(
        &command,
        "@echo off\r\nif \"%~1\"==\"--version\" (\r\n  echo codex 1.2.3\r\n  exit /b 0\r\n)\r\ncd\r\n",
    )
    .expect("write Windows agent fixture");
    command
}

#[cfg(windows)]
fn write_long_running_agent_fixture(directory: &Path) -> std::path::PathBuf {
    let command = directory.join("workspace-api-peer-agent.cmd");
    std::fs::write(
        &command,
        "@echo off\r\nif \"%~1\"==\"--version\" (\r\n  echo codex 1.2.3\r\n  exit /b 0\r\n)\r\necho PEER-READY\r\n:read\r\nset \"line=\"\r\nset /p \"line=\"\r\nif errorlevel 1 goto read\r\necho INPUT-ACCEPTED\r\ngoto read\r\n",
    )
    .expect("write Windows peer agent fixture");
    command
}

#[cfg(windows)]
fn write_slow_start_agent_fixture(directory: &Path) -> std::path::PathBuf {
    let command = directory.join("workspace-api-slow-peer-agent.cmd");
    std::fs::write(
        &command,
        "@echo off\r\nif \"%~1\"==\"--version\" (\r\n  ping -n 2 127.0.0.1 >nul\r\n  echo codex 1.2.3\r\n  exit /b 0\r\n)\r\necho PEER-READY\r\n:read\r\nset \"line=\"\r\nset /p \"line=\"\r\nif errorlevel 1 goto read\r\necho INPUT-ACCEPTED\r\ngoto read\r\n",
    )
    .expect("write slow Windows peer agent fixture");
    command
}

#[cfg(unix)]
fn write_agent_fixture(directory: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let command = directory.join("workspace-api-agent");
    std::fs::write(
        &command,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'codex 1.2.3'\n  exit 0\nfi\npwd\n",
    )
    .expect("write Unix agent fixture");
    let mut permissions = std::fs::metadata(&command)
        .expect("fixture metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&command, permissions).expect("make fixture executable");
    command
}

#[cfg(unix)]
fn write_long_running_agent_fixture(directory: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let command = directory.join("workspace-api-peer-agent");
    std::fs::write(
        &command,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'codex 1.2.3'\n  exit 0\nfi\necho 'PEER-READY'\nwhile IFS= read -r line; do\n  echo 'INPUT-ACCEPTED'\ndone\n",
    )
    .expect("write Unix peer agent fixture");
    let mut permissions = std::fs::metadata(&command)
        .expect("fixture metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&command, permissions).expect("make fixture executable");
    command
}

#[cfg(unix)]
fn write_slow_start_agent_fixture(directory: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let command = directory.join("workspace-api-slow-peer-agent");
    std::fs::write(
        &command,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  sleep 1\n  echo 'codex 1.2.3'\n  exit 0\nfi\necho 'PEER-READY'\nwhile IFS= read -r line; do\n  echo 'INPUT-ACCEPTED'\ndone\n",
    )
    .expect("write slow Unix peer agent fixture");
    let mut permissions = std::fs::metadata(&command)
        .expect("fixture metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&command, permissions).expect("make fixture executable");
    command
}
