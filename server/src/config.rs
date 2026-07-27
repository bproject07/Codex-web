use std::{
    env,
    net::IpAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

const MAX_PROJECT_PATH_LENGTH: usize = 32_767;
const MAX_STATE_PATH_LENGTH: usize = 32_767;
const MAX_COMMAND_LENGTH: usize = 1_024;
const MAX_TOKEN_LENGTH: usize = 512;
const MIN_TOKEN_LENGTH: usize = 16;
const MAX_LOG_LEVEL_LENGTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ShellKind {
    Powershell,
    Cmd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Codex,
    Claude,
    Agy,
}

impl AgentKind {
    pub const ALL: [Self; 3] = [Self::Codex, Self::Claude, Self::Agy];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Agy => "AGY",
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "codex-web",
    version,
    about = "Expose real Codex, Claude, and AGY CLI terminals through a local web interface"
)]
pub struct CliArgs {
    /// Address to bind. Use 0.0.0.0 only on a trusted network.
    #[arg(long, env = "CODEX_WEB_HOST", default_value = "127.0.0.1")]
    pub host: IpAddr,

    /// TCP port to listen on.
    #[arg(long, env = "CODEX_WEB_PORT", default_value_t = 8787)]
    pub port: u16,

    /// Default working directory for the primary terminal and new sessions.
    #[arg(long = "project", env = "CODEX_WEB_PROJECT_DIR", default_value = ".")]
    pub project_dir: PathBuf,

    /// Directory used for server-side Favorites and Recent workspace state.
    #[arg(long, env = "CODEX_WEB_STATE_DIR")]
    pub state_dir: Option<PathBuf>,

    /// Windows shell used to launch executable CLI entry points; ignored on non-Windows hosts.
    #[arg(
        long,
        env = "CODEX_WEB_SHELL",
        value_enum,
        default_value = "powershell"
    )]
    pub shell: ShellKind,

    /// Primary agent executable name or path. Defaults to the selected agent command.
    #[arg(long, env = "CODEX_WEB_COMMAND")]
    pub command: Option<String>,

    /// Agent represented by --command and the primary terminal.
    #[arg(
        long,
        env = "CODEX_WEB_PRIMARY_AGENT",
        value_enum,
        default_value = "codex"
    )]
    pub primary_agent: AgentKind,

    /// Executable used only for terminals created with New. Defaults to --command.
    #[arg(long, env = "CODEX_WEB_NEW_SESSION_COMMAND")]
    pub new_session_command: Option<String>,

    /// Authoritative Codex CLI executable override for the Codex profile.
    #[arg(long, env = "CODEX_WEB_CODEX_COMMAND")]
    pub codex_command: Option<String>,

    /// Authoritative Claude CLI executable override for the Claude profile.
    #[arg(long, env = "CODEX_WEB_CLAUDE_COMMAND")]
    pub claude_command: Option<String>,

    /// Start Claude with all permission checks bypassed.
    #[arg(
        long,
        env = "CODEX_WEB_CLAUDE_DANGEROUSLY_SKIP_PERMISSIONS",
        default_value_t = false
    )]
    pub claude_dangerously_skip_permissions: bool,

    /// Authoritative Google Antigravity CLI executable override for the AGY profile.
    #[arg(long, env = "CODEX_WEB_AGY_COMMAND")]
    pub agy_command: Option<String>,

    /// Disable discovery of agent CLIs that do not have an explicit command override.
    #[arg(long, env = "CODEX_WEB_NO_AGENT_AUTO_DETECT", default_value_t = false)]
    pub no_agent_auto_detect: bool,

    /// Start AGY with all tool permission requests auto-approved.
    #[arg(
        long,
        env = "CODEX_WEB_AGY_DANGEROUSLY_SKIP_PERMISSIONS",
        default_value_t = false
    )]
    pub agy_dangerously_skip_permissions: bool,

    /// Authentication token. A secure ephemeral token is generated when omitted.
    #[arg(long, env = "CODEX_WEB_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Do not open the application in the default browser.
    #[arg(long, default_value_t = false)]
    pub no_open_browser: bool,

    /// tracing filter, for example "info" or "codex_web_terminal=debug".
    #[arg(long, env = "CODEX_WEB_LOG_LEVEL", default_value = "info")]
    pub log_level: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub host: IpAddr,
    pub port: u16,
    pub project_dir: PathBuf,
    pub state_dir: PathBuf,
    pub shell: ShellKind,
    pub command: Option<String>,
    pub primary_agent: AgentKind,
    pub new_session_command: Option<String>,
    pub codex_command: Option<String>,
    pub claude_command: Option<String>,
    pub claude_dangerously_skip_permissions: bool,
    pub agy_command: Option<String>,
    pub no_agent_auto_detect: bool,
    pub agy_dangerously_skip_permissions: bool,
    pub token: Option<String>,
    pub no_open_browser: bool,
    pub log_level: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::from_args(CliArgs::parse())
    }

    pub fn from_args(args: CliArgs) -> Result<Self> {
        if args.port == 0 {
            bail!("--port must be between 1 and 65535");
        }

        validate_length(
            "project directory",
            &args.project_dir.to_string_lossy(),
            MAX_PROJECT_PATH_LENGTH,
        )?;
        if let Some(state_dir) = args.state_dir.as_ref() {
            validate_length(
                "state directory",
                &state_dir.to_string_lossy(),
                MAX_STATE_PATH_LENGTH,
            )?;
        }
        if let Some(command) = args.command.as_deref() {
            validate_nonempty_length("command", command, MAX_COMMAND_LENGTH)?;
        }
        if let Some(command) = args.new_session_command.as_deref() {
            validate_nonempty_length("new session command", command, MAX_COMMAND_LENGTH)?;
        }
        if let Some(command) = args.codex_command.as_deref() {
            validate_nonempty_length("Codex command", command, MAX_COMMAND_LENGTH)?;
        }
        if let Some(command) = args.claude_command.as_deref() {
            validate_nonempty_length("Claude command", command, MAX_COMMAND_LENGTH)?;
        }
        if let Some(command) = args.agy_command.as_deref() {
            validate_nonempty_length("AGY command", command, MAX_COMMAND_LENGTH)?;
        }
        validate_nonempty_length("log level", &args.log_level, MAX_LOG_LEVEL_LENGTH)?;

        if args
            .command
            .as_deref()
            .is_some_and(|command| command.chars().any(|character| character == '\0'))
            || args
                .new_session_command
                .as_deref()
                .is_some_and(|command| command.chars().any(|character| character == '\0'))
            || args
                .codex_command
                .as_deref()
                .is_some_and(|command| command.chars().any(|character| character == '\0'))
            || args
                .claude_command
                .as_deref()
                .is_some_and(|command| command.chars().any(|character| character == '\0'))
            || args
                .agy_command
                .as_deref()
                .is_some_and(|command| command.chars().any(|character| character == '\0'))
        {
            bail!("command must not contain a NUL character");
        }

        if let Some(token) = args.token.as_deref() {
            if token.len() < MIN_TOKEN_LENGTH {
                bail!("authentication token must contain at least {MIN_TOKEN_LENGTH} characters");
            }
            validate_nonempty_length("authentication token", token, MAX_TOKEN_LENGTH)?;
            if token.chars().any(char::is_whitespace) {
                bail!("authentication token must not contain whitespace");
            }
        }

        let project_dir = validate_project_directory(&args.project_dir)?;
        let state_dir = resolve_state_directory(args.state_dir)?;

        Ok(Self {
            host: args.host,
            port: args.port,
            project_dir,
            state_dir,
            shell: args.shell,
            command: args.command,
            primary_agent: args.primary_agent,
            new_session_command: args.new_session_command,
            codex_command: args.codex_command,
            claude_command: args.claude_command,
            claude_dangerously_skip_permissions: args.claude_dangerously_skip_permissions,
            agy_command: args.agy_command,
            no_agent_auto_detect: args.no_agent_auto_detect,
            agy_dangerously_skip_permissions: args.agy_dangerously_skip_permissions,
            token: args.token,
            no_open_browser: args.no_open_browser,
            log_level: args.log_level,
        })
    }
}

fn resolve_state_directory(configured: Option<PathBuf>) -> Result<PathBuf> {
    if configured
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        bail!("state directory must not be empty");
    }
    let path = match configured {
        Some(path) if path.is_absolute() => path,
        Some(path) => env::current_dir()
            .context("failed to resolve the relative state directory")?
            .join(path),
        None => default_state_directory()?,
    };

    validate_length(
        "resolved state directory",
        &path.to_string_lossy(),
        MAX_STATE_PATH_LENGTH,
    )?;
    Ok(path)
}

#[cfg(windows)]
fn default_state_directory() -> Result<PathBuf> {
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join("AppData").join("Local"))
        })
        .context(
            "cannot determine the user state directory; set --state-dir or CODEX_WEB_STATE_DIR",
        )?;
    Ok(base.join("codex-web-terminal"))
}

#[cfg(unix)]
fn default_state_directory() -> Result<PathBuf> {
    if let Some(state_home) = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(state_home.join("codex-web-terminal"));
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .context(
            "cannot determine the user state directory; set --state-dir or CODEX_WEB_STATE_DIR",
        )?;
    Ok(home.join(".local").join("state").join("codex-web-terminal"))
}

#[cfg(not(any(unix, windows)))]
fn default_state_directory() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .context(
            "cannot determine the user state directory; set --state-dir or CODEX_WEB_STATE_DIR",
        )?;
    Ok(home.join(".codex-web-terminal"))
}

fn validate_project_directory(path: &Path) -> Result<PathBuf> {
    let metadata = std::fs::metadata(path).with_context(|| {
        format!(
            "project directory does not exist or is inaccessible: {}",
            path.display()
        )
    })?;

    if !metadata.is_dir() {
        bail!("project path is not a directory: {}", path.display());
    }

    // Opening an iterator verifies that the server identity can access the directory
    // without creating a probe file in the user's project.
    std::fs::read_dir(path)
        .with_context(|| format!("project directory cannot be read: {}", path.display()))?;

    dunce::canonicalize(path)
        .with_context(|| format!("failed to resolve project directory: {}", path.display()))
}

fn validate_nonempty_length(name: &str, value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{name} must not be empty");
    }
    validate_length(name, value, max)
}

fn validate_length(name: &str, value: &str, max: usize) -> Result<()> {
    if value.len() > max {
        bail!("{name} is longer than the allowed {max} bytes");
    }
    Ok(())
}

pub fn static_directory() -> Option<PathBuf> {
    let executable_candidate = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("web")));

    let development_candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("web")
        .join("dist");

    executable_candidate
        .into_iter()
        .chain([development_candidate])
        .find(|candidate| candidate.join("index.html").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_for(project_dir: &Path) -> CliArgs {
        CliArgs {
            host: "127.0.0.1".parse().expect("valid host"),
            port: 8787,
            project_dir: project_dir.to_path_buf(),
            state_dir: Some(project_dir.join("state")),
            shell: ShellKind::Powershell,
            command: None,
            primary_agent: AgentKind::Codex,
            new_session_command: None,
            codex_command: None,
            claude_command: None,
            claude_dangerously_skip_permissions: false,
            agy_command: None,
            no_agent_auto_detect: false,
            agy_dangerously_skip_permissions: false,
            token: Some("0123456789abcdef".to_owned()),
            no_open_browser: true,
            log_level: "info".to_owned(),
        }
    }

    #[test]
    fn parses_and_canonicalizes_valid_config() {
        let directory = tempfile::tempdir().expect("temp directory");
        let config = Config::from_args(args_for(directory.path())).expect("valid config");

        assert_eq!(
            config.project_dir,
            dunce::canonicalize(directory.path()).expect("canonical path")
        );
        assert_eq!(config.state_dir, directory.path().join("state"));
        assert_eq!(config.port, 8787);
        assert_eq!(config.command, None);
        assert_eq!(config.primary_agent, AgentKind::Codex);
        assert_eq!(config.new_session_command, None);
        assert!(!config.claude_dangerously_skip_permissions);
        assert!(!config.agy_dangerously_skip_permissions);
    }

    #[test]
    fn rejects_a_file_as_project_directory() {
        let directory = tempfile::tempdir().expect("temp directory");
        let file_path = directory.path().join("project.txt");
        std::fs::write(&file_path, "not a directory").expect("write fixture");

        let error = Config::from_args(args_for(&file_path)).expect_err("file must be rejected");
        assert!(error.to_string().contains("not a directory"));
    }

    #[test]
    fn rejects_short_tokens() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut args = args_for(directory.path());
        args.token = Some("short".to_owned());

        let error = Config::from_args(args).expect_err("short token must be rejected");
        assert!(error.to_string().contains("at least"));
    }

    #[test]
    fn accepts_a_distinct_new_session_command() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut args = args_for(directory.path());
        args.command = Some("resume-current".to_owned());
        args.new_session_command = Some("codex".to_owned());

        let config = Config::from_args(args).expect("valid commands");

        assert_eq!(config.command.as_deref(), Some("resume-current"));
        assert_eq!(config.new_session_command.as_deref(), Some("codex"));
    }

    #[test]
    fn accepts_explicit_dangerous_permission_flags() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut args = args_for(directory.path());
        args.claude_command = Some("claude".to_owned());
        args.claude_dangerously_skip_permissions = true;
        args.agy_command = Some("agy".to_owned());
        args.agy_dangerously_skip_permissions = true;

        let config = Config::from_args(args).expect("valid agent permission flags");

        assert!(config.claude_dangerously_skip_permissions);
        assert!(config.agy_dangerously_skip_permissions);
    }

    #[test]
    fn resolves_a_relative_state_directory_without_creating_it() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut args = args_for(directory.path());
        args.state_dir = Some(PathBuf::from("codex-web-state"));

        let config = Config::from_args(args).expect("valid state directory");

        assert!(config.state_dir.is_absolute());
        assert!(config.state_dir.ends_with("codex-web-state"));
        assert!(!config.state_dir.exists());
    }
}
