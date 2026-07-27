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

Until stable releases are published, security fixes target the latest commit
on `main`. Older commits and locally modified builds are not supported.

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
