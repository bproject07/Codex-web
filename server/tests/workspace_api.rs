use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
};

use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use codex_web_terminal::{
    agents::build_agent_profiles,
    auth::AuthState,
    config::{AgentKind, Config, ShellKind},
    filesystem::{DirectoryBrowser, encode_directory_id},
    registry::SessionRegistry,
    routes::{AppState, build_router},
    workspaces::WorkspaceStore,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const TOKEN: &str = "workspace-api-test-token";

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
    let (app, sessions) =
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

async fn test_router(project: &Path) -> axum::Router {
    test_router_with_command(project, None).await.0
}

async fn test_router_with_command(
    project: &Path,
    command: Option<String>,
) -> (axum::Router, SessionRegistry) {
    let project = dunce::canonicalize(project).expect("canonical project");
    let state_dir = project.join("state");
    let config = Arc::new(Config {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 8787,
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
    });
    let profiles = build_agent_profiles(&config);
    let sessions = SessionRegistry::with_agent_configs(
        profiles.primary,
        profiles.new_session,
        profiles.additional,
    );
    let state = AppState {
        config,
        auth: AuthState::new(TOKEN.to_owned()),
        sessions: sessions.clone(),
        agents: profiles.catalog,
        directories: DirectoryBrowser::new(project),
        workspaces: WorkspaceStore::open(state_dir)
            .await
            .expect("workspace store"),
        shutdown: CancellationToken::new(),
    };
    (build_router(state, None), sessions)
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
