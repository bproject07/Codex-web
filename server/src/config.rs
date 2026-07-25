use std::{
    env,
    net::IpAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};

const MAX_PROJECT_PATH_LENGTH: usize = 32_767;
const MAX_COMMAND_LENGTH: usize = 1_024;
const MAX_TOKEN_LENGTH: usize = 512;
const MIN_TOKEN_LENGTH: usize = 16;
const MAX_LOG_LEVEL_LENGTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ShellKind {
    Powershell,
    Cmd,
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "codex-web",
    version,
    about = "Expose the real Codex CLI terminal through a local web interface"
)]
pub struct CliArgs {
    /// Address to bind. Use 0.0.0.0 only on a trusted network.
    #[arg(long, env = "CODEX_WEB_HOST", default_value = "127.0.0.1")]
    pub host: IpAddr,

    /// TCP port to listen on.
    #[arg(long, env = "CODEX_WEB_PORT", default_value_t = 8787)]
    pub port: u16,

    /// Fixed working directory in which Codex will run.
    #[arg(long = "project", env = "CODEX_WEB_PROJECT_DIR", default_value = ".")]
    pub project_dir: PathBuf,

    /// Windows shell used to launch an executable Codex entry point.
    #[arg(
        long,
        env = "CODEX_WEB_SHELL",
        value_enum,
        default_value = "powershell"
    )]
    pub shell: ShellKind,

    /// Codex executable name or path. Shell expressions are not accepted.
    #[arg(long, env = "CODEX_WEB_COMMAND", default_value = "codex")]
    pub command: String,

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
    pub shell: ShellKind,
    pub command: String,
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
        validate_nonempty_length("command", &args.command, MAX_COMMAND_LENGTH)?;
        validate_nonempty_length("log level", &args.log_level, MAX_LOG_LEVEL_LENGTH)?;

        if args.command.chars().any(|character| character == '\0') {
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

        Ok(Self {
            host: args.host,
            port: args.port,
            project_dir,
            shell: args.shell,
            command: args.command,
            token: args.token,
            no_open_browser: args.no_open_browser,
            log_level: args.log_level,
        })
    }
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
            shell: ShellKind::Powershell,
            command: "codex".to_owned(),
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
        assert_eq!(config.port, 8787);
        assert_eq!(config.command, "codex");
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
}
