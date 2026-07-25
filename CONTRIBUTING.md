# Contributing

Thank you for helping improve Codex Web Terminal. Contributions that make the
terminal safer, more reliable, more accessible, and easier to operate across
Windows and Linux are welcome.

## Before starting

1. Read [README.md](README.md), [AGENTS.md](AGENTS.md), and
   [BUILDING.md](BUILDING.md).
2. Search existing issues and pull requests.
3. Open an issue before a large protocol, security, UI, or architecture change.
4. Keep each pull request focused on one problem.

Report suspected vulnerabilities privately by following
[SECURITY.md](SECURITY.md); do not disclose them in a public issue.

## Development rules

- Preserve raw PTY and xterm.js terminal semantics.
- Never add credentials, authenticated URLs, terminal transcripts, account or
  company names, private IP addresses, or personal filesystem paths.
- Use neutral fixtures and screenshots.
- Do not add automatic public tunnels or broad firewall rules.
- Keep Windows and Unix launch behavior independently correct.
- Update documentation in the same change as behavior.

## Required validation

Every code or dependency change must pass on both Windows and Linux:

```text
web:
  npm ci
  npm test
  npm run build

server:
  cargo fmt --all -- --check
  cargo test --all-targets --locked
  cargo clippy --all-targets --locked -- -D warnings
  cargo build --release --locked
```

Run the package script and a native PTY/runtime smoke test on each affected
platform when relevant. See [BUILDING.md](BUILDING.md) for exact commands.

## Pull requests

Explain:

- the problem and chosen solution;
- security and compatibility impact;
- Windows and Linux test results;
- documentation changes;
- remaining limitations.

All contributions are submitted under the repository's [MIT license](LICENSE).
By submitting a contribution, you agree that it may be distributed under that
license.
