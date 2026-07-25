use std::{
    env,
    ffi::OsStr,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::config::ShellKind;

pub const INITIAL_COLS: u16 = 120;
pub const INITIAL_ROWS: u16 = 35;

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
        .context("failed to create a pseudo-terminal (ConPTY on Windows)")?;

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

    command.cwd(&config.project_dir);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command
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

    bail!(
        "Codex CLI was not found. Install it, make sure codex.exe or codex.cmd is in PATH, then run `codex --version`."
    )
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
    let is_batch_file = canonical_path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"));

    Ok(ResolvedCodex {
        path: canonical_path,
        is_batch_file,
    })
}

fn verify_codex_version(resolved: &ResolvedCodex, project_dir: &Path) -> Result<()> {
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

    let output = command
        .current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run `codex --version`")?;

    if !output.status.success() {
        bail!(
            "`codex --version` failed with status {}. Verify the Codex CLI installation and PowerShell execution policy.",
            output.status
        );
    }

    Ok(())
}

fn powershell_invocation(path: &Path) -> String {
    let escaped = path.to_string_lossy().replace('\'', "''");
    format!("& '{escaped}'; exit $LASTEXITCODE")
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
