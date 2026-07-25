# Security Policy

Codex Web Terminal provides authenticated remote control of a real terminal.
Treat vulnerabilities that bypass authentication, cross session boundaries,
expose terminal data, weaken Origin validation, or exceed the permissions
intended for the operating-system account running the server as
security-sensitive.

The configured project directory is the child process's working directory. It
is not an access-control boundary or a filesystem sandbox. The terminal process
inherits the operating-system permissions and environment of the server user.

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
- exploit code that would expose an active server.

Use neutral placeholders and a disposable local test environment. If private
reporting is temporarily unavailable, open a public issue containing only a
request for a private contact channel and no vulnerability details.

## Deployment responsibility

The application has no built-in TLS and must not be exposed directly to the
public Internet. Use loopback access or a restricted private network such as
Tailscale, protect the token, and follow [OPERATIONS.md](OPERATIONS.md).
