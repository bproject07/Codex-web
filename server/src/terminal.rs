use std::{
    env,
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child as ProcessChild, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::os::windows::{io::AsRawHandle, process::CommandExt};

use anyhow::{Context, Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};

use crate::{
    config::{AgentKind, ShellKind},
    filesystem::validate_canonical_readable_directory,
    peer::{
        CWT_PEER_CAPABILITY_ENV, CWT_PEER_ENDPOINT_ENV, CWT_PEER_HELPER_ENV, CWT_SESSION_ID_ENV,
        CWT_TERMINAL_ID_ENV,
    },
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
const VERSION_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    let resolved = resolve_command(&config.command, config.agent)?;
    probe_command_version(&resolved, &config.project_dir, config.agent)?;
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
    #[cfg(windows)]
    let mut command = if resolved.is_batch_file || config.shell == ShellKind::Cmd {
        let mut command = CommandBuilder::new("cmd.exe");
        command.args(["/d", "/s", "/c", "call"]);
        command.arg(&resolved.path);
        command.args(&config.arguments);
        command
    } else {
        let mut command = CommandBuilder::new("powershell.exe");
        command.args(["-NoLogo", "-NoProfile", "-Command"]);
        command.arg(powershell_invocation(&resolved.path, &config.arguments));
        command
    };

    #[cfg(not(windows))]
    let mut command = {
        let mut command = CommandBuilder::new(&resolved.path);
        command.args(&config.arguments);
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
        #[cfg(windows)]
        is_batch_file,
    })
}

fn probe_command_version(
    resolved: &ResolvedCommand,
    project_dir: &Path,
    agent: AgentKind,
) -> Result<String> {
    #[cfg(windows)]
    let mut command = if resolved.is_batch_file {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/s", "/c", "call"]);
        command.arg(&resolved.path);
        command.arg("--version");
        command
    } else {
        let mut command = Command::new(&resolved.path);
        command.arg("--version");
        command
    };

    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(&resolved.path);
        command.arg("--version");
        command
    };

    configure_version_probe_environment(&mut command, agent);
    command
        .current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    command.creation_flags(CREATE_SUSPENDED);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        command.process_group(0);
    }

    let process_owner = ProbeProcessOwner::new()
        .context("failed to create a bounded version-probe process group")?;
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run `{} --version`", agent.label()))?;
    if let Err(error) = process_owner.attach_and_start(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error).context("failed to contain the version-probe process");
    }

    let stdout = child
        .stdout
        .take()
        .context("failed to capture version command stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture version command stderr")?;
    let stdout_receiver = spawn_bounded_output_reader(stdout);
    let stderr_receiver = spawn_bounded_output_reader(stderr);

    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("failed to wait for `{} --version`", agent.label()))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            process_owner.terminate(&mut child);
            let _ = child.wait();
            bail!("`{} --version` timed out", agent.label());
        }
        thread::sleep(VERSION_PROBE_POLL_INTERVAL);
    };

    let stdout = receive_bounded_output(&stdout_receiver, deadline);
    let stderr = receive_bounded_output(&stderr_receiver, deadline);
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        // A version command that exits while leaving descendants attached to
        // its output streams must not bypass the overall probe deadline.
        process_owner.terminate(&mut child);
        bail!(
            "`{} --version` did not close its output streams",
            agent.label()
        );
    };

    // `--version` must be side-effect free. Clean up any descendant that may
    // have detached after closing the inherited output streams.
    process_owner.terminate(&mut child);

    if !status.success() {
        #[cfg(windows)]
        bail!(
            "`{} --version` failed with status {}. Verify the CLI installation and PowerShell execution policy.",
            agent.label(),
            status,
        );

        #[cfg(not(windows))]
        bail!(
            "`{} --version` failed with status {}. Verify the CLI installation and executable permissions.",
            agent.label(),
            status,
        );
    }

    sanitized_version(&stdout)
        .or_else(|| sanitized_version(&stderr))
        .with_context(|| format!("`{} --version` returned no version text", agent.label()))
}

fn configure_version_probe_environment(command: &mut Command, agent: AgentKind) {
    command
        .env_remove(CODEX_THREAD_ID_ENV)
        .env_remove(CLAUDE_NESTING_ENV)
        .env_remove(CODEX_WEB_TOKEN_ENV);
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

fn read_bounded_output(mut reader: impl Read) -> Vec<u8> {
    let mut retained = Vec::with_capacity(VERSION_OUTPUT_LIMIT);
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(length) => {
                let remaining = VERSION_OUTPUT_LIMIT.saturating_sub(retained.len());
                retained.extend_from_slice(&buffer[..length.min(remaining)]);
            }
        }
    }
    retained
}

fn spawn_bounded_output_reader(reader: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = sender.send(read_bounded_output(reader));
    });
    receiver
}

fn receive_bounded_output(receiver: &Receiver<Vec<u8>>, deadline: Instant) -> Option<Vec<u8>> {
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
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

struct ProbeProcessOwner {
    #[cfg(windows)]
    job: HANDLE,
}

impl ProbeProcessOwner {
    fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            // SAFETY: null security attributes and name request a private job
            // object with default ACLs. The returned handle is owned by Self.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(std::io::Error::last_os_error()).context("CreateJobObjectW failed");
            }

            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` has the exact layout and byte length required by
            // JobObjectExtendedLimitInformation, and `job` is a valid handle.
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&limits).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            };
            if configured == 0 {
                let error = std::io::Error::last_os_error();
                // SAFETY: this branch still owns the valid job handle.
                unsafe {
                    CloseHandle(job);
                }
                return Err(error).context("SetInformationJobObject failed");
            }

            Ok(Self { job })
        }

        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    fn attach_and_start(&self, child: &ProcessChild) -> Result<()> {
        #[cfg(windows)]
        {
            // SAFETY: both handles are valid for the duration of this call.
            let assigned = unsafe {
                AssignProcessToJobObject(self.job, AsRawHandle::as_raw_handle(child) as HANDLE)
            };
            if assigned == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("AssignProcessToJobObject failed");
            }
            resume_suspended_process(child.id())?;
        }

        #[cfg(not(windows))]
        let _ = child;

        Ok(())
    }

    fn terminate(&self, child: &mut ProcessChild) {
        #[cfg(windows)]
        {
            // SAFETY: Self owns `job`. Terminating the job is bounded and
            // includes descendants even after their direct parent exits.
            let _ = unsafe { TerminateJobObject(self.job, 1) };
        }

        #[cfg(unix)]
        {
            // The version command starts in its own process group. A negative
            // PID targets that exact group, including descendants.
            if let Ok(process_group) = i32::try_from(child.id()) {
                // SAFETY: kill receives the exact negative process-group
                // identifier assigned immediately before spawn and a constant
                // signal. Errors are handled by the direct child fallback.
                let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
            }
        }

        let _ = child.kill();
    }
}

#[cfg(windows)]
fn resume_suspended_process(process_id: u32) -> Result<()> {
    // SAFETY: the snapshot handle is checked before use and closed on every
    // path below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .context("CreateToolhelp32Snapshot failed while resuming probe");
    }

    let result = (|| {
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        // A CREATE_SUSPENDED process has exactly its primary thread at this
        // point; it cannot execute or create descendants until this succeeds.
        let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while has_entry {
            if entry.th32OwnerProcessID == process_id {
                // SAFETY: the enumerated thread ID belongs to the suspended
                // process and the returned handle is checked before use.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error())
                        .context("OpenThread failed while resuming probe");
                }
                // SAFETY: `thread` grants THREAD_SUSPEND_RESUME and is valid.
                let previous_count = unsafe { ResumeThread(thread) };
                let resume_error = (previous_count == u32::MAX).then(std::io::Error::last_os_error);
                // SAFETY: this scope owns the thread handle.
                unsafe {
                    CloseHandle(thread);
                }
                if let Some(error) = resume_error {
                    return Err(error).context("ResumeThread failed for version probe");
                }
                return Ok(());
            }
            // SAFETY: snapshot and entry remain valid for enumeration.
            has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }

        bail!("suspended version-probe primary thread was not found")
    })();

    // SAFETY: this function owns the valid snapshot handle.
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

#[cfg(windows)]
impl Drop for ProbeProcessOwner {
    fn drop(&mut self) {
        // Closing a kill-on-close job is the final fail-safe for all probe
        // descendants, including processes that retained captured pipe handles.
        // SAFETY: Self owns the handle and closes it exactly once.
        unsafe {
            CloseHandle(self.job);
        }
    }
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
    fn child_agents_do_not_inherit_parent_session_markers() {
        let mut command = CommandBuilder::new("codex");
        command.env(CODEX_THREAD_ID_ENV, "parent-thread");
        command.env(CODEX_WEB_TOKEN_ENV, "server-bearer-token");
        for name in PEER_ENVIRONMENT_NAMES {
            command.env(name, "stale-peer-value");
        }

        remove_parent_agent_markers(&mut command);
        remove_inherited_peer_environment(&mut command);
        remove_server_secret_environment(&mut command);

        assert_eq!(command.get_env(CODEX_THREAD_ID_ENV), None);
        assert_eq!(command.get_env(CLAUDE_NESTING_ENV), None);
        assert_eq!(command.get_env(CODEX_WEB_TOKEN_ENV), None);
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
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn passes_fixed_arguments_to_an_executable_through_powershell() {
        let resolved = ResolvedCommand {
            path: PathBuf::from(r"C:\Program Files\Claude\claude.exe"),
            is_batch_file: false,
        };
        let config = TerminalConfig {
            project_dir: PathBuf::from(r"C:\project"),
            command: "ignored".to_owned(),
            arguments: vec!["--dangerously-skip-permissions".to_owned()],
            agent: AgentKind::Claude,
            shell: ShellKind::Powershell,
        };

        let command = pty_command(&config, &resolved);
        let expected: Vec<OsString> = vec![
            "powershell.exe".into(),
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-Command".into(),
            "& 'C:\\Program Files\\Claude\\claude.exe' '--dangerously-skip-permissions'; exit $LASTEXITCODE"
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
    fn verifies_a_batch_codex_entry_point_with_spaces_in_its_path() {
        let directory = tempfile::Builder::new()
            .prefix("codex web terminal ")
            .tempdir()
            .expect("temp directory");
        let command_path = directory.path().join("codex.cmd");
        std::fs::write(
            &command_path,
            "@echo off\r\necho codex-cli 1.0.0\r\nexit /b 0\r\n",
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
        };
        let config = TerminalConfig {
            project_dir: PathBuf::from("/tmp/codex-web-project"),
            command: "ignored".to_owned(),
            arguments: vec!["--dangerously-skip-permissions".to_owned()],
            agent: AgentKind::Codex,
            shell: ShellKind::Powershell,
        };

        let command = pty_command(&config, &resolved);
        let expected = vec![
            resolved.path.clone().into_os_string(),
            "--dangerously-skip-permissions".into(),
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
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'codex-cli 1.0.0'\n  exit 0\nfi\n[ \"$1\" = \"--dangerously-skip-permissions\" ]\n",
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
            arguments: vec!["--dangerously-skip-permissions".to_owned()],
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
