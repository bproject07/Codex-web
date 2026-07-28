# Operating Codex Web Terminal

This guide explains how to install, configure, start, use, monitor, stop,
upgrade, and troubleshoot Codex Web Terminal from a verified release archive
or a local source build.

See [BUILDING.md](BUILDING.md) for compilation and packaging. See
[README.md](README.md) for architecture, protocol, and API details.

## Security model

Codex Web Terminal is remote terminal access. It runs with the permissions and
environment of the operating-system user that starts it. A browser holding the
authentication token can:

- type into the selected agent terminal;
- create, attach to, restart, or terminate managed sessions;
- list server filesystem roots, browse readable directories, and resolve an
  absolute server path;
- read server-wide Favorites/Recent and add or remove Favorites; successful
  launches update Recent;
- launch an agent in any directory readable by the server account;
- respond to approval prompts;
- cause an agent to read or modify files allowed to the server user.

Treat the authenticated URL as a credential.

Safe defaults:

- bind to `127.0.0.1`;
- use a newly generated token;
- use Tailscale for access from another device;
- restrict the Tailscale ACL to intended users and devices;
- never expose the port directly to the public Internet;
- never commit or log the token.

## Runtime prerequisites

Before starting the server, verify the primary CLI as the same
operating-system user that will run `codex-web`:

```text
codex --version
codex login
```

Claude Code and AGY are optional. When installed, verify them with:

```text
claude --version
agy --version
```

The server performs the same read-only version probes during agent discovery.
It disables each provider's documented automatic updater only for that probe,
removes parent-agent nesting markers, enforces a three-second deadline, and
publishes only a strictly validated semantic version. On Windows the process
is created suspended, assigned to a kill-on-close Job Object, and then resumed;
on Unix it runs in a dedicated process group. Descendants therefore cannot
outlive a failed or completed probe. Normal interactive agent sessions keep
their usual updater behavior. The server does not copy or manage agent
authentication. Spawned processes inherit the current user's environment and
use that user's existing CLI configuration and credentials.

Choose the default project directory deliberately:

```text
--project /absolute/path/to/project
```

The backend canonicalizes this path, verifies that it is a readable directory,
and starts the primary terminal there. It is also the fallback for a
new-session API request that omits `directoryId`.

`--project` is not a filesystem sandbox or allowlist. An authenticated browser
can use **+ New** to select another absolute directory readable by the
operating-system account running the server. The selected directory applies
only to that new managed terminal. The server canonicalizes and checks it
again at browse and launch time.

### Installing or updating an agent CLI

Installation and updates happen on the **server host**, not on the phone,
laptop, or browser used to view the terminal. The **New** dialog reports each
agent as `ready`, `missing`, or `misconfigured`, displays its installed version
when available, and provides a platform-specific manual command. After running
that command in a trusted host terminal, select **Refresh** or **Check again**
to repeat detection.

Official native installation commands:

| CLI | Windows PowerShell | Linux/macOS |
| --- | --- | --- |
| Codex | `powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 \| iex"` | `curl -fsSL https://chatgpt.com/codex/install.sh \| sh` |
| Claude Code | `irm https://claude.ai/install.ps1 \| iex` | `curl -fsSL https://claude.ai/install.sh \| bash` |
| AGY | `irm https://antigravity.google/cli/install.ps1 \| iex` | `curl -fsSL https://antigravity.google/cli/install.sh \| bash` |

Official update commands:

| CLI | Native installation | Package-manager note |
| --- | --- | --- |
| Codex | `codex update` | For an npm installation: `npm install --global @openai/codex@latest` |
| Claude Code | `claude update` | WinGet: `winget upgrade Anthropic.ClaudeCode`; Homebrew: `brew upgrade claude-code` |
| AGY | Re-run the platform install command; it installs or upgrades | AGY also checks for background updates unless `AGY_CLI_DISABLE_AUTO_UPDATE=true` |

Verify the current upstream instructions before executing a downloaded script:
[Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[Claude Code](https://code.claude.com/docs/en/setup), and
[Antigravity CLI](https://antigravity.google/docs/cli/install).

There is deliberately no silent browser-side Install button and no install
API. A browser session has full terminal input authority already; allowing it
to install or replace host executables would create an additional supply-chain
and privilege boundary. Downloaded scripts may change, invoke package
managers, modify `PATH`, prompt for authentication, or require local policy
review. The operator must review and run them explicitly. **Refresh** and
**Check again** perform only fixed executable discovery and bounded
`--version` probes.

## Install a tagged release

Download the Windows or Linux archive and `SHA256SUMS.txt` from the official
[GitHub Releases](https://github.com/bproject07/Codex-web/releases) page. A
release archive is accepted only after Windows/Linux tests, the packaged
`@cwt` regression, and target-specific third-party license generation pass.
Do not use an archive copied from a local `dist` directory or an unofficial
mirror.

Verify the archive before extraction. On Linux:

```bash
grep -F '  codex-web-terminal-vX.Y.Z-linux-x86_64-glibc.tar.gz' \
  SHA256SUMS.txt | sha256sum -c -
gh attestation verify \
  --repo bproject07/Codex-web \
  --signer-workflow bproject07/Codex-web/.github/workflows/release.yml \
  codex-web-terminal-vX.Y.Z-linux-x86_64-glibc.tar.gz
```

On Windows, compare `Get-FileHash -Algorithm SHA256 <archive.zip>` with the
matching line in `SHA256SUMS.txt`, then run:

```powershell
gh attestation verify `
  --repo bproject07/Codex-web `
  --signer-workflow bproject07/Codex-web/.github/workflows/release.yml `
  .\codex-web-terminal-vX.Y.Z-windows-x86_64.zip
```

Extract the whole versioned directory. Keep `web` and
`THIRD_PARTY_LICENSES` beside the executable. Windows packages are not
Authenticode-signed yet and may trigger SmartScreen's unknown-publisher
warning; checksum and provenance verification are required before choosing to
run an unsigned archive. The Linux artifact is built on Ubuntu 22.04 for
x86_64 glibc 2.35 or newer; it is not a musl or universal Linux build.

## First local start on Windows

For a source checkout, build first:

```powershell
.\scripts\build.ps1
```

Start on loopback with an automatically generated token:

```powershell
.\scripts\run.ps1 `
  -Project "C:\Projects\my-app" `
  -ListenHost "127.0.0.1" `
  -Port 8787
```

The server prints an authenticated URL. Open the complete URL in the browser.
The frontend moves the token into the current tab's `sessionStorage` and
removes it from the visible address bar.

## First local start on Linux

For a source checkout, build first:

```bash
./scripts/build.sh
```

Start on loopback:

```bash
./scripts/run.sh "/home/user/projects/my-app" \
  --host 127.0.0.1 \
  --port 8787
```

Or run the package directly:

```bash
./dist-linux/codex-web \
  --project "/home/user/projects/my-app" \
  --host 127.0.0.1 \
  --port 8787
```

## Authentication token lifecycle

If `--token` and `CODEX_WEB_TOKEN` are both omitted, the server generates a
strong ephemeral token. It changes on every server restart.

Use an explicit token when a bookmark, service, or planned reconnect must keep
working across restarts. Tokens must:

- contain at least 16 characters;
- contain no whitespace;
- remain at or below 512 bytes.

Generate a URL-safe 256-bit token on Windows PowerShell:

```powershell
$tokenBytes = New-Object byte[] 32
$generator = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$generator.GetBytes($tokenBytes)
$generator.Dispose()
$env:CODEX_WEB_TOKEN = [Convert]::ToBase64String($tokenBytes).
  TrimEnd("=").
  Replace("+", "-").
  Replace("/", "_")
```

Generate one on Linux:

```bash
export CODEX_WEB_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
```

Prefer an environment variable over a command-line token on multi-user
machines because process command lines can be visible to other local users.
For a service, store the token in a permission-restricted environment file.

The browser stores the token only in the current tab's `sessionStorage`.
Closing that tab or choosing **Forget token** removes the browser copy. It does
not stop the server or change the server-side token.

## Workspace state and backup

The folder picker's Favorites and Recent entries are host- and
operating-system-account-local, server-wide state. They are shared by every
browser that authenticates to this server and are stored in:

```text
<state-directory>/workspaces.json
```

Select the state directory with `--state-dir` or
`CODEX_WEB_STATE_DIR`. Defaults:

```text
Windows: %LOCALAPPDATA%\codex-web-terminal
         or %USERPROFILE%\AppData\Local\codex-web-terminal
Unix:   $XDG_STATE_HOME/codex-web-terminal
         or $HOME/.local/state/codex-web-terminal
```

Windows uses `LOCALAPPDATA` when it exists; the `USERPROFILE` path is used only
when it does not. On Unix,
`XDG_STATE_HOME` must be absolute or the `$HOME/.local/state` fallback is
used. A relative explicit value is resolved against the working directory
from which the server starts.

For `run.ps1`, set the environment variable:

```powershell
$env:CODEX_WEB_STATE_DIR = Join-Path $env:LOCALAPPDATA "codex-web-terminal-instance-1"
.\scripts\run.ps1 -Project "C:\Projects\my-app"
```

`run.sh` also accepts the backend option after its project argument:

```bash
./scripts/run.sh "/srv/projects/default" \
  --state-dir "$HOME/.local/state/codex-web-terminal-instance-1"
```

The file uses schema version 1, is limited to 32 MiB (33,554,432 bytes) on
both read and write, and stores at most 100 Favorites and 30 Recent folders.
Recent is deduplicated by native directory, ordered newest first, and records
the actual agent used. A successful primary startup, **New** launch, or
restart updates Recent; it also updates the preferred agent of an existing
Favorite for that directory. If this post-launch save fails, the PTY remains
live and the server logs a warning. A Favorite mutation that would serialize
beyond the limit is rejected with HTTP 507 before the current file or
in-memory state is replaced.

Writes use a new temporary file in the same directory, flush it, and
atomically replace `workspaces.json`. Unix also syncs the parent directory and
creates a missing final state directory with mode `0700` and new state files
with `0600`. An existing Unix directory or file must already be owned by the
effective server user and grant no group/other permissions. The server rejects
unsafe existing targets instead of changing their mode. On Windows, protect
the chosen directory with an ACL appropriate for the service account.

The state location must be a dedicated real directory. Filesystem roots,
known broad account/system locations, the current or system temporary
directory, symlinks, and Windows reparse points are rejected. The existing
`workspaces.json`, when present, must be a regular non-link file. An unsafe
location prevents server startup rather than being silently repaired.
The store coordinates writers only inside one process; it has no cross-process
lock or merge. Assign a distinct `--state-dir` to every server instance that
can run concurrently. Two instances sharing one file can overwrite each
other's newer Favorites or Recent updates.

At startup, malformed JSON, an unsupported version, invalid records, or a file
larger than 32 MiB is renamed to
`workspaces.corrupt.<uuid>.json`. The server logs a warning and continues with
a clean schema-1 library; normal successful primary startup may immediately
add the default folder to Recent. The quarantined bytes are preserved and are
not overwritten. Do not publish that file because it contains filesystem
paths and usage history.

For a consistent backup, stop the server and copy `workspaces.json` together
with any quarantined files you intend to retain. Restore only a reviewed
schema-1 file while the server is stopped. Ensure the service account owns the
directory and can create, replace, and rename files inside it. Both display
paths and reversible native path IDs can reveal filesystem layout, so protect
backups with the same care as the live file.

Saved paths are not continuously monitored. A renamed, deleted, or newly
restricted folder may remain visible in Favorites or Recent. Opening,
updating, or launching from it performs a fresh canonicalization and read
check; a missing path returns `404`, and an inaccessible path returns `403`.
Restart performs the same launch-time validation before terminating the
running PTY, and also rejects a path that now resolves through a symlink or
junction to a different canonical directory. Remove the stale Favorite or
browse to the new location.

## Command-line options

| Option | Default | Meaning |
| --- | --- | --- |
| `--host` | `127.0.0.1` | Address on which the HTTP server listens |
| `--port` | `8787` | TCP port |
| `--max-sessions` | `20` | Managed capacity from `1` through `256`, including the primary entry, stopped entries, and dedicated `@cwt` reviewers |
| `--project` | current directory | Default working directory for the primary PTY and new sessions without a selected folder |
| `--state-dir` | per-user OS state directory | Dedicated directory containing `workspaces.json` Favorites/Recent state |
| `--command` | derived | Explicit primary executable override; otherwise follows `--primary-agent` |
| `--primary-agent` | `codex` | Agent represented by `--command`: `codex`, `claude`, or `agy` |
| `--new-session-command` | resolved primary command | Optional executable used when **New** starts the primary agent |
| `--codex-command` | unset | Explicit Codex CLI executable override |
| `--claude-command` | unset | Explicit Claude Code executable override |
| `--claude-dangerously-skip-permissions` | off | Start Claude with permission checks bypassed |
| `--agy-command` | unset | Explicit AGY executable override |
| `--agy-dangerously-skip-permissions` | off | Start AGY with tool permission requests auto-approved |
| `--no-agent-auto-detect` | off | Disable discovery of optional agent CLIs |
| `--shell` | `powershell` | Windows wrapper (`powershell` or `cmd`); ignored on Unix |
| `--token` | generated | Explicit authentication token |
| `--no-open-browser` | off | Prevent automatic browser launch |
| `--log-level` | `info` | Rust `tracing` filter |

Equivalent environment variables:

```text
CODEX_WEB_HOST
CODEX_WEB_PORT
CODEX_WEB_MAX_SESSIONS
CODEX_WEB_PROJECT_DIR
CODEX_WEB_STATE_DIR
CODEX_WEB_COMMAND
CODEX_WEB_PRIMARY_AGENT
CODEX_WEB_NEW_SESSION_COMMAND
CODEX_WEB_CODEX_COMMAND
CODEX_WEB_CLAUDE_COMMAND
CODEX_WEB_CLAUDE_DANGEROUSLY_SKIP_PERMISSIONS
CODEX_WEB_AGY_COMMAND
CODEX_WEB_AGY_DANGEROUSLY_SKIP_PERMISSIONS
CODEX_WEB_NO_AGENT_AUTO_DETECT
CODEX_WEB_SHELL
CODEX_WEB_TOKEN
CODEX_WEB_LOG_LEVEL
```

Command-line arguments override environment variables.

`scripts/run.ps1` exposes the same setting as `-MaxSessions`; when that
parameter is omitted it leaves `CODEX_WEB_MAX_SESSIONS` available to the
server. `run.sh` forwards `--max-sessions` after its project argument. Raising
the capacity increases both process and memory exposure: every running slot
may own a full agent CLI process, and every managed entry may retain a 16 MiB
output buffer.

The command values are executable names or paths, not shell expressions. Do
not pass pipes, redirections, command substitutions, or chained commands.

If the primary terminal is launched by a trusted wrapper that resumes one
specific Codex thread, also set `--new-session-command codex`. The primary
terminal will use the resume wrapper, while the **New** button will start an
independent Codex CLI process when the primary Codex card is selected. Other
agent cards use their own override or default command.

### Agent discovery and explicit profiles

By default, the server probes the executable implied by `--primary-agent` and
auto-detects `codex`, `claude`, and `agy` from `PATH` and their documented
per-user locations. It runs a fixed `--version` probe with bounded output and a
timeout. A ready optional agent is offered in **New**. Missing and
misconfigured agents remain visible in the catalog with manual installation
or repair guidance, but cannot create sessions.

An explicit `--command`, `--codex-command`, `--claude-command`, or
`--agy-command` is authoritative. If it does not resolve or its `--version`
probe fails, the profile is `misconfigured`; the server does not silently fall
back to another binary. `--no-agent-auto-detect` (or
`CODEX_WEB_NO_AGENT_AUTO_DETECT=true`) restricts optional profiles to explicit
configuration. The primary profile is still validated.

The browser cannot provide executable paths, arguments, URLs, or shell syntax.
For a service whose `PATH` differs from an interactive shell, configure
trusted absolute paths:

```powershell
.\scripts\run.ps1 `
  -Project "C:\Projects\my-app" `
  -CodexCommand "$env:APPDATA\npm\codex.cmd" `
  -ClaudeCommand "$HOME\.local\bin\claude.exe" `
  -AgyCommand "$env:LOCALAPPDATA\agy\bin\agy.exe"
```

Each managed terminal is a fresh agent-session boundary. The launcher removes
only the inherited nesting markers `CODEX_THREAD_ID` and `CLAUDECODE` before
version checks and PTY startup; authentication and provider environment
variables remain untouched.

To deliberately auto-approve every tool action for both optional profiles:

```powershell
.\scripts\run.ps1 `
  -Project "C:\Projects\my-app" `
  -ClaudeCommand "$HOME\.local\bin\claude.exe" `
  -ClaudeDangerouslySkipPermissions `
  -AgyCommand "$env:LOCALAPPDATA\agy\bin\agy.exe" `
  -AgyDangerouslySkipPermissions
```

The equivalent direct launches are
`claude --dangerously-skip-permissions` and
`agy --dangerously-skip-permissions`. These modes remove the normal approval
barrier for file changes, commands, network access, and other supported tools.
Use them only when the operating-system account, every selectable working
directory, network, credentials, and reachable services are intentionally
placed inside the agent's trust boundary. The switches are off by default.

## Windows command resolution

For a command such as `codex`, `claude`, or `agy`, Windows searches `PATH` and
the documented per-user locations in this extension preference:

1. `<command>.exe`
2. `<command>.cmd`
3. an exact extension already supplied by the caller

`.cmd` entry points are invoked through `cmd.exe /d /s /c call`.
Executable entry points normally use PowerShell unless `--shell cmd` is
selected. `.ps1` shims are intentionally not selected automatically because a
PowerShell execution policy can block npm-generated `.ps1` shims.

## Linux and Unix command resolution

Unix searches the configured `PATH` for the exact executable name and checks
the documented per-user locations during auto-detection. A trusted absolute
path can also be supplied:

```bash
--command /usr/bin/codex
```

After the selected CLI's `--version` succeeds, the resolved executable is
started directly inside the Unix PTY without a shell wrapper.

## Access over Tailscale

Install and connect Tailscale on both the server and browser devices:

```bash
tailscale status
tailscale ip -4
```

The safest direct bind is the server's specific Tailscale IP:

```bash
export CODEX_WEB_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"

./dist-linux/codex-web \
  --project "/home/user/projects/my-app" \
  --host "100.x.y.z" \
  --port 8787 \
  --no-open-browser
```

On Windows, use the equivalent `-ListenHost` option:

```powershell
.\scripts\run.ps1 `
  -Project "C:\Projects\my-app" `
  -ListenHost "100.x.y.z" `
  -Port 8787 `
  -NoOpenBrowser
```

`run.ps1` and the server inherit `CODEX_WEB_TOKEN`; omitting `-Token` keeps the
credential out of the process command line.

Open:

```text
http://100.x.y.z:8787/?token=YOUR_TOKEN
```

Binding to `0.0.0.0` also works, but exposes the port on every IPv4 interface.
Use it only when firewall rules and the surrounding network are understood:

```bash
./dist-linux/codex-web \
  --project "/home/user/projects/my-app" \
  --host 0.0.0.0 \
  --port 8787 \
  --no-open-browser
```

Tailscale provides private transport, but the application URL is still HTTP
unless an HTTPS reverse proxy is added. Restrict access with Tailscale ACLs and
never add a public router port-forward.

### Reverse proxy origin requirement

A backend bound to `127.0.0.1` accepts only loopback browser origins. Therefore,
an HTTPS reverse proxy serving a non-loopback hostname must use a backend bound
to either:

- the exact private or Tailscale address reachable by the proxy; or
- `0.0.0.0`, protected by a host firewall that admits only the intended
  interface and proxy.

Preserve the browser-facing `Host` header and port. Forward WebSocket Upgrade
and Connection headers and binary frames without conversion. Otherwise the
strict WebSocket Origin check rejects the connection.

### Sharing the current server

The current token is server-wide, not per-user or per-session. Anyone who
receives it has the same ability as the owner to list, view, type into, create,
restart, terminate, and remove eligible managed sessions. It also authorizes
filesystem-root discovery, directory browsing, manual absolute-path
resolution, Favorites/Recent changes, and launching agents anywhere readable
by the server account. The opaque directory IDs used by the API are transport
values, not additional access control.

Multiple browsers attached to one session share both input and PTY dimensions.
The latest valid resize wins, so desktop and mobile clients with different
viewport sizes can trigger redraw or scroll changes for each other.

Share the authenticated URL only when full read/write terminal access is
intended. Read-only links, expiring share links, per-session grants, and
revocation without rotating the server token are future work described in
[TODO.md](TODO.md).

## Browser interface

### Connection status

The header status combines the HTTP session lifecycle and the browser
WebSocket state. A healthy attached session should reach **Connected**.
Reconnect attempts use increasing delays and do not restart the selected agent.

### Sessions

The header shows one tab for each server-managed terminal. The active tab is
highlighted and each tab includes a lifecycle-status dot.

- Select a tab to attach the single browser terminal view to that managed PTY.
- Swipe the tab strip horizontally on mobile.
- Use a wheel, trackpad, or the left/right overflow buttons on desktop.
- **+ New** stays beside the tab strip and creates another terminal.
- **Manage** opens the detailed session list.

- **Attach** in the detailed list switches to that managed PTY.
- Attaching does not stop the previously displayed session.
- **Refresh** reloads sanitized session metadata.
- **Remove** terminates and deletes a non-primary managed session.
- The primary `<agent> 1` entry (for example `Codex 1`) cannot be removed.

### `@cwt` peer workflow

**@cwt** opens a supervised cross-agent composer without changing xterm input
handling. A new peer thread always starts a fresh dedicated reviewer in the
source terminal's current, revalidated server directory. It does not reuse a
normal tab, even when a matching agent appears idle.

The operational sequence is:

1. choose the reviewer agent and action, then use **Source ready — Prepare
   handoff** while the source is at an empty agent prompt;
2. wait for the source agent to submit a bounded handoff;
3. inspect or edit **Preview handoff**;
4. use **Reviewer ready — Send** while the reviewer is at an empty agent
   prompt;
5. wait for **Response ready**;
6. use **Source ready — Return** while the source is at an empty agent prompt;
7. use a follow-up or **Recheck** to retain that same reviewer context.

Concrete example: from a Codex source tab, choose **Verify** with Claude and
enter `Review the current implementation for correctness, security regressions,
and missing Windows/Linux tests.` Inspect the generated handoff before
dispatch. Return Claude's response to the same Codex source, then use
**Recheck** for another pass that should retain Claude's reviewer context.
Use **+ New peer** only when a clean reviewer conversation is intentional.
It is disabled when session capacity is full, while follow-ups on an existing
reviewer remain available. One reviewer thread retains at most 32 turns; close
it and start a clean peer after reaching that boundary. The broker permits at
most 256 active in-memory peer threads, though the configured session capacity
normally applies first because each thread owns a dedicated terminal.

Catalog **Ready** means executable discovery and the bounded `--version` probe
succeeded. A fresh provider TUI may still require sign-in, onboarding, or
folder trust; complete those prompts manually in the linked reviewer tab.

The reviewer is visible as a linked tab and counts toward the configured
session capacity. `×` closes and removes it only after the reviewer process exit is
confirmed. A termination error keeps the exact reviewer and thread available
for a retry instead of reporting a false successful close. Provisioning owns
the reviewer identity before its PTY starts, so a concurrent Close is rejected
until that transaction finishes. The server never closes another session to
make room. An open peer thread prevents restart, terminate, or deletion of its
source. Generic restart/terminate controls are disabled or rejected for the
reviewer itself because a restarted PTY would not retain the claimed context.

Peer coordination does not parse raw PTY output and does not infer agent
idleness from silence, cursor position, or process state. The agents call a
hidden helper in the same `codex-web` executable. That helper connects to an
ephemeral loopback-only listener with a random capability scoped to the exact
PTY generation. The capability is not the browser token, is never accepted on
the public listener, and is revoked when the generation ends.
`CODEX_WEB_TOKEN` is stripped from managed PTYs and version probes. Server
shutdown disables new capabilities and revokes existing ones before releasing
the private listener; an unexpected private-listener exit also stops the
public service.

The readiness-labelled buttons are an explicit operator acknowledgement, not
an inferred state. The corresponding API requests require `sourceReady: true`
or `reviewerReady: true`, and delivery is rejected if the PTY generation has
changed. Do not confirm readiness while the CLI is showing a permission,
login, trust, or first-run prompt, or while text is partially entered.

If an agent's tool policy declines or blocks the helper invocation, the turn
remains visibly pending. Inspect that agent's tab, approve the local command
if appropriate, or close the peer thread. Do not diagnose completion by
copying terminal output into the application. Handoffs and responses are
limited to 64 KiB, live only in memory, and are lost when the server stops.
Their line endings are normalized and unsafe terminal control characters are
rejected by both the broker and helper.

### New

**+ New** uses two focused steps.

1. **Choose a project folder** selects the native working directory on the
   server.
2. **New terminal** selects Codex, Claude, or AGY and starts that CLI in the
   chosen directory.

The folder dialog has:

- **Favorites** — explicitly starred server folders, up to 100;
- **Recent** — up to 30 successfully used folders, newest first and
  deduplicated;
- **Browse** — filesystem roots, breadcrumbs, **Up**, and one level of sorted
  child directories at a time;
- **Folder path** — a manual absolute path on the server, useful when a folder
  has more than the 10,000 displayed subdirectory limit.

Files never appear in the browser and directory listing is not recursive.
Paths refer to the host running Codex Web Terminal, not the viewing phone or
laptop. **Use folder** advances to the agent picker. **Change folder** returns
without losing the intended launch flow. The star action adds or removes a
Favorite.

A Recent entry remembers its last agent. A Favorite remembers its preferred
agent after a successful launch from that directory. Those entries provide a
direct **Start Codex**, **Start Claude**, or **Start AGY** action. The server
still revalidates the folder and the frontend checks the current catalog. If
the remembered agent is no longer ready, the full agent picker opens so
another installed agent can be selected.

The agent dialog identifies the server operating system and architecture and
makes clear that the CLI runs on the server host, not in the viewing browser
or phone. It also displays the chosen working folder.

Each agent card reports:

- **Ready** and `Installed version …` when the fixed version probe succeeds;
- **Not found** when no candidate executable resolves;
- **Configuration error** when an explicit override or resolved executable
  fails validation.

Only a ready card provides **Start Codex**, **Start Claude**, or **Start AGY**.
A missing or misconfigured card shows a selectable provider command, **Copy**,
**Official docs**, the required shell, the `--version` verification command,
and **Check again**. Opening **New** always requests a fresh catalog so a tab
cannot keep stale availability from an earlier server generation. The header
**Refresh** and per-card **Check again** make the same
`/api/agent-catalog?refresh=true` request; it never executes the displayed
command. Creation errors remain in the open dialog. A successful create closes
it and attaches the terminal.

On a phone, scroll vertically inside the agent-card list. Each card keeps its
own **Start** action; later cards and their buttons remain reachable without
scrolling the underlying terminal page.

The server allows 20 managed sessions by default and accepts a configured
capacity from 1 through 256. **Manage** displays the current/capacity value,
for example `3/20`. The count includes the primary entry, ordinary running or
stopped entries, and dedicated peer reviewers. **+ New** is disabled when the
capacity is full; existing peer follow-ups remain available because they reuse
their reviewer. A slot is released only by deleting a removable ordinary
entry or closing its peer thread. Each entry has its own lifecycle, output
replay buffer, and connected-client count; each running entry also owns a full
agent process.

When a dangerous-mode switch is active, the card warns that approvals are
disabled and the agent may edit files and run commands without asking for
confirmation.

### Connect / Reconnect

This button closes and recreates only the browser WebSocket attachment. It is
safe to use after a network interruption or stale screen. It does not restart
or terminate the underlying agent process.

### Restart

**Restart** terminates and recreates the selected agent's PTY. Its stable
`terminalId` remains, but its `sessionId`, PID, and PTY generation change.
The agent profile and selected working folder remain the same, and a
successful restart refreshes that folder in Recent. Output from the previous
generation is not treated as current live output.
On Linux, termination targets the direct PTY child and cannot guarantee cleanup
of a descendant that deliberately detached itself.

### Fullscreen

Requests browser fullscreen mode. Leaving fullscreen does not affect the
server session.

### Desktop slash key

On desktop, pressing an unmodified `/` while a non-editable header control has
focus sends `/` to the connected terminal. This prevents Firefox Quick Find
from taking over after using controls such as Reconnect. Slash remains normal
text in form fields and dialogs, and modified shortcuts such as `Ctrl+/` are
left to the browser or operating system.

### Mobile keys

Shows or hides the mobile toolbar. Its order begins with Enter and the arrow
keys, followed by Page Up/Down, Ctrl mode, Esc, Tab, Ctrl+C, Ctrl+L, Top, Live,
and Hide.

- **PgUp/PgDn** moves through xterm's client-side scrollback.
- **Top** moves to the oldest retained client-side line.
- **Live** returns to current terminal output.
- **Ctrl** applies Ctrl to the next typed ASCII letter and then turns off.
- **Hide** hides the toolbar; it can be shown again from the header.

### Settings

Settings control font size, client scrollback, theme, cursor blinking, and the
mobile toolbar. **Copy diagnostics** captures mobile viewport measurements
after terminal focus; it does not include the authentication token, keyboard
input, or terminal text.

**Terminate _agent_** stops the selected process without deleting its managed
entry. **Forget token** removes the token from the current browser tab.

## Reconnect and replay behavior

Closing the browser or losing connectivity does not terminate managed PTYs.
The server retains up to 16 MiB of raw PTY output per session. A newly attached
browser receives at most the newest 2 MiB and then:

1. receives the current sanitized session snapshot;
2. resets the xterm screen;
3. receives a bounded replay;
4. transitions to live output without a replay/live gap.

xterm also keeps client-side scrollback, 10,000 lines by default. The server
buffer contains raw ANSI bytes rather than a rendered screen model. If the
oldest ANSI state has been discarded, a very old replay can look imperfect;
causing the selected agent to redraw or restarting that session repairs it.

## Health and diagnostics

All HTTP API requests require a bearer token:

```bash
curl \
  -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  http://127.0.0.1:8787/api/health
```

Healthy output has:

```json
{
  "status": "ok",
  "codexInstalled": true,
  "sessionRunning": true,
  "connectedClients": 0,
  "sessionCount": 1,
  "runningSessions": 1,
  "maxSessions": 20
}
```

Important distinction:

- `codexInstalled: true` means the most recent preflight for at least one
  registered session successfully resolved the command and ran
  the configured agent's `--version`; it is not a continuously refreshed
  installation probe. The field name is retained for API compatibility.
- `sessionRunning: true` means at least one PTY process is actually running.
- `maxSessions` is the configured registry capacity, not a hard-coded UI
  value. `sessionCount` includes stopped entries and dedicated reviewers until
  they are removed or closed.

The frontend can still load while a PTY is failed so diagnostics and restart
controls remain available.

Inspect the complete agent catalog:

```bash
curl \
  -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  "http://127.0.0.1:8787/api/agent-catalog?refresh=true"
```

Schema version 1 includes `server.os`, `server.arch`, `server.shell`, and one
entry per cataloged agent. All three agents are cataloged by default; optional
agents without explicit overrides are omitted when auto-detection is disabled:

```json
{
  "kind": "claude",
  "state": "ready",
  "configuration": "auto",
  "version": "2.x",
  "dangerouslySkipPermissions": false,
  "install": {
    "command": "platform-specific manual command",
    "shell": "powershell",
    "verifyCommand": "claude --version",
    "updateCommand": "claude update",
    "docsUrl": "https://code.claude.com/docs/en/setup",
    "requiresServerAccess": true
  }
}
```

`version` is `null` when the CLI is not ready. `missing` means no supported
candidate resolved. `misconfigured` means an explicit override or candidate
was found but failed validation. `configuration: "override"` means the
command value is authoritative and never falls back to another executable.
Repairing that exact file or its permissions can be followed immediately by
**Check again**. Changing or removing the startup override value requires
updating the server configuration and restarting the server; running a generic
installer alone does not replace an override. The legacy `/api/agents`
endpoint lists only ready kinds and exists for older frontends.

List sessions:

```bash
curl \
  -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  http://127.0.0.1:8787/api/sessions
```

List active peer threads:

```bash
curl \
  -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  http://127.0.0.1:8787/api/peer/threads
```

Peer API responses contain the current handoff or reviewer response after one
has been submitted. Treat them as terminal-conversation data: do not put them
in diagnostics or logs. Browser routes use the normal bearer token. The
private `/internal/v1/peer` helper route exists only on a separate loopback
listener and is not served on the configured public port.

Inspect filesystem roots and saved workspace state:

```bash
curl -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  http://127.0.0.1:8787/api/filesystem/roots

curl -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  http://127.0.0.1:8787/api/workspaces
```

List the configured default directory with an empty object, or return a
previously received opaque `directoryId`:

```bash
curl -X POST \
  -H "Authorization: Bearer $CODEX_WEB_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{}' \
  http://127.0.0.1:8787/api/filesystem/list
```

These responses contain server filesystem paths. Sanitize them before sharing
diagnostics. All `/api/filesystem/*` and `/api/workspaces*` routes require the
same bearer token as session control. Session-create, directory list/resolve,
and Favorite-upsert JSON bodies are capped at 256 KiB.

Do not paste production health commands containing real tokens into tickets,
chat messages, or shared logs.

## Logging

Structured `tracing` output includes:

- bind address and project directory;
- PTY startup and PID when available;
- client connect/disconnect;
- restart and process exit;
- sanitized errors.

It deliberately excludes tokens, keyboard input, terminal output, Codex
credentials, and authentication files. The separate startup `println!`
intentionally prints the complete authenticated URL. If stdout is redirected
to a file or journal, that destination contains the token.

Select verbosity:

```bash
--log-level info
--log-level codex_web_terminal=debug
```

Debug logging remains server-level; it does not enable terminal-content
logging.

## Linux user service example

For a persistent single-user installation, place the package in a stable
directory such as:

```text
/home/alice/apps/codex-web/
├── codex-web
└── web/
```

Create a protected environment file:

```bash
install -d -m 0700 "$HOME/.config/codex-web"
umask 077
python3 -c 'import secrets; print("CODEX_WEB_TOKEN=" + secrets.token_urlsafe(32))' \
  > "$HOME/.config/codex-web/environment"
chmod 0600 "$HOME/.config/codex-web/environment"
```

Create `~/.config/systemd/user/codex-web.service`:

```ini
[Unit]
Description=Codex Web Terminal
After=network-online.target

[Service]
Type=simple
WorkingDirectory=%h/projects/my-app
EnvironmentFile=%h/.config/codex-web/environment
ExecStart=%h/apps/codex-web/codex-web --host 127.0.0.1 --port 8787 --project %h/projects/my-app --state-dir %h/.local/state/codex-web-terminal --command /absolute/path/to/codex --no-open-browser
Restart=on-failure
RestartSec=3
KillSignal=SIGINT
KillMode=control-group
StandardOutput=null
StandardError=journal

[Install]
WantedBy=default.target
```

Load and start it:

```bash
systemctl --user daemon-reload
systemctl --user enable --now codex-web.service
systemctl --user status codex-web.service
```

Replace `/absolute/path/to/codex` with the result of `command -v codex` for the
service user. Using an absolute path avoids differences between an interactive
shell's `PATH` and the systemd user-manager environment. The explicit
`--state-dir` keeps Favorites and Recent in a predictable per-user location;
the server creates a missing final directory with mode `0700`. If it already
exists, it must be owned by the service user and grant no group/other
permissions.

This hardened example discards stdout because startup stdout contains the
authenticated URL. It also discards the normal structured tracing stream,
which currently uses stdout. Service state is still available with:

```bash
systemctl --user status codex-web.service
```

For detailed diagnostics, stop the unit and run the same command interactively
in a private terminal. If `StandardOutput=journal` is enabled temporarily,
understand that the journal will retain the complete token-bearing startup URL
until it is rotated or vacuumed.

Stop:

```bash
systemctl --user stop codex-web.service
```

To reach this service over Tailscale, replace `127.0.0.1` in `ExecStart` with
the machine's specific Tailscale IP, then run `daemon-reload` and restart the
unit. Avoid `0.0.0.0` unless all interfaces are intentionally in scope.

## Normal shutdown

For an interactive server, press Ctrl+C in the server console. The server
stops its managed sessions and closes the HTTP listener.

For a user service:

```bash
systemctl --user stop codex-web.service
```

After shutdown, verify that the port is no longer listening:

```bash
ss -ltn
```

On Windows:

```powershell
Get-NetTCPConnection -State Listen -LocalPort 8787 -ErrorAction SilentlyContinue
```

Do not kill unrelated processes merely because they use a similar name.
Resolve the exact service, PID, and port first.

## Upgrade procedure

Server restarts terminate all managed PTYs. Save or finish important agent work
before upgrading.

Recommended sequence:

1. For an official release, download the new archive and verify its checksum
   and attestation before extraction. For a source deployment, pull or check
   out the desired reviewed commit and run the full Windows and Linux
   build/test matrix from [BUILDING.md](BUILDING.md).
2. Read the release notes and confirm that the host satisfies the artifact's
   platform and glibc requirements.
3. Keep the old package until the new one has passed validation.
4. Stop the existing server.
5. Replace the executable, the entire adjacent `web` directory, and the
   accompanying documentation/license bundle together.
6. Start the new server with the same project, state directory, and explicit
   token if continuity of Favorites/Recent, the browser URL, and the credential
   is required. This does not preserve PTY processes or live terminal sessions.
7. Check `/api/health`, load the frontend, attach, and verify keyboard input.
8. Remove the old package only after the new runtime is confirmed.

Do not mix a new frontend with an older backend during deployment.
Before deployment, confirm that the Markdown documentation still matches the
current commands, versions, package contents, supported platforms, behavior,
and known limitations.

## Troubleshooting

### The URL opens but the terminal is blank

Check the session snapshot and server log. The HTTP server intentionally
remains available after PTY startup fails. Confirm the selected agent's
`--version` command as the same OS user, inspect `/api/agent-catalog`, and
check `sessionRunning`.

### An agent is not installed or has a configuration error

Use the **New terminal** card's shell, command, and verification instructions
on the server host. **Not found** means no supported executable resolved.
**Configuration error** commonly means an explicit path is wrong, the file is
not executable, or `--version` failed. Explicit overrides never fall back.

After correcting the host installation, select **Refresh** or **Check again**.
If startup flags or service environment variables changed, restart the server
first; understand that restarting destroys its in-memory PTYs. Do not paste
the displayed install command into the browser developer console.

### Authentication fails

Use the URL from the current server instance. A generated token changes after
restart. Five repeated failures from one address trigger a temporary
one-minute block.

### A selected folder cannot be opened or launched

Confirm that the path is absolute on the server host, still exists, is a
directory, and can be listed by the exact operating-system account running
`codex-web`. A missing directory returns `404`; insufficient access returns
`403`. A path copied from the viewing phone or laptop is not meaningful unless
that same native path exists on the server. `--project` does not restrict the
picker, and changing it does not repair permissions on another directory.

### Workspace state was quarantined

Look in the configured state directory for
`workspaces.corrupt.<uuid>.json`. This means `workspaces.json` exceeded
32 MiB, was invalid, or used an unsupported schema version. The server
preserves the file and loads clean state; successful primary startup may then
add the default folder to Recent. Stop the server before restoring a reviewed
schema-1 backup.

If startup instead reports an unsafe state location, nothing is quarantined or
chmod-repaired. Use a dedicated non-link directory. On Unix, make it owned by
the effective service user with no group/other permissions; `0700` for the
directory and `0600` for an existing state file are the normal settings. Also
verify create/rename permission for the service account.

Inspect a Unix default without following or changing anything:

```bash
case "${XDG_STATE_HOME:-}" in
  /*) workspace_state_dir="$XDG_STATE_HOME/codex-web-terminal" ;;
  *) workspace_state_dir="$HOME/.local/state/codex-web-terminal" ;;
esac
if test -e "$workspace_state_dir" || test -L "$workspace_state_dir"; then
  test ! -L "$workspace_state_dir"
  stat -c '%F %U %G %a %n' -- "$workspace_state_dir"
  if test -e "$workspace_state_dir/workspaces.json" ||
    test -L "$workspace_state_dir/workspaces.json"; then
    test ! -L "$workspace_state_dir/workspaces.json"
    stat -c '%F %U %G %a %n' -- \
      "$workspace_state_dir/workspaces.json"
  fi
fi
```

After verifying the exact owner and path, the owner can tighten overly broad
Unix modes explicitly:

```bash
chmod 0700 -- "$workspace_state_dir"
if test -e "$workspace_state_dir/workspaces.json"; then
  chmod 0600 -- "$workspace_state_dir/workspaces.json"
fi
```

Do not point `--state-dir` directly at `/`, the home directory,
`XDG_STATE_HOME`, the current directory, or the system temporary directory;
use a dedicated child directory. Correct wrong ownership deliberately as an
administrator rather than making the application take ownership.

On Windows, inspect reparse metadata and the inherited ACL before restarting:

```powershell
$workspaceStateDir = Join-Path $env:LOCALAPPDATA "codex-web-terminal"
Get-Item -LiteralPath $workspaceStateDir -Force |
  Format-List FullName,Attributes,LinkType,Target
Get-Acl -LiteralPath $workspaceStateDir | Format-List
```

Use a dedicated child directory, not a drive root, profile/base directory,
current directory, or temporary directory. Remove unexpected reparse points
or repair ACLs through normal Windows administration; the application does
not rewrite them.

### Browser reconnect loops

Confirm:

- the server process and port are active;
- the URL host, port, and scheme are correct;
- a reverse proxy forwards WebSocket Upgrade and binary frames;
- the page Origin host matches the public Host;
- browser extensions are not blocking WebSockets.

### Tailscale URL is unreachable

Check both devices:

```bash
tailscale status
tailscale ping SERVER_NAME
```

Verify that the server bound either the exact Tailscale IP or an intentionally
broader address. Check Tailscale ACLs and the host firewall.

### Linux reports `sessionRunning: false`

Check:

```bash
command -v codex
codex --version
command -v claude
claude --version
command -v agy
agy --version
test -r /dev/ptmx
```

Check only the CLIs you intend to use. Also verify executable permission on the
catalog's resolved command and read access to the default or selected working
directory.

### Windows reports a PowerShell policy error

Use `where.exe codex`, `where.exe claude`, or `where.exe agy`. Prefer a
discovered `.exe` or `.cmd` entry point. The application deliberately avoids
automatic `.ps1` selection.

### Mobile viewport or keyboard behavior is unusual

Use **Settings → Mobile viewport diagnostics**, tap the terminal, wait for
collection to finish, and copy the diagnostic JSON. Record the browser,
device, orientation, and exact interaction sequence. Diagnostics intentionally
exclude terminal content and credentials.

## Operational checklist

Before exposing the service to another device:

- [ ] The primary CLI is ready and authenticated for the service user.
- [ ] Optional agent status/version and manual commands match the server OS.
- [ ] Dangerous permission-bypass flags are off unless explicitly required.
- [ ] The default project directory is correct.
- [ ] Everyone holding the token is trusted to browse and launch in every
      directory readable by the server account.
- [ ] The workspace state directory is private, writable by the service
      account, and included in the intended backup policy.
- [ ] Every server instance that can run concurrently has a distinct state
      directory.
- [ ] The build and tests passed on the target platform.
- [ ] The package contains the matching executable and `web` assets.
- [ ] The token is strong, private, and not committed.
- [ ] The bind address is loopback or a private/Tailscale interface.
- [ ] Firewall and Tailscale ACL scope is understood.
- [ ] `/api/health` reports a running session.
- [ ] The browser can browse a disposable folder, start the selected agent
      there, attach, type, reconnect, and replay.
- [ ] A precise stop procedure is known.
