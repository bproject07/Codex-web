use std::{
    env,
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[cfg(windows)]
use std::ffi::OsStr;

use anyhow::{Context, Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{
    config::{AgentKind, ShellKind},
    filesystem::validate_canonical_readable_directory,
    peer::{
        CWT_PEER_CAPABILITY_ENV, CWT_PEER_ENDPOINT_ENV, CWT_PEER_HELPER_ENV, CWT_SESSION_ID_ENV,
        CWT_TERMINAL_ID_ENV,
    },
    process_tree::{BoundedProcessOptions, BoundedProcessOutput, run_bounded},
    update_bootstrap::{READINESS_NONCE_ENV, SERVER_RESTART_CAPABILITY_ENV, SUPERVISED_WORKER_ENV},
};

pub const INITIAL_COLS: u16 = 120;
pub const INITIAL_ROWS: u16 = 35;
const CODEX_THREAD_ID_ENV: &str = "CODEX_THREAD_ID";
const CLAUDE_NESTING_ENV: &str = "CLAUDECODE";
const CODEX_WEB_TOKEN_ENV: &str = "CODEX_WEB_TOKEN";
const CLAUDE_DISABLE_AUTOUPDATER_ENV: &str = "DISABLE_AUTOUPDATER";
const AGY_DISABLE_AUTO_UPDATE_ENV: &str = "AGY_CLI_DISABLE_AUTO_UPDATE";
const PEER_ENVIRONMENT_NAMES: [&str; 5] = [
    CWT_PEER_ENDPOINT_ENV,
    CWT_PEER_HELPER_ENV,
    CWT_TERMINAL_ID_ENV,
    CWT_SESSION_ID_ENV,
    CWT_PEER_CAPABILITY_ENV,
];
const VERSION_OUTPUT_LIMIT: usize = 16 * 1024;
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const HELP_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub project_dir: PathBuf,
    pub command: String,
    pub arguments: Vec<String>,
    pub agent: AgentKind,
    pub shell: ShellKind,
}

pub struct SpawnedTerminal {
    pub master: Box<dyn MasterPty + Send>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    path: PathBuf,
    codex_no_daemon: bool,
    #[cfg(windows)]
    is_batch_file: bool,
}

impl ResolvedCommand {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandInspectionState {
    Ready,
    Missing,
    Misconfigured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInspection {
    pub state: CommandInspectionState,
    pub version: Option<String>,
}

pub fn inspect_command(config: &TerminalConfig, explicit_override: bool) -> CommandInspection {
    let search_directories = command_search_directories(config.agent);
    let resolved = match resolve_command_in(&config.command, config.agent, &search_directories) {
        Ok(resolved) => resolved,
        Err(_) => {
            return CommandInspection {
                state: if explicit_override
                    || command_candidate_exists(&config.command, &search_directories)
                {
                    CommandInspectionState::Misconfigured
                } else {
                    CommandInspectionState::Missing
                },
                version: None,
            };
        }
    };

    match probe_command_version(&resolved, &config.project_dir, config.agent) {
        Ok(version) => CommandInspection {
            state: CommandInspectionState::Ready,
            version: Some(version),
        },
        Err(_) => CommandInspection {
            state: CommandInspectionState::Misconfigured,
            version: None,
        },
    }
}

pub fn preflight(config: &TerminalConfig) -> Result<ResolvedCommand> {
    validate_project_directory(config)?;
    let mut resolved = resolve_command(&config.command, config.agent)?;
    probe_command_version(&resolved, &config.project_dir, config.agent)?;
    if config.agent == AgentKind::Codex {
        resolved.codex_no_daemon = probe_codex_no_daemon(&resolved, &config.project_dir);
    }
    Ok(resolved)
}

pub fn validate_project_directory(config: &TerminalConfig) -> Result<()> {
    validate_canonical_readable_directory(&config.project_dir).with_context(|| {
        format!(
            "configured project directory is no longer the same readable directory: {}",
            config.project_dir.display()
        )
    })
}

pub fn spawn_resolved(
    config: &TerminalConfig,
    resolved: &ResolvedCommand,
) -> Result<SpawnedTerminal> {
    spawn_resolved_with_environment(config, resolved, &[])
}

pub fn spawn_resolved_with_environment(
    config: &TerminalConfig,
    resolved: &ResolvedCommand,
    environment: &[(OsString, OsString)],
) -> Result<SpawnedTerminal> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("failed to create a native pseudo-terminal")?;

    let command = pty_command_with_environment(config, resolved, environment);
    validate_project_directory(config)?;
    let child = pair.slave.spawn_command(command).with_context(|| {
        format!(
            "failed to start {} in the pseudo-terminal",
            config.agent.label()
        )
    })?;
    let pid = child.process_id();
    let reader = pair
        .master
        .try_clone_reader()
        .context("failed to open the PTY output stream")?;
    let writer = pair
        .master
        .take_writer()
        .context("failed to open the PTY input stream")?;

    drop(pair.slave);

    Ok(SpawnedTerminal {
        master: pair.master,
        reader,
        writer,
        child,
        pid,
    })
}

#[cfg(test)]
fn pty_command(config: &TerminalConfig, resolved: &ResolvedCommand) -> CommandBuilder {
    pty_command_with_environment(config, resolved, &[])
}

fn pty_command_with_environment(
    config: &TerminalConfig,
    resolved: &ResolvedCommand,
    environment: &[(OsString, OsString)],
) -> CommandBuilder {
    let mut arguments = config.arguments.clone();
    if config.agent == AgentKind::Codex && resolved.codex_no_daemon {
        arguments.push("--no-daemon".to_owned());
    }

    #[cfg(windows)]
    let mut command = if resolved.is_batch_file || config.shell == ShellKind::Cmd {
        let mut command = CommandBuilder::new("cmd.exe");
        command.args(["/d", "/s", "/c", "call"]);
        command.arg(&resolved.path);
        command.args(&arguments);
        command
    } else {
        let mut command = CommandBuilder::new("powershell.exe");
        command.args(["-NoLogo", "-NoProfile", "-Command"]);
        command.arg(powershell_invocation(&resolved.path, &arguments));
        command
    };

    #[cfg(not(windows))]
    let mut command = {
        let mut command = CommandBuilder::new(&resolved.path);
        command.args(&arguments);
        command
    };

    command.cwd(&config.project_dir);
    remove_parent_agent_markers(&mut command);
    remove_inherited_peer_environment(&mut command);
    for (name, value) in environment {
        command.env(name, value);
    }
    remove_parent_agent_markers(&mut command);
    remove_server_secret_environment(&mut command);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command
}

fn remove_parent_agent_markers(command: &mut CommandBuilder) {
    command.env_remove(CODEX_THREAD_ID_ENV);
    command.env_remove(CLAUDE_NESTING_ENV);
}

fn remove_inherited_peer_environment(command: &mut CommandBuilder) {
    for name in PEER_ENVIRONMENT_NAMES {
        command.env_remove(name);
    }
}

fn remove_server_secret_environment(command: &mut CommandBuilder) {
    command.env_remove(CODEX_WEB_TOKEN_ENV);
    command.env_remove(SUPERVISED_WORKER_ENV);
    command.env_remove(READINESS_NONCE_ENV);
    command.env_remove(SERVER_RESTART_CAPABILITY_ENV);
}

fn resolve_command(command: &str, agent: AgentKind) -> Result<ResolvedCommand> {
    resolve_command_in(command, agent, &command_search_directories(agent))
}

fn resolve_command_in(
    command: &str,
    agent: AgentKind,
    search_directories: &[PathBuf],
) -> Result<ResolvedCommand> {
    let requested = Path::new(command);
    let contains_path_separator = command.contains(['\\', '/']);

    if requested.is_absolute() || contains_path_separator {
        return resolve_candidate_path(requested, agent);
    }

    #[cfg(windows)]
    let extensions = ["exe", "cmd", ""];
    #[cfg(not(windows))]
    let extensions = [""];
    let mut first_candidate_error = None;

    // Extension is the outer loop intentionally: codex.exe is preferred over
    // codex.cmd across PATH, as required for predictable Windows startup.
    for extension in extensions {
        let file_name = if extension.is_empty() || requested.extension().is_some() {
            command.to_owned()
        } else {
            format!("{command}.{extension}")
        };

        for directory in search_directories {
            let candidate = directory.join(&file_name);
            if candidate.is_file() {
                match resolved_from_existing_path(candidate, agent) {
                    Ok(resolved) => return Ok(resolved),
                    Err(error) => {
                        first_candidate_error.get_or_insert(error);
                    }
                }
            }
        }

        if requested.extension().is_some() {
            break;
        }
    }

    if let Some(error) = first_candidate_error {
        return Err(error);
    }

    #[cfg(windows)]
    bail!(
        "{} CLI was not found. Install it, make sure its .exe or .cmd entry point is in PATH, then verify `{command} --version`.",
        agent.label()
    );

    #[cfg(not(windows))]
    bail!(
        "{} CLI was not found. Install it, make sure the executable is in PATH, then verify `{command} --version`.",
        agent.label()
    );
}

fn command_search_directories(agent: AgentKind) -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|directory| directory.is_absolute())
        .collect();

    for directory in well_known_command_directories(agent) {
        push_unique_path(&mut directories, directory);
    }
    directories
}

fn push_unique_path(directories: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !candidate.is_absolute() {
        return;
    }

    #[cfg(windows)]
    let already_present = directories.iter().any(|existing| {
        existing
            .to_string_lossy()
            .eq_ignore_ascii_case(&candidate.to_string_lossy())
    });

    #[cfg(not(windows))]
    let already_present = directories.iter().any(|existing| existing == &candidate);

    if !already_present {
        directories.push(candidate);
    }
}

fn command_candidate_exists(command: &str, search_directories: &[PathBuf]) -> bool {
    let requested = Path::new(command);
    if requested.is_absolute() || command.contains(['\\', '/']) {
        if requested.is_file() {
            return true;
        }
        #[cfg(windows)]
        return requested.extension().is_none()
            && ["exe", "cmd"]
                .into_iter()
                .any(|extension| requested.with_extension(extension).is_file());
        #[cfg(not(windows))]
        return false;
    }

    #[cfg(windows)]
    let extensions = ["exe", "cmd", ""];
    #[cfg(not(windows))]
    let extensions = [""];

    extensions.into_iter().any(|extension| {
        let file_name = if extension.is_empty() || requested.extension().is_some() {
            command.to_owned()
        } else {
            format!("{command}.{extension}")
        };
        search_directories
            .iter()
            .any(|directory| directory.join(&file_name).is_file())
    })
}

fn well_known_command_directories(agent: AgentKind) -> Vec<PathBuf> {
    let mut directories = Vec::new();

    #[cfg(windows)]
    {
        if agent == AgentKind::Codex
            && let Some(install_dir) = env::var_os("CODEX_INSTALL_DIR").map(PathBuf::from)
        {
            directories.push(install_dir);
        }

        if let Some(local_app_data) = env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            match agent {
                AgentKind::Codex => {
                    directories.push(
                        local_app_data
                            .join("Programs")
                            .join("OpenAI")
                            .join("Codex")
                            .join("bin"),
                    );
                }
                AgentKind::Claude => {}
                AgentKind::Agy => directories.push(local_app_data.join("agy").join("bin")),
            }
            directories.push(
                local_app_data
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Links"),
            );
        }

        if agent == AgentKind::Claude
            && let Some(user_profile) = env::var_os("USERPROFILE").map(PathBuf::from)
        {
            directories.push(user_profile.join(".local").join("bin"));
        }

        if matches!(agent, AgentKind::Codex | AgentKind::Claude)
            && let Some(app_data) = env::var_os("APPDATA").map(PathBuf::from)
        {
            directories.push(app_data.join("npm"));
        }
    }

    #[cfg(not(windows))]
    {
        if agent == AgentKind::Codex
            && let Some(install_dir) = env::var_os("CODEX_INSTALL_DIR").map(PathBuf::from)
        {
            directories.push(install_dir);
        }
        if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
            directories.push(home.join(".local").join("bin"));
        }
        directories.push(PathBuf::from("/usr/local/bin"));
        directories.push(PathBuf::from("/opt/homebrew/bin"));
    }

    directories
}

fn resolve_candidate_path(path: &Path, agent: AgentKind) -> Result<ResolvedCommand> {
    if path.is_file() {
        return resolved_from_existing_path(path.to_path_buf(), agent);
    }

    #[cfg(windows)]
    if path.extension().is_none() {
        for extension in ["exe", "cmd"] {
            let candidate = path.with_extension(extension);
            if candidate.is_file() {
                return resolved_from_existing_path(candidate, agent);
            }
        }
    }

    bail!(
        "{} command does not exist or is not a file: {}",
        agent.label(),
        path.display()
    )
}

fn resolved_from_existing_path(path: PathBuf, agent: AgentKind) -> Result<ResolvedCommand> {
    let canonical_path = dunce::canonicalize(&path).with_context(|| {
        format!(
            "failed to resolve {} command: {}",
            agent.label(),
            path.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let permissions = std::fs::metadata(&canonical_path)
            .with_context(|| {
                format!(
                    "failed to inspect {} command: {}",
                    agent.label(),
                    path.display()
                )
            })?
            .permissions();
        if permissions.mode() & 0o111 == 0 {
            bail!("{} command is not executable", agent.label());
        }
    }

    #[cfg(windows)]
    let is_batch_file = canonical_path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"));

    Ok(ResolvedCommand {
        path: canonical_path,
        codex_no_daemon: false,
        #[cfg(windows)]
        is_batch_file,
    })
}

fn probe_command_version(
    resolved: &ResolvedCommand,
    project_dir: &Path,
    agent: AgentKind,
) -> Result<String> {
    let output = run_command_probe(
        resolved,
        project_dir,
        agent,
        "--version",
        VERSION_OUTPUT_LIMIT,
    )
    .with_context(|| format!("failed to run `{} --version`", agent.label()))?;

    if !output.status.success() {
        #[cfg(windows)]
        bail!(
            "`{} --version` failed with status {}. Verify the CLI installation and PowerShell execution policy.",
            agent.label(),
            output.status,
        );

        #[cfg(not(windows))]
        bail!(
            "`{} --version` failed with status {}. Verify the CLI installation and executable permissions.",
            agent.label(),
            output.status,
        );
    }

    sanitized_version(&output.stdout)
        .or_else(|| sanitized_version(&output.stderr))
        .with_context(|| format!("`{} --version` returned no version text", agent.label()))
}

fn probe_codex_no_daemon(resolved: &ResolvedCommand, project_dir: &Path) -> bool {
    // Older CLIs and trusted wrappers may not implement this optional flag.
    // Check the exact executable on each launch, including after a host update.
    let Ok(output) = run_command_probe(
        resolved,
        project_dir,
        AgentKind::Codex,
        "--help",
        HELP_OUTPUT_LIMIT,
    ) else {
        return false;
    };
    output.status.success()
        && !output.stdout_truncated
        && !output.stderr_truncated
        && (help_advertises_no_daemon(&output.stdout) || help_advertises_no_daemon(&output.stderr))
}

fn help_advertises_no_daemon(output: &[u8]) -> bool {
    String::from_utf8_lossy(output)
        .lines()
        .any(|line| matches!(line.split_whitespace().next(), Some("--no-daemon")))
}

fn run_command_probe(
    resolved: &ResolvedCommand,
    project_dir: &Path,
    agent: AgentKind,
    argument: &'static str,
    output_limit: usize,
) -> Result<BoundedProcessOutput> {
    #[cfg(windows)]
    let mut command = if resolved.is_batch_file {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/s", "/c", "call"]);
        command.arg(&resolved.path);
        command.arg(argument);
        command
    } else {
        let mut command = Command::new(&resolved.path);
        command.arg(argument);
        command
    };

    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(&resolved.path);
        command.arg(argument);
        command
    };

    configure_version_probe_environment(&mut command, agent);
    command.current_dir(project_dir);
    run_bounded(
        &mut command,
        BoundedProcessOptions {
            timeout: VERSION_PROBE_TIMEOUT,
            stdout_limit: output_limit,
            stderr_limit: output_limit,
        },
    )
}

fn configure_version_probe_environment(command: &mut Command, agent: AgentKind) {
    command
        .env_remove(CODEX_THREAD_ID_ENV)
        .env_remove(CLAUDE_NESTING_ENV)
        .env_remove(CODEX_WEB_TOKEN_ENV)
        .env_remove(SUPERVISED_WORKER_ENV)
        .env_remove(READINESS_NONCE_ENV)
        .env_remove(SERVER_RESTART_CAPABILITY_ENV);
    for name in PEER_ENVIRONMENT_NAMES {
        command.env_remove(name);
    }

    match agent {
        AgentKind::Claude => {
            command.env(CLAUDE_DISABLE_AUTOUPDATER_ENV, "1");
        }
        AgentKind::Agy => {
            command.env(AGY_DISABLE_AUTO_UPDATE_ENV, "true");
        }
        AgentKind::Codex => {}
    }
}

fn sanitized_version(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output)
        .split_whitespace()
        .map(|candidate| {
            candidate.trim_matches(|character: char| {
                matches!(
                    character,
                    '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | ':' | '='
                )
            })
        })
        .filter_map(normalize_version_token)
        .next()
}

fn normalize_version_token(token: &str) -> Option<String> {
    let token = token
        .strip_prefix('v')
        .or_else(|| token.strip_prefix('V'))
        .unwrap_or(token);
    if token.is_empty() || token.len() > 64 {
        return None;
    }

    let (version_and_prerelease, build) = match token.split_once('+') {
        Some((version, build))
            if !build.contains('+') && valid_semver_identifiers(build, false) =>
        {
            (version, Some(build))
        }
        Some(_) => return None,
        None => (token, None),
    };
    let (core, prerelease) = match version_and_prerelease.split_once('-') {
        Some((core, prerelease)) if valid_semver_identifiers(prerelease, true) => {
            (core, Some(prerelease))
        }
        Some(_) => return None,
        None => (version_and_prerelease, None),
    };

    let mut components = core.split('.');
    let (Some(major), Some(minor), Some(patch), None) = (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    ) else {
        return None;
    };
    if ![major, minor, patch]
        .into_iter()
        .all(valid_semver_numeric_identifier)
    {
        return None;
    }

    let _ = (prerelease, build);

    Some(token.to_owned())
}

fn valid_semver_numeric_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .chars()
            .all(|character| character.is_ascii_digit())
        && (identifier == "0" || !identifier.starts_with('0'))
}

fn valid_semver_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
                && !(reject_numeric_leading_zero
                    && identifier
                        .chars()
                        .all(|character| character.is_ascii_digit())
                    && !valid_semver_numeric_identifier(identifier))
        })
}

#[cfg(windows)]
fn powershell_invocation(path: &Path, arguments: &[String]) -> String {
    let mut invocation = format!("& {}", powershell_literal(&path.to_string_lossy()));
    for argument in arguments {
        invocation.push(' ');
        invocation.push_str(&powershell_literal(argument));
    }
    invocation.push_str("; exit $LASTEXITCODE");
    invocation
}

#[cfg(windows)]
fn powershell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod command_tests {
    use std::{
        collections::HashMap,
        ffi::{OsStr, OsString},
    };

    use super::*;

    #[test]
    fn no_daemon_requires_an_exact_help_option() {
        assert!(help_advertises_no_daemon(
            b"Options:\r\n      --no-daemon  Run without the background server\r\n"
        ));
        for text in [
            "Options:\n  --yolo  Disable approvals\n",
            "error: unexpected argument '--no-daemon' found",
            "Use --no-daemon with a newer CLI",
            "  --no-daemon-extra\n",
            "  --no-daemon=<value>\n",
        ] {
            assert!(!help_advertises_no_daemon(text.as_bytes()), "{text}");
        }
    }

    #[test]
    fn native_codex_launch_preserves_legacy_and_detects_standalone_support() {
        for (version, help, help_status, standalone) in [
            ("0.155.1", "  --yolo", 0, false),
            ("0.156.0", "  --no-daemon  Disable daemon", 0, true),
            ("0.156.0-alpha.1", "  --no-daemon", 0, true),
            ("9.0.0", "  --yolo", 0, false),
            ("0.155.1", "  --no-daemon", 2, false),
        ] {
            let directory = tempfile::Builder::new()
                .prefix("codex web compatibility ")
                .tempdir()
                .expect("temporary fixture directory");
            #[cfg(windows)]
            let command_path = directory.path().join("codex.cmd");
            #[cfg(not(windows))]
            let command_path = directory.path().join("codex");
            #[cfg(windows)]
            let source = format!(
                "@echo off\r\n\
                 if \"%~1\"==\"--version\" goto version\r\n\
                 if \"%~1\"==\"--help\" goto help\r\n\
                 if not \"%~1\"==\"--yolo\" exit /b 41\r\n\
                 if not \"%~2\"==\"{}\" exit /b 42\r\n\
                 if not \"%~3\"==\"\" exit /b 43\r\n\
                 exit /b 0\r\n\
                 :version\r\n\
                 if not \"%~2\"==\"\" exit /b 44\r\n\
                 echo codex-cli {version}\r\n\
                 exit /b 0\r\n\
                 :help\r\n\
                 if not \"%~2\"==\"\" exit /b 45\r\n\
                 echo {help}\r\n\
                 exit /b {help_status}\r\n",
                if standalone { "--no-daemon" } else { "" },
            );
            #[cfg(not(windows))]
            let source = format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = --version ]; then\n\
                   [ \"$#\" -eq 1 ] || exit 44\n\
                   echo 'codex-cli {version}'\n\
                   exit 0\n\
                 fi\n\
                 if [ \"$1\" = --help ]; then\n\
                   [ \"$#\" -eq 1 ] || exit 45\n\
                   echo '{help}'\n\
                   exit {help_status}\n\
                 fi\n\
                 [ \"$#\" -eq {} ] && [ \"$1\" = --yolo ] && [ \"$2\" = '{}' ]\n",
                if standalone { 2 } else { 1 },
                if standalone { "--no-daemon" } else { "" },
            );
            std::fs::write(&command_path, source).expect("write Codex fixture");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&command_path, std::fs::Permissions::from_mode(0o700))
                    .expect("make Codex fixture executable");
            }
            let config = TerminalConfig {
                project_dir: dunce::canonicalize(directory.path()).expect("canonical fixture path"),
                command: command_path.to_string_lossy().into_owned(),
                arguments: vec!["--yolo".to_owned()],
                agent: AgentKind::Codex,
                shell: ShellKind::Powershell,
            };
            let resolved = preflight(&config).expect("Codex fixture preflight");
            assert_eq!(resolved.codex_no_daemon, standalone, "version {version}");
            let mut terminal =
                spawn_resolved(&config, &resolved).expect("native Codex fixture PTY");
            // ConPTY needs its output drained even when the command only exits.
            // The real session reader does this continuously as well.
            let reader_thread = std::thread::spawn(move || {
                std::io::copy(&mut terminal.reader, &mut std::io::sink())
            });
            #[cfg(windows)]
            {
                // Answer ConPTY's startup cursor query as xterm does in the browser.
                terminal
                    .writer
                    .write_all(b"\x1b[1;1R")
                    .expect("answer ConPTY cursor query");
                terminal.writer.flush().expect("flush cursor response");
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let status = loop {
                if let Some(status) = terminal.child.try_wait().expect("poll Codex fixture") {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = terminal.child.kill();
                    let _ = terminal.child.wait();
                    panic!("Codex fixture {version} did not exit");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            drop(terminal.writer);
            drop(terminal.master);
            let _ = reader_thread.join().expect("join fixture output reader");
            assert_eq!(
                status.exit_code(),
                0,
                "version {version}, help status {help_status}"
            );
        }
    }

    #[test]
    fn child_agents_do_not_inherit_parent_session_markers() {
        let mut command = CommandBuilder::new("codex");
        command.env(CODEX_THREAD_ID_ENV, "parent-thread");
        command.env(CODEX_WEB_TOKEN_ENV, "server-bearer-token");
        command.env(SUPERVISED_WORKER_ENV, "1");
        command.env(READINESS_NONCE_ENV, "internal-nonce");
        command.env(SERVER_RESTART_CAPABILITY_ENV, "1");
        for name in PEER_ENVIRONMENT_NAMES {
            command.env(name, "stale-peer-value");
        }

        remove_parent_agent_markers(&mut command);
        remove_inherited_peer_environment(&mut command);
        remove_server_secret_environment(&mut command);

        assert_eq!(command.get_env(CODEX_THREAD_ID_ENV), None);
        assert_eq!(command.get_env(CLAUDE_NESTING_ENV), None);
        assert_eq!(command.get_env(CODEX_WEB_TOKEN_ENV), None);
        assert_eq!(command.get_env(SUPERVISED_WORKER_ENV), None);
        assert_eq!(command.get_env(READINESS_NONCE_ENV), None);
        assert_eq!(command.get_env(SERVER_RESTART_CAPABILITY_ENV), None);
        for name in PEER_ENVIRONMENT_NAMES {
            assert_eq!(command.get_env(name), None);
        }
    }

    #[test]
    fn internal_environment_is_applied_without_restoring_parent_agent_markers() {
        let config = TerminalConfig {
            project_dir: PathBuf::from("."),
            command: "codex".to_owned(),
            arguments: Vec::new(),
            agent: AgentKind::Codex,
            shell: ShellKind::Powershell,
        };
        let resolved = ResolvedCommand {
            path: PathBuf::from("codex"),
            codex_no_daemon: false,
            #[cfg(windows)]
            is_batch_file: false,
        };
        let environment = vec![
            (
                OsString::from("CWT_PEER_ENDPOINT"),
                OsString::from("127.0.0.1:43123"),
            ),
            (
                OsString::from(CODEX_THREAD_ID_ENV),
                OsString::from("parent-thread"),
            ),
            (
                OsString::from(CODEX_WEB_TOKEN_ENV),
                OsString::from("server-bearer-token"),
            ),
            (OsString::from("TERM"), OsString::from("unsafe-override")),
        ];

        let command = pty_command_with_environment(&config, &resolved, &environment);

        assert_eq!(
            command.get_env("CWT_PEER_ENDPOINT"),
            Some(OsStr::new("127.0.0.1:43123"))
        );
        assert_eq!(command.get_env(CODEX_THREAD_ID_ENV), None);
        assert_eq!(command.get_env(CLAUDE_NESTING_ENV), None);
        assert_eq!(command.get_env(CODEX_WEB_TOKEN_ENV), None);
        assert_eq!(command.get_env("TERM"), Some(OsStr::new("xterm-256color")));
    }

    #[test]
    fn version_probes_are_non_nesting_and_disable_provider_updaters() {
        fn environment(command: &Command) -> HashMap<OsString, Option<OsString>> {
            command
                .get_envs()
                .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
                .collect()
        }

        let mut claude = Command::new("claude");
        claude
            .env(CODEX_THREAD_ID_ENV, "parent-codex")
            .env(CLAUDE_NESTING_ENV, "parent-claude")
            .env(CODEX_WEB_TOKEN_ENV, "server-bearer-token")
            .env(SERVER_RESTART_CAPABILITY_ENV, "1")
            .env("CWT_PEER_CAPABILITY", "stale-peer-secret");
        configure_version_probe_environment(&mut claude, AgentKind::Claude);
        let claude_environment = environment(&claude);
        assert_eq!(
            claude_environment.get(OsStr::new(CODEX_THREAD_ID_ENV)),
            Some(&None)
        );
        assert_eq!(
            claude_environment.get(OsStr::new(CLAUDE_NESTING_ENV)),
            Some(&None)
        );
        assert_eq!(
            claude_environment.get(OsStr::new(CODEX_WEB_TOKEN_ENV)),
            Some(&None)
        );
        assert_eq!(
            claude_environment.get(OsStr::new(SERVER_RESTART_CAPABILITY_ENV)),
            Some(&None)
        );
        assert_eq!(
            claude_environment.get(OsStr::new(CLAUDE_DISABLE_AUTOUPDATER_ENV)),
            Some(&Some(OsString::from("1")))
        );
        assert_eq!(
            claude_environment.get(OsStr::new("CWT_PEER_CAPABILITY")),
            Some(&None)
        );
        assert!(!claude_environment.contains_key(OsStr::new(AGY_DISABLE_AUTO_UPDATE_ENV)));

        let mut agy = Command::new("agy");
        configure_version_probe_environment(&mut agy, AgentKind::Agy);
        let agy_environment = environment(&agy);
        assert_eq!(
            agy_environment.get(OsStr::new(AGY_DISABLE_AUTO_UPDATE_ENV)),
            Some(&Some(OsString::from("true")))
        );
        assert_eq!(
            agy_environment.get(OsStr::new(CODEX_WEB_TOKEN_ENV)),
            Some(&None)
        );
        assert!(!agy_environment.contains_key(OsStr::new(CLAUDE_DISABLE_AUTOUPDATER_ENV)));

        let mut codex = Command::new("codex");
        configure_version_probe_environment(&mut codex, AgentKind::Codex);
        let codex_environment = environment(&codex);
        assert_eq!(
            codex_environment.get(OsStr::new(CODEX_WEB_TOKEN_ENV)),
            Some(&None)
        );
        assert!(!codex_environment.contains_key(OsStr::new(CLAUDE_DISABLE_AUTOUPDATER_ENV)));
        assert!(!codex_environment.contains_key(OsStr::new(AGY_DISABLE_AUTO_UPDATE_ENV)));
    }

    #[test]
    fn missing_auto_command_and_missing_override_have_distinct_states() {
        let config = TerminalConfig {
            project_dir: PathBuf::from("."),
            command: "codex-web-definitely-missing-agent-command-7f3d".to_owned(),
            arguments: Vec::new(),
            agent: AgentKind::Claude,
            shell: ShellKind::Powershell,
        };

        assert_eq!(
            inspect_command(&config, false).state,
            CommandInspectionState::Missing
        );
        assert_eq!(
            inspect_command(&config, true).state,
            CommandInspectionState::Misconfigured
        );
    }

    #[test]
    fn explicit_missing_path_does_not_fall_back_to_search_directories() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("explicit").join("missing-agent");

        assert!(
            resolve_command_in(
                &missing.to_string_lossy(),
                AgentKind::Claude,
                &[directory.path().to_path_buf()],
            )
            .is_err()
        );
    }

    #[test]
    fn version_text_exposes_only_a_bounded_semantic_version() {
        let output = "warning: C:\\Users\\private-name\\agent\n\
                      \u{202e}codex-cli v9.8.7-beta.1+build.4\n";

        let version = sanitized_version(output.as_bytes()).expect("sanitized version");

        assert_eq!(version, "9.8.7-beta.1+build.4");
        assert!(!version.contains("private-name"));
        assert!(!version.contains('\u{202e}'));
    }

    #[test]
    fn version_text_rejects_paths_and_arbitrary_warning_lines() {
        assert_eq!(
            sanitized_version(b"warning from C:\\Users\\private-name\\agent"),
            None
        );
        assert_eq!(sanitized_version(b"completed successfully"), None);
        assert_eq!(
            sanitized_version(b"warning: account 12.34_private-name"),
            None
        );
        assert_eq!(sanitized_version(b"agent 01.2.3"), None);
        assert_eq!(sanitized_version(b"agent 1.02.3"), None);
        assert_eq!(sanitized_version(b"agent 1.2"), None);
        assert_eq!(sanitized_version(b"agent 1.2.3-01"), None);
    }

    #[test]
    fn discovery_does_not_add_relative_search_directories() {
        let mut directories = vec![PathBuf::from("/already/absolute")];

        push_unique_path(&mut directories, PathBuf::from("."));

        assert_eq!(directories.len(), 1);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::{ffi::OsString, time::Instant};

    use super::*;

    #[test]
    fn passes_fixed_arguments_to_an_executable_through_powershell() {
        let resolved = ResolvedCommand {
            path: PathBuf::from(r"C:\Program Files\Codex\codex.exe"),
            codex_no_daemon: true,
            is_batch_file: false,
        };
        let config = TerminalConfig {
            project_dir: PathBuf::from(r"C:\project"),
            command: "ignored".to_owned(),
            arguments: vec!["--yolo".to_owned()],
            agent: AgentKind::Codex,
            shell: ShellKind::Powershell,
        };

        let command = pty_command(&config, &resolved);
        let expected: Vec<OsString> = vec![
            "powershell.exe".into(),
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-Command".into(),
            "& 'C:\\Program Files\\Codex\\codex.exe' '--yolo' '--no-daemon'; exit $LASTEXITCODE"
                .into(),
        ];

        assert_eq!(command.get_argv(), &expected);
    }

    #[test]
    fn escapes_powershell_literals_in_executable_paths_and_arguments() {
        let arguments = vec!["value with ' quote".to_owned()];

        assert_eq!(
            powershell_invocation(
                Path::new(r"C:\Program Files\Agent's CLI\agent.exe"),
                &arguments,
            ),
            "& 'C:\\Program Files\\Agent''s CLI\\agent.exe' 'value with '' quote'; exit $LASTEXITCODE"
        );
    }

    #[test]
    fn passes_codex_arguments_separately_through_cmd() {
        for (path, is_batch_file, shell) in [
            (
                r"C:\Program Files\Codex\codex.cmd",
                true,
                ShellKind::Powershell,
            ),
            (r"C:\Program Files\Codex\codex.exe", false, ShellKind::Cmd),
        ] {
            let resolved = ResolvedCommand {
                path: PathBuf::from(path),
                codex_no_daemon: true,
                is_batch_file,
            };
            let config = TerminalConfig {
                project_dir: PathBuf::from(r"C:\project"),
                command: "ignored".to_owned(),
                arguments: vec!["--yolo".to_owned()],
                agent: AgentKind::Codex,
                shell,
            };
            let command = pty_command(&config, &resolved);
            let expected: Vec<OsString> = vec![
                "cmd.exe".into(),
                "/d".into(),
                "/s".into(),
                "/c".into(),
                "call".into(),
                path.into(),
                "--yolo".into(),
                "--no-daemon".into(),
            ];

            assert_eq!(command.get_argv(), &expected);
        }
    }

    #[test]
    fn verifies_a_batch_codex_entry_point_with_spaces_in_its_path() {
        let directory = tempfile::Builder::new()
            .prefix("codex web terminal ")
            .tempdir()
            .expect("temp directory");
        let command_path = directory.path().join("codex.cmd");
        std::fs::write(
            &command_path,
            "@echo off\r\nif not \"%~1\"==\"--version\" exit /b 8\r\nif not \"%~2\"==\"\" exit /b 9\r\necho codex-cli 1.0.0\r\nexit /b 0\r\n",
        )
        .expect("write fake Codex command");
        let resolved =
            resolved_from_existing_path(command_path, AgentKind::Codex).expect("resolve command");

        probe_command_version(&resolved, directory.path(), AgentKind::Codex)
            .expect("batch preflight succeeds");
    }

    #[test]
    fn rejects_successful_version_commands_without_version_text() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let command_path = directory.path().join("empty-agent.cmd");
        std::fs::write(&command_path, "@echo off\r\nexit /b 0\r\n")
            .expect("write empty version fixture");
        let resolved =
            resolved_from_existing_path(command_path, AgentKind::Claude).expect("resolve command");

        assert!(probe_command_version(&resolved, directory.path(), AgentKind::Claude).is_err());
    }

    #[test]
    fn explicit_path_with_an_extension_is_not_rewritten() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let requested = directory.path().join("agent.txt");
        std::fs::write(
            directory.path().join("agent.exe"),
            b"not the requested file",
        )
        .expect("write neighboring executable");

        assert!(resolve_candidate_path(&requested, AgentKind::Claude).is_err());
        assert!(!command_candidate_exists(&requested.to_string_lossy(), &[]));
    }

    #[test]
    fn version_probe_times_out_and_terminates_a_batch_process() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let command_path = directory.path().join("slow-agent.cmd");
        std::fs::write(
            &command_path,
            "@echo off\r\nping -n 10 127.0.0.1 >nul\r\necho too-late\r\n",
        )
        .expect("write slow command");
        let resolved =
            resolved_from_existing_path(command_path, AgentKind::Claude).expect("resolve command");
        let started = Instant::now();

        assert!(probe_command_version(&resolved, directory.path(), AgentKind::Claude).is_err());
        assert!(started.elapsed() < Duration::from_secs(7));
    }

    #[test]
    fn version_probe_job_terminates_background_descendants() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let command_path = directory.path().join("background-agent.cmd");
        let marker_path = directory.path().join("orphan-marker.txt");
        let marker_literal = marker_path.to_string_lossy().replace('\'', "''");
        std::fs::write(
            &command_path,
            format!(
                "@echo off\r\nstart \"\" /b powershell.exe -NoLogo -NoProfile -Command \"Start-Sleep -Seconds 5; [IO.File]::WriteAllText('{marker_literal}', 'orphaned')\"\r\necho agent 1.2.3\r\nexit /b 0\r\n"
            ),
        )
        .expect("write background-process fixture");
        let resolved =
            resolved_from_existing_path(command_path, AgentKind::Claude).expect("resolve command");
        let started = Instant::now();

        let _ = probe_command_version(&resolved, directory.path(), AgentKind::Claude);
        assert!(started.elapsed() < Duration::from_secs(7));
        std::thread::sleep(Duration::from_secs(3));

        assert!(
            !marker_path.exists(),
            "the Windows probe job must terminate descendants after the probe"
        );
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    fn builds_a_direct_executable_command_for_unix() {
        let resolved = ResolvedCommand {
            path: PathBuf::from("/opt/codex/bin/codex"),
            codex_no_daemon: true,
        };
        let config = TerminalConfig {
            project_dir: PathBuf::from("/tmp/codex-web-project"),
            command: "ignored".to_owned(),
            arguments: vec!["--yolo".to_owned()],
            agent: AgentKind::Codex,
            shell: ShellKind::Powershell,
        };

        let command = pty_command(&config, &resolved);
        let expected = vec![
            resolved.path.clone().into_os_string(),
            "--yolo".into(),
            "--no-daemon".into(),
        ];

        assert_eq!(
            command.get_argv(),
            &expected,
            "Unix must execute the resolved command without a shell wrapper"
        );
    }

    #[test]
    fn starts_an_executable_directly_in_the_native_pty() {
        let directory = tempfile::Builder::new()
            .prefix("codex web terminal ")
            .tempdir()
            .expect("temp directory");
        let command_path = directory.path().join("fake-codex");
        std::fs::write(
            &command_path,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  [ \"$#\" -eq 1 ] || exit 9\n  echo 'codex-cli 1.0.0'\n  exit 0\nfi\nif [ \"$1\" = \"--help\" ]; then\n  [ \"$#\" -eq 1 ] || exit 9\n  echo '  --no-daemon'\n  exit 0\nfi\n[ \"$#\" -eq 2 ] && [ \"$1\" = \"--yolo\" ] && [ \"$2\" = \"--no-daemon\" ]\n",
        )
        .expect("write fake Codex command");

        let mut permissions = std::fs::metadata(&command_path)
            .expect("fake command metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&command_path, permissions).expect("make fake command executable");

        let config = TerminalConfig {
            project_dir: directory.path().to_path_buf(),
            command: command_path.to_string_lossy().into_owned(),
            arguments: vec!["--yolo".to_owned()],
            agent: AgentKind::Codex,
            shell: ShellKind::Powershell,
        };
        let resolved = preflight(&config).expect("Unix preflight succeeds");
        let mut terminal = spawn_resolved(&config, &resolved).expect("Unix PTY starts");
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = terminal.child.try_wait().expect("poll fake command") {
                break status;
            }
            if Instant::now() >= deadline {
                let kill_result = terminal.child.kill();
                let _ = terminal.child.wait();
                panic!("fake command did not exit within 3 seconds; kill={kill_result:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };

        assert_eq!(status.exit_code(), 0);
    }

    #[test]
    fn rejects_a_non_executable_unix_command() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let command_path = directory.path().join("not-executable");
        std::fs::write(&command_path, "#!/bin/sh\necho agent\n").expect("write command");
        let config = TerminalConfig {
            project_dir: directory.path().to_path_buf(),
            command: command_path.to_string_lossy().into_owned(),
            arguments: Vec::new(),
            agent: AgentKind::Agy,
            shell: ShellKind::Powershell,
        };

        assert!(resolved_from_existing_path(command_path, AgentKind::Agy).is_err());
        assert_eq!(
            inspect_command(&config, false).state,
            CommandInspectionState::Misconfigured
        );
    }

    #[test]
    fn bare_name_skips_a_non_executable_earlier_path_candidate() {
        let first = tempfile::tempdir().expect("first PATH directory");
        let second = tempfile::tempdir().expect("second PATH directory");
        let first_candidate = first.path().join("claude");
        let second_candidate = second.path().join("claude");
        std::fs::write(&first_candidate, "#!/bin/sh\necho blocked\n")
            .expect("write non-executable candidate");
        std::fs::write(&second_candidate, "#!/bin/sh\necho '2.1.220'\n")
            .expect("write executable candidate");
        let mut permissions = std::fs::metadata(&second_candidate)
            .expect("executable candidate metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&second_candidate, permissions)
            .expect("make second candidate executable");

        let resolved = resolve_command_in(
            "claude",
            AgentKind::Claude,
            &[first.path().to_path_buf(), second.path().to_path_buf()],
        )
        .expect("later executable PATH candidate resolves");

        assert_eq!(
            resolved.path(),
            dunce::canonicalize(second_candidate)
                .expect("canonical executable candidate")
                .as_path()
        );
    }

    #[test]
    fn rejects_successful_version_commands_without_version_text() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let command_path = directory.path().join("empty-agent");
        std::fs::write(&command_path, "#!/bin/sh\nexit 0\n").expect("write empty version fixture");
        let mut permissions = std::fs::metadata(&command_path)
            .expect("command metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&command_path, permissions).expect("make command executable");
        let resolved =
            resolved_from_existing_path(command_path, AgentKind::Claude).expect("resolve command");

        assert!(probe_command_version(&resolved, directory.path(), AgentKind::Claude).is_err());
    }

    #[test]
    fn version_probe_times_out_and_terminates_a_unix_process() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let command_path = directory.path().join("slow-agent");
        std::fs::write(&command_path, "#!/bin/sh\nsleep 10\necho too-late\n")
            .expect("write slow command");
        let mut permissions = std::fs::metadata(&command_path)
            .expect("command metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&command_path, permissions).expect("make command executable");
        let resolved =
            resolved_from_existing_path(command_path, AgentKind::Agy).expect("resolve command");
        let started = Instant::now();

        assert!(probe_command_version(&resolved, directory.path(), AgentKind::Agy).is_err());
        assert!(started.elapsed() < Duration::from_secs(7));
    }

    #[test]
    fn background_descendants_cannot_hold_probe_pipes_past_the_deadline() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let command_path = directory.path().join("background-agent");
        std::fs::write(
            &command_path,
            "#!/bin/sh\n(sleep 10) &\necho 'agent 1.2.3'\nexit 0\n",
        )
        .expect("write background command");
        let mut permissions = std::fs::metadata(&command_path)
            .expect("command metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&command_path, permissions).expect("make command executable");
        let resolved =
            resolved_from_existing_path(command_path, AgentKind::Agy).expect("resolve command");
        let started = Instant::now();

        assert!(probe_command_version(&resolved, directory.path(), AgentKind::Agy).is_err());
        assert!(started.elapsed() < Duration::from_secs(7));
    }
}
