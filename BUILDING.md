# Building Codex Web Terminal

This guide describes how to clone, validate, build, test, and package Codex Web
Terminal on Windows and Linux. It is intended both for people working manually
and for automation or coding agents that need an exact, reproducible workflow.

For runtime configuration and day-to-day administration, see
[OPERATIONS.md](OPERATIONS.md). Repository-specific guidance for coding agents
is in [AGENTS.md](AGENTS.md).

## What gets built

The repository contains two applications:

1. `web/` is a React and TypeScript frontend built by Vite.
2. `server/` is a Rust backend that owns the PTYs, serves the frontend, and
   exposes the authenticated HTTP and WebSocket interfaces.

The lightweight workspace launcher is split deliberately:

- `web/src/workspaces/` owns the dependency-free folder-picker UI and state
  model;
- `web/src/api.ts` validates its authenticated HTTP DTOs;
- `server/src/filesystem.rs` owns native path-ID encoding and bounded,
  directory-only browsing;
- `server/src/workspaces.rs` owns versioned Favorites/Recent persistence;
- the session registry receives an already validated native working directory
  and applies it only to the newly reserved PTY.

The frontend must be built before creating a local application package. A
packaged application has this layout:

```text
package-directory/
├── codex-web.exe       # Windows
│   or codex-web        # Linux
├── web/
│   ├── index.html
│   └── assets/
├── README.md
├── BUILDING.md
├── OPERATIONS.md
├── AGENTS.md
├── TODO.md
├── CONTRIBUTING.md
├── SECURITY.md
├── CODE_OF_CONDUCT.md
├── THIRD_PARTY_NOTICES.md
├── THIRD_PARTY_LICENSES/        # tagged release archives
│   ├── THIRD_PARTY_LICENSES.txt
│   └── manifest.json
├── docs/
│   └── screenshots/
└── LICENSE
```

The `web` directory must remain next to the executable. When the backend is run
with `cargo run`, it also searches the source tree's `web/dist` directory.

The normal build scripts create local packages and intentionally stop before
generating redistribution notices. Tagged GitHub Releases add a
target-specific `THIRD_PARTY_LICENSES` directory and publish only after that
bundle, Windows/Linux tests, packaged peer regression, archive layout,
checksums, and provenance all pass. Before sharing a local package, run the
same generator documented below; `THIRD_PARTY_NOTICES.md` alone is not the
complete binary-redistribution bundle.

## Supported and validated platforms

The following paths have been exercised:

| Platform | Frontend | Rust build and tests | Native PTY runtime |
| --- | --- | --- | --- |
| Windows 10/11 x86-64 | Yes | Yes, GNU toolchain; MSVC is the recommended target | Yes, ConPTY |
| Arch Linux x86-64 | Yes | Yes, native GNU/Linux target | Yes, Unix PTY |
| Ubuntu 22.04 x86-64 release runner | Yes | Yes, glibc release package | Yes, synthetic native PTY |

The Unix command construction also applies to macOS, but macOS has not yet had
a full runtime validation.

## Cross-platform validation policy

Every code change, bug fix, refactor, or dependency update must be tested on
both Windows and Linux before commit or release. A successful test on only one
operating system is not sufficient, even when the edited code appears
platform-independent.

At minimum:

1. run the frontend tests and production build on Windows and Linux;
2. run Rust formatting, tests, Clippy, and the locked release build on Windows
   and Linux;
3. validate the package layout on both platforms;
4. run a native PTY/runtime smoke test on each affected platform when launch,
   process lifecycle, WebSocket I/O, resize, replay, or terminal behavior
   changes;
5. generate and validate the target-specific third-party license bundle;
6. update every affected Markdown file in the same commit.

For workspace browsing, persistence, or selected-directory launch changes,
the Windows and Linux checks must additionally cover native path-ID round
trips, directory-only one-level listings, manual absolute-path resolution,
Favorites/Recent persistence and limits, stale/inaccessible paths,
the 256-KiB request-body boundary, 32-MiB (33,554,432-byte) state read/write
boundary, corrupt-state quarantine, and a real fixture PTY whose current
working directory is the selected folder.

If the local machine cannot run Linux, use GitHub Actions, a Linux VM, or a
Linux host you control. Do not mark the change complete or describe it as
Linux-supported until that validation has actually passed.

Documentation must describe the current source and verified behavior. Check
CLI flags, environment variables, dependency versions, build commands,
package contents, UI labels, platform support, and known limitations instead
of copying potentially stale text from an earlier release.
Agent install locations and install/update commands must be checked against the
current official OpenAI, Anthropic, and Google documentation. Validate the
catalog metadata and manual **Refresh** / **Check again** workflow on both
Windows and Linux; never substitute a successful mock probe for the real
platform checks.

## Source checkout

The public source repository is:

```text
https://github.com/bproject07/Codex-web.git
```

```bash
git clone https://github.com/bproject07/Codex-web.git
cd Codex-web
```

After cloning, verify the checkout:

```bash
git status
git log -1 --oneline
```

The expected branch is `main`, and a new checkout should have a clean working
tree.

## Shared prerequisites

Both Windows and Linux builds require:

- Git
- Node.js `^20.19.0` or `>=22.12.0`
- npm
- a current stable Rust toolchain with Cargo (the locked graph currently
  requires Rust 1.88 or newer)
- Python 3.11 or newer for regression and redistribution-license scripts
- enough space for `web/node_modules` and `server/target`

Codex, Claude, and AGY are not needed to compile or run the automated unit
tests. At least the selected primary CLI is required for a real application
session and the final runtime smoke test. Testing all three profiles requires
all three CLIs on the runtime host.

| Component | Build machine | Packaged runtime host |
| --- | --- | --- |
| Git | Needed to clone/update | Not required |
| Node.js and npm | Needed for the frontend build | Needed only when an installed CLI distribution requires Node |
| Rust and Cargo | Needed for backend build/tests | Not required |
| Native C linker/toolchain | Needed for the Rust build | Not required |
| Python | Needed for regression/release validation | Not required |
| Agent CLI and login | Needed only for a real smoke test | Primary required; others optional and auto-detected |
| Browser | Needed only for browser tests | Required on the viewing device |

Verify the toolchain:

```text
git --version
node --version
npm --version
rustc --version
cargo --version
python --version
codex --version
claude --version
agy --version
```

The frontend dependency graph is locked by `web/package-lock.json`. Use
`npm ci`, not `npm update`, for validation and release builds. The Rust
dependency graph is locked by `server/Cargo.lock`.

## Windows prerequisites

Install:

- a supported Node.js release with npm (`^20.19.0` or `>=22.12.0`)
- stable Rust through rustup
- either:
  - Visual Studio Build Tools with **Desktop development with C++**, or
  - MinGW-w64 GCC plus a Rust `x86_64-pc-windows-gnu` toolchain
- Codex CLI for runtime use

The preferred Rust target is MSVC. Open **Developer PowerShell for Visual
Studio** so `link.exe` is in `PATH`:

```powershell
rustup default stable-x86_64-pc-windows-msvc
rustc -vV
Get-Command link.exe
```

If Visual C++ is not available, install a GNU toolchain and ensure MinGW
`gcc.exe` is in `PATH`:

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup toolchain list
Get-Command gcc.exe
```

The Windows build script detects an unavailable MSVC linker and uses an
installed GNU toolchain when possible.

## Windows: automated build

Run from the repository root in PowerShell:

```powershell
.\scripts\build.ps1
```

The script performs these operations:

1. Resolves the repository, frontend, backend, and output paths.
2. Refuses to clean any directory other than the repository's exact `dist`
   directory.
3. Verifies `node`, `npm`, `rustc`, and `cargo`, including the supported Node
   version range.
4. Runs `npm ci` because a lockfile is present.
5. Runs the TypeScript check and Vite production build through
   `npm run build`.
6. Selects MSVC or the installed Windows GNU fallback.
7. Runs `cargo build --release --locked`.
8. Recreates `dist/`.
9. Copies the server executable, frontend assets, documentation, and license.

Expected output:

```text
dist/
├── codex-web.exe
├── web/
│   ├── index.html
│   └── assets/
├── README.md
├── BUILDING.md
├── OPERATIONS.md
├── AGENTS.md
├── TODO.md
├── CONTRIBUTING.md
├── SECURITY.md
├── CODE_OF_CONDUCT.md
├── THIRD_PARTY_NOTICES.md
├── docs/
│   └── screenshots/
└── LICENSE
```

Start the packaged build:

```powershell
.\scripts\run.ps1 -Project "C:\Projects\my-app"
```

The `-Project` directory is the canonicalized default. The primary agent
starts there; **+ New** may choose another directory readable by the server
account for that new managed session.

`run.ps1` searches `dist/codex-web.exe` before the release and debug binaries
under `server/target`. If `dist` contains an older package, rebuild it or run
the intended target binary explicitly.

## Windows: manual build and full validation

Use these commands when diagnosing a build or reproducing CI-like checks.

Frontend:

```powershell
Push-Location .\web
npm ci
npm test
npm run build
Pop-Location
```

Rust formatting, tests, linting, and release build:

```powershell
Push-Location .\server
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
Pop-Location
```

With the GNU fallback:

```powershell
$gnuToolchainLine = rustup toolchain list |
  Where-Object { $_ -match "x86_64-pc-windows-gnu" } |
  Select-Object -First 1
if (-not $gnuToolchainLine) {
  throw "Install an x86_64-pc-windows-gnu Rust toolchain first."
}
$gnuToolchain = ($gnuToolchainLine -split "\s+")[0]
rustup component add --toolchain $gnuToolchain clippy

Push-Location .\server
rustup run $gnuToolchain cargo test --all-targets --locked
rustup run $gnuToolchain cargo clippy --all-targets --locked -- -D warnings
rustup run $gnuToolchain cargo build --release --locked
Pop-Location
```

Confirm that `$gnuToolchain` contains the exact installed name before running
the commands. If no line is returned, install the GNU toolchain first.

The executable is:

```text
server\target\release\codex-web.exe
```

To run from the source tree after building the frontend:

```powershell
.\server\target\release\codex-web.exe `
  --project "C:\Projects\my-app" `
  --host 127.0.0.1 `
  --port 8787
```

## Linux prerequisites

Install the equivalent of:

- Git
- a C/C++ build toolchain (`gcc` or `clang`, a linker, and standard headers)
- Node.js `^20.19.0` or `>=22.12.0` with npm
- stable Rust and Cargo
- Python 3 only if running the optional browser/mobile regression utilities
- the selected Codex, Claude, or AGY CLI for a real runtime session

On Arch Linux, the required native build tools are normally provided by
`base-devel`. Rust can be installed with rustup or the distribution packages.
When using rustup:

```bash
rustup default stable
rustup component add rustfmt clippy
```

Verify native PTY support and the build environment:

```bash
test -r /dev/ptmx
cc --version
node --version
npm --version
rustc --version
cargo --version
```

## Linux: automated build

Run:

```bash
./scripts/build.sh
```

If the executable bit was lost while copying the source outside Git, use:

```bash
bash scripts/build.sh
```

The script uses the lockfiles, builds both applications, and creates:

```text
dist-linux/
├── codex-web
├── web/
│   ├── index.html
│   └── assets/
├── README.md
├── BUILDING.md
├── OPERATIONS.md
├── AGENTS.md
├── TODO.md
├── CONTRIBUTING.md
├── SECURITY.md
├── CODE_OF_CONDUCT.md
├── THIRD_PARTY_NOTICES.md
├── docs/
│   └── screenshots/
└── LICENSE
```

Run it with:

```bash
./scripts/run.sh "/home/user/projects/my-app"
```

Additional backend arguments can follow the project directory:

```bash
./scripts/run.sh "/home/user/projects/my-app" \
  --host 127.0.0.1 \
  --port 8787 \
  --no-open-browser
```

`run.sh` searches `dist-linux/codex-web` before the release and debug binaries
under `server/target`. If `dist-linux` is stale, rebuild it or run the intended
target binary explicitly. The project may be supplied only as the first
positional argument. It sets the default/primary working directory; it does
not restrict the authenticated folder picker to that tree.

## Linux: manual build and full validation

Frontend:

```bash
cd web
npm ci
npm test
npm run build
cd ..
```

Rust formatting, tests, linting, and release build:

```bash
cd server
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
cd ..
```

The native executable is:

```text
server/target/release/codex-web
```

The tested Linux artifact is a native, dynamically linked glibc executable,
not a universal portable binary. Build on the deployment host or on the oldest
glibc-based distribution that must be supported. musl and cross-compilation
have not been validated.

Confirm that it is a Linux executable:

```bash
file server/target/release/codex-web
./server/target/release/codex-web --version
./server/target/release/codex-web --help
```

Run directly from the source tree:

```bash
./server/target/release/codex-web \
  --project "/home/user/projects/my-app" \
  --host 127.0.0.1 \
  --port 8787 \
  --no-open-browser
```

On Unix, the resolved `--command` executable is launched directly inside the
native PTY. The Windows-only `--shell` option is accepted for configuration
compatibility but ignored.

## Manual Linux packaging

The following commands show the package layout explicitly. They assume the
frontend and release backend have already been built:

```bash
install -d dist-linux/web
install -m 0755 server/target/release/codex-web dist-linux/codex-web
cp -R web/dist/. dist-linux/web/
cp README.md BUILDING.md OPERATIONS.md AGENTS.md TODO.md \
  CONTRIBUTING.md SECURITY.md CODE_OF_CONDUCT.md \
  THIRD_PARTY_NOTICES.md LICENSE dist-linux/
cp -R docs dist-linux/
```

If `dist-linux` already contains an older build, prefer
`./scripts/build.sh`, which validates and safely recreates that exact output
directory.

Verify the package:

```bash
test -x dist-linux/codex-web
test -f dist-linux/web/index.html
./dist-linux/codex-web --version
```

## Redistribution license bundle

`scripts/generate-third-party-licenses.py` uses only the Python standard
library. It reads the locked non-development Cargo graph for one release
target, every installed locked npm runtime/build package, and the active Rust
standard-library notice. It validates exact reviewed license expressions,
collects top-level LICENSE, LICENCE, COPYING, COPYRIGHT, and NOTICE evidence,
and asserts the known extra attribution files for `atomic-waker`, `matchit`,
`unicode-ident`, and ICU4X. It also binds npm packages to their lock paths,
registry tarball URLs, and SHA-512 integrity values. Missing or unreviewed
evidence, malformed license expressions, mismatched package metadata, and
unexpected build-only fallbacks fail the build.

Run it once after a clean package build:

Windows MSVC:

```powershell
python -B .\scripts\generate-third-party-licenses.py `
  --target x86_64-pc-windows-msvc `
  --expected-rust-version 1.95.0 `
  --output-dir .\dist\THIRD_PARTY_LICENSES
```

Linux GNU:

```bash
python3 -B ./scripts/generate-third-party-licenses.py \
  --target x86_64-unknown-linux-gnu \
  --expected-rust-version 1.95.0 \
  --output-dir ./dist-linux/THIRD_PARTY_LICENSES
```

The generator refuses to replace an existing output directory. Re-run the
normal clean package build before regenerating. Each bundle contains:

```text
THIRD_PARTY_LICENSES/
├── THIRD_PARTY_LICENSES.txt
└── manifest.json
```

The manifest records the target, Rust release/host, canonical UTF-8/LF SHA-256
of both lockfiles, and sorted package, role, locked provenance, license,
evidence source, filename, and normalized evidence-body digest metadata. The
output has no timestamp or machine path and must be byte-identical when
generated twice from the same lockfiles, installed package set, and Rust
toolchain. Review it whenever dependencies or the release toolchain change.
Do not hand-edit or commit generated bundles.

The public Windows release is MSVC. A local GNU fallback build has a different
target dependency graph and is not approved for redistribution by the MSVC
bundle.

## Maintainer release workflow

`.github/workflows/release.yml` has a non-publishing `workflow_dispatch` dry
run and a publishing path for strict `vMAJOR.MINOR.PATCH` tags. A publishing
tag must point at the current `origin/main` tip, not merely an older commit in
its history. Before a dry run or tag, synchronize the version in:

- `server/Cargo.toml`;
- the root `codex-web-terminal` entry in `server/Cargo.lock`;
- `web/package.json`;
- the top-level and `packages[""]` versions in `web/package-lock.json`.

Run the complete tag-free build first:

```bash
gh workflow run release.yml --ref main -f version=X.Y.Z
```

That invocation builds, packages, downloads, safely extracts, and validates
both archives but cannot attest or publish a GitHub Release.

The workflow uses Node.js 22.23.1 and Rust 1.95.0, repeats the complete
frontend/Rust validation, builds fresh packages on Windows MSVC and Ubuntu
22.04, generates the target-specific license bundles, and exercises
`scripts/peer-review-regression.py` against each packaged executable. It then
creates exactly:

```text
codex-web-terminal-vX.Y.Z-windows-x86_64.zip
codex-web-terminal-vX.Y.Z-linux-x86_64-glibc.tar.gz
SHA256SUMS.txt
```

Each archive has one versioned root directory. The release tool tests reject
absolute paths, traversal, links, special files, case-colliding entries,
missing files, a wrong license-manifest target, and the wrong PE/ELF
architecture. Both native jobs safely extract their archive and run the
extracted binary's `--version`; a later Linux job independently downloads and
checks both archive layouts. The Linux archive preserves the executable bit
and targets x86_64 glibc 2.35 or newer. The Windows executable is not yet
Authenticode-signed.

After the package and target-specific license inventory are complete, the
workflow runs `scripts/generate-release-package-manifest.py`. It adds the exact
schema-1 `release-package.json` product, version, and target marker required by
the runtime updater. Local `scripts/build.ps1` and `scripts/build.sh` output
must not contain this marker and therefore cannot self-install. The archive
validator binds the marker to the tag version and the expected MSVC/Linux GNU
target.

### Bootstrap and worker compatibility

The complete manually installed v0.2 package supplies the long-lived bootstrap
executable. It may initially serve requests directly, but after the first
built-in update the same root PID launches and supervises verified workers from
`<state-dir>/updates/releases`. A worker never launches the next worker; while
holding the update lock it writes a bounded pending record containing only the
request ID and source/target versions, releases the lock, initiates orderly
shutdown, and exits with the private restart status. The root validates that
exact transition before acting.

The root passes the token only through the worker environment; the worker
consumes/removes it before application threads start. It also supplies a fresh
per-launch readiness nonce through that private environment, which the worker
consumes/removes and returns only in authenticated health. Paths remain out of
the pointer files. The root commits the active/exact-previous version pointer
only after readiness matches both the expected version and nonce. Candidate
failure or commit failure terminates and waits for that process, leaves active
state unchanged, and starts the exact previous executable; rollback must pass
the same readiness check.

This makes the v0.2 root a compatibility and security boundary. Normal release
archives update workers, not the already running root. A change to the
root/worker marker, reserved exit status, pending/active schema, readiness
contract, or supervisor trust logic must include a migration plan and release
notes that require a manual full-archive launcher replacement when the old
root cannot safely implement it. Never remove the bootstrap package used by a
service or launcher.

Only the tag-only final publish job has `contents: write`, `id-token: write`,
and `attestations: write`, and it is attached to the repository's `release`
environment. It downloads the two exact validated artifacts, rejects extra
filenames, writes checksums, creates GitHub provenance attestations, and
uploads to a draft. Before publication, it polls for the three exact uploaded
assets and requires each GitHub `sha256:` digest and size to match the local
Windows archive, Linux archive, and `SHA256SUMS.txt`; the checksum file must
also bind both archives. Only then does it publish. A second bounded poll
requires the resulting release to report `immutable: true` with the same exact
asset set, sizes, and digests. It refuses to overwrite an existing release or
asset.

Repository release immutability must be enabled before publishing a version
that is offered to the built-in updater. GitHub applies that setting only to
future releases. The runtime requires `immutable: true`, the exact uploaded
asset state/name/size, the GitHub `sha256:` asset digest, a matching
`SHA256SUMS.txt` entry, and its own safe package checks. Artifact attestations
remain the stronger manual provenance check and are not falsely represented as
verified by the embedded updater.

The `release` environment must contain
`RELEASE_IMMUTABILITY_READ_TOKEN`: a fine-grained token scoped only to this
repository with **Administration: read** (and the automatically included
metadata read access), no write administration permission, and a maintained
expiration. The workflow uses it only to call the repository immutability
status endpoint before creating the draft and again immediately before
publication. The ordinary short-lived `GITHUB_TOKEN` retains the existing
`contents`, `id-token`, and `attestations` permissions for release publication;
the workflow never enables or disables repository immutability.

The maintainer sequence is:

1. enable repository release immutability and configure the read-only
   `RELEASE_IMMUTABILITY_READ_TOKEN` secret in the protected `release`
   environment;
2. complete and record Windows and Linux validation;
3. review dependency licenses, documentation, and a secret scan of the tree
   and Git history;
4. commit and push the reviewed source to `main`;
5. complete the non-publishing `workflow_dispatch` run;
6. create and push the matching version tag from the unchanged `main` tip;
7. monitor the Release workflow and verify the published checksum,
   attestation, archive layout, and clean-host startup.

Do not upload an old local `dist` directory or create release assets manually.
Configure the `release` environment and a repository rule for `v*` tag
creation before the first tag. If a publish run fails while the release is
still a draft, inspect that draft and its logs; remove only that unpublished
draft while preserving the tag before retrying:

```bash
gh release delete vX.Y.Z --repo bproject07/Codex-web --yes
```

Do not use that retry procedure after publication. An immutable release has
already consumed its tag identity and protected its assets; investigate a
post-publication verification failure as a release incident instead of trying
to overwrite assets or reuse the version.

## Runtime smoke tests

### Basic HTTP and PTY check

First verify Codex itself:

```bash
command -v codex
codex --version
```

Start the server on loopback:

```bash
./dist-linux/codex-web \
  --project "$PWD" \
  --host 127.0.0.1 \
  --port 8787 \
  --no-open-browser
```

Open the authenticated URL printed by the server. A successful runtime check
has all of these properties:

- the page and its hashed JS/CSS assets load;
- `/api/health` reports `sessionRunning: true`;
- the terminal shows the real Codex TUI;
- keyboard input reaches Codex;
- **+ New** can browse a disposable server directory and start the selected
  agent there;
- the created session snapshot reports that directory in both `project` and
  `directoryId`;
- the directory appears in **Recent**, can be starred in **Favorites**, and
  remains after a server restart using the same state directory;
- reconnecting the browser replays existing terminal output;
- terminating or restarting the selected session updates its lifecycle state.

Use `--state-dir` or `CODEX_WEB_STATE_DIR` with a disposable directory for
this check. Give parallel test servers distinct state directories because the
store has no cross-process locking. Do not write test Favorites/Recent into a
live operator profile.

### Updater supervisor regression

Run the deterministic native supervisor regression on Windows and Linux:

```text
python -B scripts/updater-supervisor-regression.py
```

When Windows validation uses an explicit GNU Rust toolchain:

```powershell
python -B .\scripts\updater-supervisor-regression.py `
  --toolchain 1.95.0-x86_64-pc-windows-gnu
```

The fixture uses an isolated temporary state directory and loopback port. It
performs two sequential forward transitions, asserts that the original root
PID survives while each worker PID changes, rejects a nested supervisor,
waits for active state to commit after readiness, then forces a candidate
readiness failure and verifies exact-prior rollback with the active version
unchanged. By default the fixture is both the root and the synthetic worker.

After building a complete package, repeat the same regression with the real
packaged server as the stable root and the fixture only as its supervised
worker:

```powershell
python -B .\scripts\updater-supervisor-regression.py `
  --root-server .\dist\codex-web.exe
```

```bash
python3 -B ./scripts/updater-supervisor-regression.py \
  --root-server ./dist-linux/codex-web
```

The release workflow runs this packaged-root mode on both Windows and Linux.
Run it locally as well whenever updater process, package, or readiness behavior
changes.

The cross-platform registry test in `server/src/registry.rs` uses a synthetic
command that prints its native current working directory; it proves that the
selected path is the PTY child CWD rather than only display metadata.
`server/tests/workspace_api.rs` separately verifies the authenticated endpoint
contracts. `cargo test --all-targets --locked` includes both.

Repeat the workspace runtime on Windows and Linux with native paths. Verify:

- root enumeration (`C:\`-style logical drives on Windows, `/` on Unix);
- one-level listing returns directories only and no nested descendants/files;
- an absolute manual path opens, while a relative path is rejected;
- a deleted or unreadable shortcut cannot launch a PTY;
- malformed or future-version state is preserved under a
  `workspaces.corrupt.<uuid>.json` name and a clean schema loads before the
  normal primary-startup Recent update;
- an opaque ID obtained on one operating system is never treated as portable
  to the other.

### Optional agent-profile smoke test

Agent discovery is enabled by default. Verify the intended entry points first:

```bash
codex --version
claude --version
agy --version
```

Start a disposable loopback server without command overrides:

```bash
./dist-linux/codex-web \
  --project "$PWD" \
  --host 127.0.0.1 \
  --port 8790 \
  --no-open-browser
```

Check `GET /api/agent-catalog` and the **New** picker. Every installed CLI must
be `ready` with a nonempty installed version. A deliberately unavailable CLI
must remain visible as `missing`, show the correct official command for the
server OS, and become ready after a host-side installation followed by
**Refresh** or **Check again**. The check must not execute an installer,
updater, package manager, login flow, or shell expression.

Repeat with explicit `--command`, `--codex-command`, `--claude-command`, and `--agy-command`
paths. A deliberately invalid explicit path must be `misconfigured` and must
not fall back to a PATH entry. Repeat with `--no-agent-auto-detect`: the
primary profile is still resolved and validated, while optional profiles
require explicit overrides.

For an explicit dangerous-mode launch test, add
`--claude-dangerously-skip-permissions` and
`--agy-dangerously-skip-permissions`. These switches pass the fixed upstream
`--dangerously-skip-permissions` argument directly to the respective process
and must remain off for normal validation.

Run this matrix on both Windows and Linux. Validate real native PTY startup for
each installed agent, not only catalog JSON or mocked commands. If an upstream
install location, verification command, update command, or CLI flag changes,
update the implementation and all affected Markdown in the same commit.

### Backend without Codex

To isolate the PTY layer on Linux, use Bash as the command:

```bash
./dist-linux/codex-web \
  --project "$PWD" \
  --command /bin/bash \
  --host 127.0.0.1 \
  --port 8787 \
  --no-open-browser
```

This validates the web terminal and Unix PTY without invoking a model or
creating a Codex conversation.

## Optional browser and mobile regression scripts

The Python utilities in `scripts/` are specialized diagnostics rather than the
normal unit-test path. They require Python 3. Browser-driving scripts also
require Python Playwright and a matching browser installation. Check each
script's help for its platform requirements; some older mobile diagnostics
are Windows-oriented, while the agent-catalog, workspace-picker, and peer
regressions own cross-platform fixtures and cleanup.

Use them only on a disposable test port and never point them at a live
production session. The standard cross-platform validation remains:

- frontend Vitest suite and production build;
- Rust format, test, Clippy, and release build;
- a separate native PTY/browser smoke test.

The Vitest suite includes the `web/src/workspaces/` model and dialog plus API
DTO validation. The Rust suite includes native path encoding, bounded
directory browsing, workspace persistence, registry selected-CWD tests, and
`server/tests/workspace_api.rs`. Keep these cases in the normal suites rather
than making the launcher depend on an optional browser utility.

The normal suites also cover the `@cwt` peer state model, strict frontend DTO
normalization, composer Preview/Return behavior, non-nested close buttons,
dedicated session metadata, per-generation capability rotation/revocation,
loopback bridge authorization, helper argument/HTTP bounds, and an
authenticated API launch of a synthetic second PTY. They also enforce the
256-active-thread broker boundary, the 32-turn per-thread boundary, configured
session-capacity conflicts, and rollback when a fresh reviewer cannot reserve
a slot. Peer changes must keep
those tests in the standard Windows and Linux runs; a real provider review is
a manual smoke because provider credentials, model cost, and approval UI are
not deterministic test dependencies.

That provider smoke must verify that the injected prompt is actually
submitted, not merely visible in the TUI composer. Automation text and its
submit key use separate ordered PTY writes with the configured settle interval
so paste-burst guards cannot consume Enter as part of the pasted text.

The cross-platform peer-review regression owns a disposable server, state
directory, project, and three synthetic agent profiles. It drives the real
native PTYs and private helper over HTTP, revises the preview, returns the
review, confirms that `Recheck` retains the same reviewer terminal and PTY
generation, then closes and purges the peer without stopping the source. It
also verifies that WebSocket restart controls cannot rotate either protected
PTY generation and that the reviewer process tree actually exits. It does not
require Playwright, provider credentials, a repository, or a real agent CLI,
and refuses the reserved live ports `8788`, `8789`, and `8790`.

On Windows:

```powershell
python -B .\scripts\peer-review-regression.py `
  --server .\dist\codex-web.exe `
  --port 8804
```

On Linux:

```bash
python3 -B ./scripts/peer-review-regression.py \
  --server ./dist-linux/codex-web \
  --port 8804
```

For a disposable manual peer smoke:

1. start a package on an unused port and with an isolated `--state-dir`;
2. open **@cwt** from a running source tab;
3. choose a different ready agent and a disposable reviewer directory that is
   not the source directory, then use **Source ready — Prepare handoff** at an
   empty source prompt;
4. verify the preview, then use **Reviewer ready — Send** at an empty reviewer
   prompt;
5. verify a new linked reviewer tab was created in the selected reviewer
   directory and the source directory did not change;
6. use **Source ready — Return**, then issue **Recheck** and confirm the same
   reviewer `terminalId` and `sessionId` remain;
7. close the reviewer and confirm the source is still running;
8. stop the disposable server.

Do not run this smoke against ports `8788`, `8789`, or `8790`, and do not use a
live conversation or private repository as test content.

The workspace-picker regression owns a disposable server, synthetic
directories, isolated state, a synthetic long-running CLI, and its cleanup. It
checks bearer protection, directory-only API contracts, opaque IDs,
Favorite add/persist/delete, the primary default folder and later Recent
updates, the actual PTY child working directory, focus handoff, direct
Favorite/Recent starts, and 360×639 plus 360×345 mobile layouts. It refuses
the reserved live ports `8788`, `8789`, and `8790`:

```powershell
python -B .\scripts\workspace-picker-regression.py `
  --server .\server\target\release\codex-web.exe `
  --port 8803
```

On Linux:

```bash
python -B ./scripts/workspace-picker-regression.py \
  --server ./server/target/release/codex-web \
  --port 8803
```

The cross-platform agent-catalog regression starts a disposable server,
supplies synthetic CLI fixtures, validates the versioned catalog, exercises a
CLI-unavailable-to-ready refresh transition, starts the newly ready agent, and
checks the responsive picker at desktop and 360×639 mobile sizes. It refuses
the reserved live ports `8788`, `8789`, and `8790`:

```powershell
python .\scripts\agent-catalog-regression.py `
  --server .\dist\codex-web.exe `
  --port 8797
```

On Linux, use the packaged Linux executable:

```bash
python ./scripts/agent-catalog-regression.py \
  --server ./dist-linux/codex-web \
  --port 8797
```

The session-tab regression creates four synthetic PTYs and checks desktop
wheel/arrows, mobile swipe, active-tab switching, responsive header height,
the default `4/20` capacity label, and the visibility of **+ New** and
**Manage**:

```powershell
python .\scripts\session-tabs-regression.py `
  --server .\dist\codex-web.exe `
  --port 8798
```

The desktop-slash regression verifies in Chromium and Firefox that `/` reaches
the terminal exactly once when a header control has focus, remains normal text
inside an input, and is not intercepted when modified with Ctrl. It can also
inspect the real Firefox Quick Find bar when Selenium and a Firefox executable
are available:

```powershell
python .\scripts\desktop-slash-regression.py `
  --server .\dist\codex-web.exe `
  --port 8799 `
  --system-firefox "C:\Program Files\Mozilla Firefox\firefox.exe"
```

Omit `--system-firefox` to run only the Playwright Chromium and Firefox checks.

The Android IME regression loads an already-running frontend with a Chrome 150
Samsung-sized mobile context. It replaces the HTTP API and WebSocket with
synthetic in-browser routes, so no generated keystroke can reach a PTY. It
checks duplicate `keyCode 229`, composition commits, deferred composition
Enter, soft-keyboard line breaks, and intentionally repeated input. The script
refuses to run against the live port `8789`:

```powershell
python -B .\scripts\android-ime-input-regression.py `
  --url http://127.0.0.1:8790/
```

## Updating dependencies intentionally

Normal validation uses `npm ci` and Cargo `--locked`. If dependency updates are
the explicit task:

1. update the relevant manifest deliberately;
2. regenerate only the corresponding lockfile;
3. inspect dependency and license changes;
4. run the complete frontend and Rust validation;
5. commit the manifest and lockfile together.

Do not use `npm update` or an unlocked Cargo build merely to make an unrelated
task pass.

## Clean rebuilds

Generated directories are ignored by Git:

```text
web/node_modules/
web/dist/
server/target/
dist/
dist-*/
```

Normal clean rebuild:

```bash
(cd server && cargo clean)
./scripts/build.sh
```

On Windows, use the equivalent PowerShell commands:

```powershell
Push-Location .\server
cargo clean
Pop-Location

.\scripts\build.ps1
```

Do not delete the repository root or an unresolved variable path. Build
scripts deliberately restrict destructive cleanup to their exact output
directory.

## What should be committed

Commit:

- Rust source and `server/Cargo.lock`
- frontend source and `web/package-lock.json`
- build/run scripts
- release workflow and the third-party license generator
- release package marker generator, GitHub release metadata verifier, and
  updater archive validation
- tests
- Markdown documentation
- `LICENSE`

Do not commit:

- `server/target`
- `web/node_modules`
- `web/dist`
- `dist` or `dist-*`
- `.env` files
- runtime logs
- authentication tokens
- credential-bearing launch scripts
- generated `THIRD_PARTY_LICENSES` directories and release archives

Check before committing:

```bash
git status --short
git diff --check
git diff
```

For any code change, also record that the complete Windows and Linux validation
from this guide passed. Review all Markdown changes for current, verifiable
information before committing.

## Troubleshooting build failures

### `npm ci` rejects the lockfile

`web/package.json` and `web/package-lock.json` disagree. Do not bypass this in
a release build. Resolve the dependency change intentionally, regenerate the
lockfile, inspect its diff, and rerun `npm ci`.

### Node.js is too old

Use a version accepted by the current Vite lockfile: `^20.19.0` or
`>=22.12.0`. Node 21 and Node 22.0–22.11 are not in that supported range.
Reopen the terminal so `PATH` is refreshed and verify `node --version`.

### `link.exe` is missing on Windows

Use Developer PowerShell after installing the Visual C++ build tools, or use
the documented GNU Rust target with MinGW. The presence of an MSVC Rust
toolchain alone does not install Microsoft's native linker.

### `cargo` or a component is missing

Install stable Rust, then:

```bash
rustup component add rustfmt clippy
```

### Linux compilation succeeds but an agent session fails

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

The user running `codex-web` must be able to execute the selected CLI and
access the default and selected working directories. The catalog deliberately
does not expose executable paths; inspect resolution on the server with
`where.exe` (Windows) or `command -v` (Unix). An invalid explicit command is
intentionally `misconfigured`; it does not fall back to an auto-detected
executable.

### The workspace tests fail only on one operating system

Directory IDs intentionally encode native Windows UTF-16 or Unix path bytes
and carry a platform prefix. Do not normalize them through UTF-8 strings,
construct them in frontend tests, or expect IDs from one OS to decode on the
other. Use IDs returned by that server unchanged. Keep root enumeration and
case-sensitive/case-insensitive sorting assertions platform-specific.

If persistence tests fail, use an isolated temporary state directory. Check
same-directory create/rename permission. On Unix, new targets should be `0700`
for the directory and `0600` for the file; an existing target must already be
owned by the effective test user and grant no group/other permissions. The
server must reject rather than chmod an unsafe target. A state file that is
corrupt, uses a future version, or is larger than 32 MiB should be
quarantined, not silently rewritten, and a pending write beyond 32 MiB must
leave the current file intact.

### The page says the frontend build is missing

For source-tree execution, build `web/dist`. For a package, copy the contents
of `web/dist` into a directory named `web` next to the executable.

### Build works on one platform but not the other

PTY launch code is intentionally OS-specific:

- Windows resolves `.exe`/`.cmd` and wraps the command with PowerShell or
  `cmd.exe`.
- Unix resolves an executable and starts it directly.

Run both the Windows and Linux Rust test suites after changing
`server/src/terminal.rs`, command resolution, process termination, or
configuration parsing.
