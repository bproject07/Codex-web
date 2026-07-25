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
├── docs/
│   └── screenshots/
└── LICENSE
```

The `web` directory must remain next to the executable. When the backend is run
with `cargo run`, it also searches the source tree's `web/dist` directory.

The build scripts create local packages; the repository does not currently
publish prebuilt binary releases. Before redistributing an executable or
browser bundle, generate and include the complete upstream license and NOTICE
texts described in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). The
informational notice alone is not a complete binary-redistribution license
bundle.

## Supported and validated platforms

The following paths have been exercised:

| Platform | Frontend | Rust build and tests | Native PTY runtime |
| --- | --- | --- | --- |
| Windows 10/11 x86-64 | Yes | Yes, GNU toolchain; MSVC is the recommended target | Yes, ConPTY |
| Arch Linux x86-64 | Yes | Yes, native GNU/Linux target | Yes, Unix PTY |

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
5. update every affected Markdown file in the same commit.

If the local machine cannot run Linux, use GitHub Actions, a Linux VM, or a
Linux host you control. Do not mark the change complete or describe it as
Linux-supported until that validation has actually passed.

Documentation must describe the current source and verified behavior. Check
CLI flags, environment variables, dependency versions, build commands,
package contents, UI labels, platform support, and known limitations instead
of copying potentially stale text from an earlier release.

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
- enough space for `web/node_modules` and `server/target`

Codex CLI is not needed to compile or run the automated unit tests. It is
required for a real application session and for the final runtime smoke test.

| Component | Build machine | Packaged runtime host |
| --- | --- | --- |
| Git | Needed to clone/update | Not required |
| Node.js and npm | Needed for the frontend build | Needed when the installed Codex CLI itself uses Node |
| Rust and Cargo | Needed for backend build/tests | Not required |
| Native C linker/toolchain | Needed for the Rust build | Not required |
| Codex CLI and login | Needed only for a real smoke test | Required |
| Browser | Needed only for browser tests | Required on the viewing device |

Verify the toolchain:

```text
git --version
node --version
npm --version
rustc --version
cargo --version
codex --version
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

The `-Project` directory is the directory in which every managed Codex CLI
session will run.

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
- Codex CLI for a real runtime session

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
positional argument.

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
- reconnecting the browser replays existing terminal output;
- terminating or restarting the selected session updates its lifecycle state.

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
normal unit-test path. They require Python 3, Python Playwright, and a matching
browser installation. Their current fixture paths and process cleanup are
Windows-oriented.

Use them only on a disposable test port and never point them at a live
production session. The standard cross-platform validation remains:

- frontend Vitest suite and production build;
- Rust format, test, Clippy, and release build;
- a separate native PTY/browser smoke test.

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

### Linux compilation succeeds but a session fails

Check:

```bash
command -v codex
codex --version
ls -l "$(command -v codex)"
test -r /dev/ptmx
```

The user running `codex-web` must be able to execute Codex and access the
configured project directory.

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
