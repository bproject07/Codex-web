# Security Policy

Codex Web Terminal provides authenticated remote control of a real terminal.
Treat vulnerabilities that bypass authentication, cross session boundaries,
expose terminal data, weaken Origin validation, or exceed the permissions
intended for the operating-system account running the server as
security-sensitive.

The configured `--project` directory is the default working directory for the
primary terminal. It is not an access-control boundary, filesystem root, or
sandbox. After bearer authentication, the browser can list filesystem roots,
browse directories, resolve an absolute server path, read Favorites/Recent,
change Favorites, and launch a new terminal in any directory readable by the
operating-system account running the server. Successful launches also update
Recent. The terminal process inherits that account's permissions and
environment.

Every Codex process is launched with `--yolo`, which disables Codex approvals
and sandboxing. This applies to the primary terminal, new and restarted
sessions, and dedicated `@cwt` reviewers. There is currently no server or
browser opt-out. Treat possession of the bearer token as authority to cause
unprompted Codex actions with the full permissions of the server account.

All `/api/filesystem/*`, `/api/workspaces*`, and session endpoints require the
same bearer token. Directory IDs preserve native Windows UTF-16 or Unix path
bytes as an opaque API transport value. They are neither secret nor encrypted,
signed capabilities, and they do not provide authorization. Every use is
decoded, canonicalized, and checked again by the server. Because the encoding
is reversible and responses also include display paths, IDs and saved state
can reveal filesystem layout.

Directory browsing is intentionally non-recursive and returns only immediate
child directories, not files, but this is a UI/data-minimization property
rather than confinement. Anyone with the token must be trusted with the full
filesystem reach of the server account. Run the service under a dedicated,
least-privileged account when broader access is not intended.
Session-create and workspace JSON request bodies are capped at 256 KiB.

## Peer helper boundary

The supervised `@cwt` workflow creates a fresh dedicated reviewer PTY under
the same operating-system account as every other managed agent. This provides
conversation separation from ordinary tabs, not hostile-process or filesystem
isolation. A reviewer can inspect anything its account and configured agent
policy permit. Use a least-privileged account when that reach is too broad.

Peer handoffs and responses are limited to 64 KiB and kept only in memory.
They are returned only by bearer-authenticated browser APIs or by a private
helper listener bound to an ephemeral loopback port. Each PTY generation gets
a 256-bit capability scoped to its source/reviewer role. It rotates on restart
and is revoked on failure, exit, terminate, or deletion. The browser token is
never passed to the helper: `CODEX_WEB_TOKEN` is removed from managed PTY and
version-probe environments after server configuration is read. Helper
capabilities must never appear in argv, URLs, logs, diagnostics, screenshots,
or public error reports. During shutdown, new capability activation is
disabled and active capabilities are cleared before the private listener is
released. An unexpected private-bridge exit fails the public server closed.

Artifacts preserve normal Unicode Markdown, but their line endings are
normalized and terminal control characters are rejected by the broker and
again by the helper before output. Automation delivery also requires an
explicit source/reviewer readiness acknowledgement and the exact active PTY
generation. This prevents stale-session routing; it does not detect whether a
third-party CLI is showing its normal prompt, a permission dialog, or setup
screen. Confirm readiness only at an empty agent prompt.

A process running as the same account may be able to inspect another process's
environment on some operating systems. The scoped capability prevents it from
creating sessions or routing arbitrary turns, but it does not prove that a
particular model authored an artifact. Stronger provenance requires separate
operating-system or container identities and is outside the current scope.

Favorites and Recent are stored server-side in `workspaces.json`. This file
contains filesystem paths and usage history. Protect its directory and
backups, and do not attach it to public reports. Give concurrently running
server instances different state directories: persistence is coordinated
inside one process only and has no cross-process lock or merge. The state
location must be a dedicated non-link directory and `workspaces.json` a
regular non-link file; Windows reparse points are rejected.

On Unix, a newly created state directory uses mode `0700` and a new state file
uses `0600`. Existing targets must already be owned by the effective server
user and grant no group/other permissions. The application rejects them
rather than silently changing operator-managed permissions. On Windows,
operators must use appropriate ACLs. Invalid, future-version, or state larger
than the 32-MiB (33,554,432-byte) read/write limit is preserved under a
`workspaces.corrupt.<uuid>.json` name rather than silently overwritten. A
pending write over the same limit is rejected before replacement.

## Supported versions

Security fixes target the latest published release and the latest commit on
`main`. Older releases, unofficial archives, and locally modified builds are
not supported.

Official release archives appear only on the repository's GitHub Releases
page. Verify the matching `SHA256SUMS.txt` entry and GitHub artifact
attestation before extraction:

```text
gh attestation verify --repo bproject07/Codex-web \
  --signer-workflow bproject07/Codex-web/.github/workflows/release.yml \
  <archive>
```

Release archives include a target-specific `THIRD_PARTY_LICENSES` manifest
bound to both dependency lockfiles. Windows executables are not
Authenticode-signed yet and may show an unknown-publisher warning. A checksum
protects transfer integrity and an attestation binds the archive to the
repository workflow; neither makes an unreviewed version safe or grants the
terminal fewer operating-system permissions.

## Application update boundary

Automatic checks use a compile-time fixed GitHub API endpoint for
`bproject07/Codex-web`; the browser cannot provide a repository, URL, asset,
checksum, path, command, or executable. Only a newer stable SemVer release
whose GitHub metadata is published, immutable, and contains the exact native
asset plus `SHA256SUMS.txt` is eligible. Both assets must be in the uploaded
state, within bounded sizes, and expose valid SHA-256 metadata. The downloaded
archive hash must match both GitHub metadata and the checksum file.

Archive inspection happens before execution in a newly created private state
directory. Absolute/traversal paths, backslashes, NUL/control characters,
links, special files, duplicates, case collisions, unsafe Windows names,
excessive entry counts, expanded sizes, and compression ratios are rejected.
The package must contain the exact release marker, native x86-64 PE/ELF
header, backend version, frontend, documentation, and target-bound license
inventory.

Installation is side-by-side under `<state-dir>/updates/releases`; it never
overwrites the running executable and never elevates privileges. The operator
must explicitly acknowledge termination of all PTYs and in-memory peer
threads.

The manually installed v0.2 executable is the persistent root supervisor and
update trust anchor. It retains the same PID while it launches and monitors
verified worker generations; a worker cannot create another supervisor. A
worker may request the next generation only through its reserved exit status
and a matching bounded `pending.json`. That file contains only schema,
request ID, source version, and target version. Paths are derived from the
private state directory. Tokens, URLs, commands, checksums, arguments, and
environment values are never persisted in either update pointer. Pointer
writes are private and atomic; invalid or stale pending state is quarantined
instead of being interpreted as launch authority.

The root passes the current token to a worker only in `CODEX_WEB_TOKEN`—not in
worker argv, readiness URLs, JSON, update files, or logs—and the worker
consumes and removes it from the inherited process environment before
application threads start. If browser policy blocks `sessionStorage`, the
browser's single post-update reload reuses the normal same-origin authenticated
URL mechanism and immediately removes the token query again; it does not enter
update state. For every launch the root also creates an unguessable readiness
nonce and passes it only through the private worker environment. The worker
consumes/removes that variable and returns the nonce only in its authenticated
health response. The root accepts a generation only after direct authenticated
local readiness, with system proxies disabled and a bounded response, reports
both the expected server version and exact nonce. It then atomically changes
the active pointer containing only the active and exact previous versions. On
validation, launch, readiness, or pointer-commit failure, the candidate is
terminated and waited for; the exact prior executable is started and must
itself become ready while the active pointer remains unchanged.

Worker-package updates cannot repair a vulnerability in the already running
root supervisor or change its private worker/pending protocol. A release that
changes that security boundary must require a manual full-archive launcher
replacement. Operators must keep the configured bootstrap package protected
and present; automatic release cleanup is limited to managed worker packages
under the state directory.

Only official archives contain `release-package.json`. Copying that marker,
weakening the fixed repository/asset checks, accepting mutable releases, or
making browser-triggered installation silent is a security-sensitive change.
GitHub artifact attestation verification remains a recommended manual
provenance step; checksum verification alone is not described as equivalent.

## Reporting a vulnerability

Use **Report a vulnerability** in the GitHub repository's **Security** tab.
This creates a private report for the maintainers.

Include the affected version or commit, impact, a sanitized reproduction, and
any remediation idea you have. Remove all live credentials and private data
before submitting.

Do not open a public issue containing:

- an authentication token or authenticated URL;
- terminal input, output, or screenshots with private content;
- Codex credentials or configuration;
- account, organization, company, device, host, or private project names;
- private IP addresses or filesystem paths;
- `workspaces.json`, quarantined workspace state, or Favorite/Recent exports;
- exploit code that would expose an active server.

Use neutral placeholders and a disposable local test environment. If private
reporting is temporarily unavailable, open a public issue containing only a
request for a private contact channel and no vulnerability details.

## Deployment responsibility

The application has no built-in TLS and must not be exposed directly to the
public Internet. Use loopback access or a restricted private network such as
Tailscale, protect the token, use a least-privileged server account, secure the
workspace state directory, and follow [OPERATIONS.md](OPERATIONS.md).
