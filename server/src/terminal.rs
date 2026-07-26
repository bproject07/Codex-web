use std::{
    env,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[cfg(windows)]
use std::ffi::OsStr;

use anyhow::{Context, Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::config::ShellKind;

pub const INITIAL_COLS: u16 = 120;
pub const INITIAL_ROWS: u16 = 35;
const CODEX_THREAD_ID_ENV: &str = "CODEX_THREAD_ID";

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub project_dir: PathBuf,
    pub command: String,
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
pub struct ResolvedCodex {
    path: PathBuf,
    #[cfg(windows)]
    is_batch_file: bool,
}

impl ResolvedCodex {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn preflight(config: &TerminalConfig) -> Result<ResolvedCodex> {
    let resolved = resolve_codex(&config.command)?;
    verify_codex_version(&resolved, &config.project_dir)?;
    Ok(resolved)
}

pub fn spawn_resolved(
    config: &TerminalConfig,
    resolved: &ResolvedCodex,
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

    let command = pty_command(config, resolved);
    let child = pair
        .slave
        .spawn_command(command)
        .context("failed to start Codex in the pseudo-terminal")?;
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

fn pty_command(config: &TerminalConfig, resolved: &ResolvedCodex) -> CommandBuilder {
    #[cfg(windows)]
    let mut command = if resolved.is_batch_file || config.shell == ShellKind::Cmd {
        let mut command = CommandBuilder::new("cmd.exe");
        command.args(["/d", "/s", "/c", "call"]);
        command.arg(&resolved.path);
        command
    } else {
        let mut command = CommandBuilder::new("powershell.exe");
        command.args(["-NoLogo", "-NoProfile", "-Command"]);
        command.arg(powershell_invocation(&resolved.path));
        command
    };

    #[cfg(not(windows))]
    let mut command = CommandBuilder::new(&resolved.path);

    command.cwd(&config.project_dir);
    remove_parent_codex_thread(&mut command);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command
}

fn remove_parent_codex_thread(command: &mut CommandBuilder) {
    command.env_remove(CODEX_THREAD_ID_ENV);
}

fn resolve_codex(command: &str) -> Result<ResolvedCodex> {
    let requested = Path::new(command);
    let contains_path_separator = command.contains(['\\', '/']);

    if requested.is_absolute() || contains_path_separator {
        return resolve_candidate_path(requested);
    }

    #[cfg(windows)]
    let extensions = ["exe", "cmd", ""];
    #[cfg(not(windows))]
    let extensions = [""];

    let search_directories: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();

    // Extension is the outer loop intentionally: codex.exe is preferred over
    // codex.cmd across PATH, as required for predictable Windows startup.
    for extension in extensions {
        let file_name = if extension.is_empty() || requested.extension().is_some() {
            command.to_owned()
        } else {
            format!("{command}.{extension}")
        };

        for directory in &search_directories {
            let candidate = directory.join(&file_name);
            if candidate.is_file() {
                return resolved_from_existing_path(candidate);
            }
        }

        if requested.extension().is_some() {
            break;
        }
    }

    #[cfg(windows)]
    bail!(
        "Codex CLI was not found. Install it, make sure codex.exe or codex.cmd is in PATH, then run `codex --version`."
    );

    #[cfg(not(windows))]
    bail!(
        "Codex CLI was not found. Install it, make sure the executable is in PATH, then run `codex --version`."
    );
}

fn resolve_candidate_path(path: &Path) -> Result<ResolvedCodex> {
    if path.is_file() {
        return resolved_from_existing_path(path.to_path_buf());
    }

    #[cfg(windows)]
    for extension in ["exe", "cmd"] {
        let candidate = path.with_extension(extension);
        if candidate.is_file() {
            return resolved_from_existing_path(candidate);
        }
    }

    bail!(
        "Codex command does not exist or is not a file: {}",
        path.display()
    )
}

fn resolved_from_existing_path(path: PathBuf) -> Result<ResolvedCodex> {
    let canonical_path = dunce::canonicalize(&path)
        .with_context(|| format!("failed to resolve Codex command: {}", path.display()))?;

    #[cfg(windows)]
    let is_batch_file = canonical_path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"));

    Ok(ResolvedCodex {
        path: canonical_path,
        #[cfg(windows)]
        is_batch_file,
    })
}

fn verify_codex_version(resolved: &ResolvedCodex, project_dir: &Path) -> Result<()> {
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

    let output = command
        .current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run `codex --version`")?;

    if !output.status.success() {
        #[cfg(windows)]
        bail!(
            "`codex --version` failed with status {}. Verify the Codex CLI installation and PowerShell execution policy.",
            output.status
        );

        #[cfg(not(windows))]
        bail!(
            "`codex --version` failed with status {}. Verify the Codex CLI installation and executable permissions.",
            output.status
        );
    }

    Ok(())
}

#[cfg(windows)]
fn powershell_invocation(path: &Path) -> String {
    let escaped = path.to_string_lossy().replace('\'', "''");
    format!("& '{escaped}'; exit $LASTEXITCODE")
}

#[cfg(test)]
mod command_tests {
    use super::*;

    #[test]
    fn child_codex_does_not_inherit_the_parent_thread() {
        let mut command = CommandBuilder::new("codex");
        command.env(CODEX_THREAD_ID_ENV, "parent-thread");

        remove_parent_codex_thread(&mut command);

        assert_eq!(command.get_env(CODEX_THREAD_ID_ENV), None);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

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
        let resolved = resolved_from_existing_path(command_path).expect("resolve command");

        verify_codex_version(&resolved, directory.path()).expect("batch preflight succeeds");
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
        let resolved = ResolvedCodex {
            path: PathBuf::from("/opt/codex/bin/codex"),
        };
        let config = TerminalConfig {
            project_dir: PathBuf::from("/tmp/codex-web-project"),
            command: "ignored".to_owned(),
            shell: ShellKind::Powershell,
        };

        let command = pty_command(&config, &resolved);
        let expected = vec![resolved.path.clone().into_os_string()];

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
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'codex-cli 1.0.0'\nfi\nexit 0\n",
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
}
