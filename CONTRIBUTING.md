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
- Keep peer coordination supervised and provider-neutral: no ANSI/output
  scraping, idle heuristics, ordinary-session reuse, or repository
  requirement.
- Keep peer helper capabilities loopback-only, per PTY generation, bounded,
  and absent from argv, responses, logs, diagnostics, and screenshots.
- Do not hand-edit generated third-party license bundles or release archives.
  Dependency changes must pass the fail-closed target license generator.
- Preserve the updater trust boundary: fixed official repository and exact
  assets, immutable stable releases, bounded downloads, dual SHA-256 checks,
  safe extraction, official-package marker, side-by-side activation, explicit
  PTY termination confirmation, one stable root supervisor, authenticated
  exact-version plus per-launch nonce readiness, readiness-gated pointer
  change, and exact-prior rollback. Worker token/nonce environment values must
  be consumed and removed before application threads start. `pending.json`
  remains limited to request/source/target identity; never accept or persist
  browser-supplied update URLs, paths, commands, checksums, tokens, or
  environment values.
- Do not make worker generations supervise one another or replace/delete the
  bootstrap package. A change to the root/worker marker, pending/active schema,
  reserved exit status, readiness contract, or supervisor security boundary
  requires an explicit compatibility plan and may require a manual launcher
  replacement.
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
Changes to peer coordination must run `scripts/peer-review-regression.py`
against the packaged Windows and Linux binaries. Dependency or release changes
must also generate and review the Windows MSVC and Linux GNU
`THIRD_PARTY_LICENSES` bundles.
Updater changes additionally require malicious ZIP/TAR tests and
`scripts/updater-supervisor-regression.py` on both operating systems. The
native regression must cover a stable root PID, two sequential worker
generations without a nested supervisor, active-pointer commit only after
readiness, and exact-prior rollback after a failed candidate.

Version tags and GitHub Releases are maintainer-owned. Contributors should
change package versions only as part of an explicitly scoped release change
and should never upload local `dist` output to a pull request.

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
