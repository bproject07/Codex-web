# Repository Guide for Coding Agents

This file is the operational contract for agents working in this repository.
Read it before editing. Human-facing build and runtime details live in
[BUILDING.md](BUILDING.md) and [OPERATIONS.md](OPERATIONS.md).

## Mission

Codex Web Terminal exposes real Codex CLI, Claude Code, Google Antigravity
CLI (`agy`), and Kilo Code CLI (`kilo`) PTYs through an authenticated web
interface. It must preserve terminal semantics rather than reimplement any
agent UI.

The most important properties are:

- raw PTY bytes remain raw;
- terminal input reaches exactly the selected managed session;
- reconnect replays bounded output and then resumes live output without gaps;
- tokens and terminal content never enter structured tracing;
- Windows and Unix command launch remain independently correct;
- a browser disconnect never terminates a managed PTY;
- production changes do not silently replace or restart a live server;
- agent discovery is read-only and never installs or updates host software;
- workspace browsing returns directories only and never reads file content;
- each selected working directory is validated by the server before PTY
  launch.

## Repository map

```text
.
├── AGENTS.md                  Agent workflow and invariants
├── CLAUDE.md                  Claude-specific pointer and no-local-build rule
├── BUILDING.md                Windows/Linux build, test, and package guide
├── OPERATIONS.md              Runtime, UI, Tailscale, service, and upgrade guide
├── TODO.md                    Deliberately unimplemented, security-scoped ideas
├── README.md                  Product overview, architecture, API, and protocol
├── CONTRIBUTING.md            Contribution workflow and validation contract
├── SECURITY.md                Private vulnerability reporting policy
├── CODE_OF_CONDUCT.md         Community behavior and enforcement
├── THIRD_PARTY_NOTICES.md     Dependency licensing and attribution
├── LICENSE
├── .github/
│   ├── workflows/
│   │   ├── ci.yml             Windows/Linux continuous integration
│   │   └── release.yml        Immutable, attested Windows/Linux releases
│   └── ISSUE_TEMPLATE/        Privacy-safe issue forms
├── docs/
│   └── screenshots/           Sanitized, reproducible product screenshots
├── scripts/
│   ├── build.ps1              Windows production package
│   ├── run.ps1                Windows launcher
│   ├── build.sh               Linux production package
│   ├── run.sh                 Linux launcher
│   ├── fixtures/              Deterministic, synthetic demo PTY
│   ├── agent-catalog-regression.py
│   ├── android-ime-input-regression.py
│   ├── desktop-slash-regression.py
│   ├── mobile-codex-smoke.py  Browser/mobile smoke test
│   ├── mobile-resize-regression.py  Chrome wheel/mobile viewport regression
│   ├── peer-review-regression.py  Native PTY/helper peer workflow
│   ├── updater-supervisor-regression.py  Stable-root update/restart/rollback fixture
│   ├── generate-release-package-manifest.py  Updater package identity marker
│   ├── generate-third-party-licenses.py  Release license/NOTICE gate
│   ├── validate-release-archive.py  Bounded archive/layout/binary validation
│   ├── verify-github-release.py  Exact immutable asset/digest verification
│   ├── session-tabs-regression.py
│   ├── session-cache-regression.py  Synthetic tab retention and routing regression
│   ├── terminal-flow-regression.py  Output flow control, restore retry, and IPv6
│   ├── transfer-regression.py  Context transfer API, helper verbs, claims, lifecycle
│   └── workspace-picker-regression.py  Auth/CWD/persistence/mobile regression
├── server/
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── examples/
│   │   └── updater-supervisor-fixture.rs  Native update/restart/rollback fixture
│   ├── src/
│   │   ├── agents.rs          Agent discovery and install-guidance catalog
│   │   ├── auth.rs            Token validation, throttling, Origin checks
│   │   ├── config.rs          CLI/environment parsing and static asset lookup
│   │   ├── filesystem.rs      Native path IDs and bounded directory browsing
│   │   ├── main.rs            Startup, listener, URLs, graceful Ctrl+C path
│   │   ├── peer.rs            Bounded peer thread/turn state and capabilities
│   │   ├── peer_cli.rs        Hidden loopback helper client
│   │   ├── peer_routes.rs     Public workflow and private bridge routes
│   │   ├── process_tree.rs    Cross-platform child-process containment
│   │   ├── protocol.rs        Browser control-message limits and parsing
│   │   ├── registry.rs        Configurable managed terminal capacity
│   │   ├── routes.rs          Protected HTTP API and static serving
│   │   ├── session.rs         PTY lifecycle, replay buffer, process management
│   │   ├── terminal.rs        Command resolution and platform-specific PTY launch
│   │   ├── transfer.rs        Context transfer coordinator: states, attempts, grants
│   │   ├── transfer_routes.rs Bearer-protected transfer API
│   │   ├── update_bootstrap.rs  Stable-root worker activation and recovery
│   │   ├── update_fs.rs       Hardened updater state and filesystem operations
│   │   ├── update_manifest.rs Signed-package identity and target validation
│   │   ├── updater.rs         Release discovery, staging, and activation API
│   │   ├── websocket.rs       Authenticated attach, replay, input, live output
│   │   └── workspaces.rs      Persistent Favorites and Recent workspace state
│   └── tests/
│       ├── server_contract.rs
│       └── workspace_api.rs   Auth, browsing, persistence, and selected-CWD API
└── web/
    ├── package.json
    ├── package-lock.json
    ├── vite.config.ts
    └── src/
        ├── AgentPicker.tsx    Responsive agent discovery/start dialog
        ├── App.tsx            Main UI, sessions, settings, lifecycle actions
        ├── api.ts             Token/session storage and HTTP API client
        ├── api.token.test.ts  Token continuity and explicit-forget regression
        ├── peer/              Peer composer, state, API, and unit tests
        ├── transfer/          Continue-in composer, controller, API, and unit tests
        ├── sessions/          Header session tabs and navigation helpers
        ├── updates/           Update UI, API model, state, and unit tests
        ├── workspaces/        Folder picker, model, DTOs, and unit tests
        ├── terminal/
        │   ├── TerminalDeck.tsx  Bounded retained views and selected-tab routing
        │   ├── TerminalView.tsx
        │   ├── viewCache.ts
        │   ├── androidImeGuard.ts
        │   ├── MobileToolbar.tsx
        │   ├── mobileKeys.ts
        │   ├── mobileResize.ts
        │   ├── protocol.ts
        │   ├── reconnect.ts
        │   ├── replay.ts
        │   └── settings.ts
        └── styles/app.css
```

Generated `dist*`, `server/target`, `web/node_modules`, and `web/dist` trees are
not source and must not be committed.

## Before changing anything

1. Read the user request and distinguish inspection from authorization to
   mutate source, services, Git, or Gitea.
2. Run:

   ```text
   git status --short
   git branch --show-current
   git log -1 --oneline
   ```

3. Preserve unrelated user changes. Never reset, clean, or check out over a
   dirty worktree merely to simplify the task.
4. Identify whether a live packaged server exists. Source edits do not update a
   running binary. Do not restart or replace a live server unless requested.
5. Never print or commit tokens, passwords, Git credentials, Codex
   authentication data, or credential-bearing local launch files.
6. Do not run builds, compilers, tests, package scripts, or browser regressions
   locally. GitHub Actions is the validation environment for this repository.
   Make source and documentation changes locally, then report validation as
   pending until the GitHub workflow completes.

## Platform launch invariant

`server/src/terminal.rs` has an intentional platform boundary.

Windows:

- searches for `codex.exe`, then `codex.cmd`, then an exact supplied extension;
- invokes `.cmd` through `cmd.exe /d /s /c call`;
- invokes executables through the configured PowerShell/cmd wrapper;
- uses ConPTY through `portable-pty`.

Unix:

- searches for the exact executable in `PATH` or accepts a trusted path;
- runs `command --version` directly during preflight;
- launches the resolved executable directly in the Unix PTY;
- ignores the Windows-only shell selection.

Do not unify these paths through a generic shell command string. The command
configuration intentionally rejects arbitrary shell expressions.
Every Codex profile appends the fixed upstream `--yolo` argument. Before each
PTY launch, a separate bounded `--help` probe of the resolved Codex executable
detects whether it advertises `--no-daemon`; append that fixed argument only
when supported. Missing, failed, timed-out, or truncated help preserves the
legacy `--yolo` launch. This applies to the primary terminal, **New**, restarts,
and dedicated peer reviewers, including executable overrides. Do not infer
support from a version number or retry an already-started PTY. `--no-daemon`
keeps the agent in the managed PTY and avoids Windows Job Object breakaway
failures. Keep process containment enabled. Both probes are read-only, bounded,
and strip the same server secrets and parent-session markers; help text must
never be logged or returned to the browser. Discovery version probes remain
exactly `codex --version`. Optional Claude and AGY dangerous-mode switches append only
the fixed upstream `--dangerously-skip-permissions` argument. Every Kilo
profile appends the fixed upstream `--auto` argument, including the primary,
new tabs, restarts, transfers, and dedicated reviewers. This auto-approves
permissions except explicit `deny` rules in Kilo's own configuration. Version
and discovery probes remain exactly `kilo --version`. Unix and Windows
`cmd` launches keep fixed arguments distinct. The Windows PowerShell wrapper
must encode each fixed argument as an independently single-quoted literal with
embedded quotes escaped. Never accept executable arguments from the browser
API.

Adding or changing an agent kind touches `AgentKind` in `config.rs`, every
exhaustive `match` in `agents.rs`, `terminal.rs`, and `session.rs`, the
`--<agent>-command` plumbing including `restart_arguments()`, the frontend
`AGENT_KINDS` table in `web/src/agents.ts`, the launcher scripts, the
agent-catalog, peer, and session-cache regressions, and every document that
lists the supported agents. A browser bundle must keep a session with an
unrecognized agent value visible and attachable (`agent: null` with a display
label) and must refuse a restart/update restore plan that would drop it; it
must never relabel an unknown agent as a runnable kind.

Agent auto-detection is also a platform boundary:

- it probes fixed executable names and documented per-user locations;
- it runs `--version` directly with bounded output and a timeout;
- an explicit command override is authoritative and must not silently fall
  back to another executable;
- `--no-agent-auto-detect` disables optional discovery without changing
  explicit profiles or validation of the primary profile;
- a missing or misconfigured CLI remains visible as catalog metadata, but is
  never offered as a runnable session profile;
- detection, **Refresh**, and **Check again** are read-only; they do not run an
  installer, updater, login flow, package manager, shell expression, or
  privileged command.

Installation guidance returned by the backend must be static, platform-specific
metadata from an allowlist. The browser may display or copy that guidance, but
must never supply a command or URL for the host to execute. Installation and
updates happen manually on the machine running `codex-web`, under its operating
system account.

Any change to command discovery, `ShellKind`, `ResolvedCodex`, `pty_command`,
preflight, PTY startup, or process termination requires:

- Windows compile/tests;
- Linux compile/tests;
- a real native PTY smoke test on the affected platform.

## Session invariants

- The registry contains one primary session and at most the configured
  capacity: 20 by default, accepted range 1 through 256.
- Capacity includes running, stopped, failed, and exited ordinary entries plus
  dedicated peer reviewers. Only deletion of a removable entry or closure of
  a peer thread releases a slot.
- The validated capacity is immutable for one server generation, is enforced
  under the registry mutex, and is reported by `/api/health`. The frontend
  treats malformed/unavailable health as unknown instead of falsely blocking
  creation, and disables only **New terminal** when a known capacity is
  full.
- The primary entry cannot be deleted.
- `terminalId` is stable for the managed entry.
- `sessionId` identifies one PTY generation and changes on restart.
- A controlled **Restart server** or software update destroys every PTY and
  server-side registry entry. Before either action, the initiating browser tab
  stores a bounded `sessionStorage` restore plan containing only each ordinary
  non-primary tab's agent, opaque directory ID, order, and selection. Once the
  server version is an allowed source/target and the primary `terminalId` has
  changed, it recreates those tabs as fresh PTYs. New entries receive new
  `terminalId` and `sessionId` values.
- Never describe restored tabs as resumed processes. PTY output, shell/agent
  conversational state, stopped/exited lifecycle state, and in-memory `@cwt`
  reviewer threads are not restored. Dedicated peer sessions are excluded.
  If the initiating tab cannot persist a non-empty restore plan, it must refuse
  the restart/update instead of silently losing the ordinary tabs.
- Same-version server restart is enabled only when the serving process is
  direct or its stable root explicitly passed the private restart-capability
  marker. A worker updated under an older root must report restart as
  unsupported and remain running. The operator must install the complete new
  release as the stable launcher before using this action.
- Output from an old generation must never be appended to the active buffer.
- The reader may drain final PTY bytes after process exit or termination is
  observed. Retain those bytes for the same generation without changing its
  lifecycle status; a restarted generation must still reject them.
- A browser attach changes only the displayed session.
- Managed children and version probes must not inherit the parent-session
  markers `CODEX_THREAD_ID` or `CLAUDECODE`; each terminal must start
  independently even when the server was launched from another agent session.
  Do not remove provider credentials or unrelated environment variables.
- When `--new-session-command` is configured, the primary terminal uses
  `--command`; **New** uses the distinct new-session command only when the
  primary agent is selected. Other agent cards use their own explicit override
  or default command. All commands follow the same platform resolution and
  preflight rules.
- Reconnect changes only the WebSocket attachment.
- Restart terminates and recreates the selected PTY.
- Terminate stops the process but keeps the managed entry.
- Deleting a non-primary session terminates it and removes its entry.
- A browser close or network interruption must not stop the PTY.

## Peer workflow invariants

- A new peer thread always owns a newly created dedicated peer PTY. Never
  route it to, inspect, or reuse an ordinary session.
- A follow-up or `Recheck` reuses only that thread's still-running dedicated
  reviewer. A changed `sessionId`, exit, restart, deletion, or server restart
  means its conversational context is no longer valid.
- Peer reviewers count toward the same configured session capacity. Never
  evict or auto-close another session to make room. Existing follow-ups may
  remain available at capacity because they reuse their reviewer.
- Disable only creation of a fresh reviewer when capacity is full; keep
  existing peer-thread follow-ups usable. One thread is bounded to 32 turns,
  and active broker threads are bounded by the supported maximum capacity of
  256. Keep both boundaries documented and covered by tests.
- A new peer request may select the dedicated reviewer's working directory and
  otherwise defaults to the source directory. The selected opaque directory
  ID is decoded, canonicalized, and checked again immediately before the
  reviewer starts. Git, TFS, or any repository is optional context, never a
  workflow requirement.
- Raw PTY output, ANSI text, cursor position, silence, and CPU state are not
  agent-completion signals. Peer artifacts move only through the bounded
  private helper protocol.
- The helper listener remains loopback-only and separate from the configured
  public listener. It receives no browser bearer token. Remove
  `CODEX_WEB_TOKEN` from every managed PTY and version-probe environment.
  Shutdown must disable new capability activation and revoke all capabilities
  before releasing this listener; an unexpected bridge exit fails the public
  server closed.
- Every real PTY generation receives a new random capability. Strip inherited
  `CWT_PEER_*`, `CWT_TERMINAL_ID`, and `CWT_SESSION_ID` values before launch
  and version probes; revoke the active capability on spawn failure, exit,
  terminate, restart, or deletion.
- A capability authorizes only the active source or reviewer role for its
  linked current turn. Compare it in constant time and never put it in argv,
  URLs, API responses, logs, diagnostics, or screenshots.
- New-thread request bodies are bounded to 256 KiB so maximum native Windows
  directory IDs fit; instructions remain independently bounded to 4 KiB.
  Handoffs and responses are non-empty UTF-8, bounded to 64 KiB, in memory
  only, and never logged. Normalize their line endings and reject terminal
  control characters other than line-feed and tab at both broker input and
  helper output boundaries.
- Preview, dispatch, and return remain explicit user actions. Automation
  requests require `sourceReady: true` or `reviewerReady: true`, are bound to
  the exact `sessionId`, and must be initiated only at an empty agent prompt.
  Do not invent a generic PTY "idle" detector or auto-repeat an ambiguous
  delivery.
- Restart/terminate is rejected for a peer reviewer. Restart, terminate, and
  deletion of a source are rejected while it owns an open peer thread.
- Provisioning and closing a peer thread are cancellation-owned transactions:
  once started, they finish or perform compensating rollback even if the
  browser request disconnects. Generic lifecycle paths must never mutate peer
  sessions; internal cleanup must use the exact thread, source terminal, peer
  terminal, and PTY generation identity. A provisioning lease owns the
  reviewer identity before reservation/start and blocks Close until source
  prompt delivery finishes. Close succeeds only after reviewer process exit is
  confirmed; a termination failure keeps the exact session and thread
  retryable.
- `@cwt` is a normal accessible launcher and composer. Do not intercept,
  buffer, erase, or replay literal `@cwt` keystrokes in `TerminalView` or the
  Android IME guard.
- Queued return prompts enter `returning`; only the exact source generation's
  private helper acknowledgement after successful output/flush marks them
  `returned`. A receipt may arrive before the return-notification lease ends;
  preserve it without releasing that lease early. Block follow-ups and normal
  Close while receipt is pending. Explicit discard must identify the current
  unread turn, and a failed close must preserve its response. Source exit
  fails a pending return; reviewer exit must not discard its stored response.
- Peer creation must not remain resubmittable while its session-list refresh
  is pending. Returning from the reviewer folder picker, including Cancel,
  restores focus inside the composer and preserves the instruction.
- Treat an automation prompt as one ordered queue transaction, but write and
  flush its text before a provider settle interval and a separate Enter write.
  Never concatenate the submit key with fast raw prompt bytes: real TUIs can
  classify that burst as paste and turn Enter into a newline. Browser input
  must not interleave between the prompt and its submit key.

Changes to the peer broker, helper, automation prompts, session purpose, or
peer routes require `scripts/peer-review-regression.py` on both Windows and
Linux in addition to the normal frontend, Rust, and package checks.

## Human sharing and recording invariants

- Guest credentials are separate from owner and private peer capabilities.
  Each single-use invitation and admitted grant is pinned to one ordinary
  terminal and PTY generation. Never let guest credentials reach owner APIs.
- Names are server-fixed to a guest identity and remain reserved after
  revocation. Presence describes browser visibility/connection, never proof
  of reading or authenticated human identity.
- Guest WebSockets accept only authentication, bounded ACK, ping and presence.
  No guest input, resizes, terminal replies, lifecycle or folder operations.
  Revocation must cancel an in-flight output write as well as an idle socket.
- Proposals are bounded, expiring and revision-bound. The SQLite actor owns
  admitted decisions after HTTP cancellation. Commit approval intent before
  registry admission; perform no disk I/O under registry/session locks.
  Registry admission checks generation, lifecycle, peer and transfer claims.
  Exact text and Enter are one ordered queue item with separate flushes.
  A missing or ambiguous receipt is uncertain, never automatically resent.
  Receipt persistence waits through metadata queue saturation and owns retries
  independently of the HTTP request. Never retry PTY admission for a receipt.
  Pending owner edits must not appear in guest proposal responses.
- SQLite holds explicit opt-in terminal recordings and proposal text outside
  tracing. Secrets are hashed. An exclusive store lease and private files,
  including WAL/SHM, are required. Storage errors must not block raw PTY I/O.
  Full queues, quota exhaustion, failed captures and late recording start are
  visible as incomplete history.
- Archive events preserve raw bytes and geometry per generation. Final reader
  drain, not process exit, ends the recording. Shutdown waits for readers and
  an ordered capture barrier. Old generation bytes never enter the new live
  buffer. Server restart revokes grants and never replays approval intents.
  Explicit Stop recording persists across terminal restarts. Re-enable retains
  sequence identity and never backfills the paused interval. Reader completion
  is tracked separately from process exit. Drain sessions concurrently.
- Guests can page only their current granted archive. Owner archives survive
  restart and revocation. Deleting a finished recording cannot erase approval
  identities or release a guest's reserved name.
  Only explicit retired-terminal cleanup may remove those ledgers, after the
  terminal is absent from the registry and no delivery receipt remains pending.
  Renewing a grant revokes its predecessor without changing the guest's name.
- Changes require `scripts/sharing-regression.py --browser` on Windows and
  Linux, alongside peer, transfer, output-flow and normal Rust/frontend checks.
- Sharing storage changes also require `scripts/sharing-storage-regression.py
  --browser --report storage-metrics.json` on Windows and Linux. Preserve exact
  retained bytes, preview-bound cleanup, durable deletion requests, bounded actor
  batches and guest/approval ledgers. Automatic retention is opt-in and uses
  completion age; live generations and pending receipts are never eligible.
  Maintenance admission must exclude enabled recorders and new sharing mutations
  while retaining ownership of receipt persistence and HTTP-cancelled work.
  Windows creates new state directories with private ACLs; existing ACL repair
  is an explicit owner action restricted to the validated configured directory
  and fixed sharing files. Refuse untrusted owners, links and another store lease.

## Transfer invariants

A context transfer ("Continue in <agent>") moves one written Markdown handoff
from an ordinary source session to an ordinary target session. It is a
separately typed transaction (`server/src/transfer.rs`) that shares the peer
broker's loopback listener, per-generation capabilities, artifact validation,
and ordered automation-prompt queue, but never a peer thread, a dedicated
reviewer, or peer Close.

- The target is always an ordinary `Interactive` session: either a fresh one
  reserved for the transfer or an existing running tab (including the
  primary). It is published in the registry and user-owned from reservation
  onward; a disconnected browser request never rolls back an exposed target,
  and a failed start removes the entry only when the rollback confirmed
  process termination. Otherwise the transfer enters `cleanup_pending` and
  only an explicit cleanup with the exact terminal and generation retries it.
- Registry participant claims (`participant_claims`) exclude lifecycle
  mutation and peer provisioning on a terminal while it is a transfer source
  (during automated preparation only) or a bound target, and exclude transfer
  claims while peer provisioning is pending or a dedicated reviewer exists.
  A terminal holds at most one transfer claim and never both kinds. Dedicated
  peer reviewers are never transfer participants. Manual drafting from a
  stopped or busy source takes no claim.
- Automated preparation requires `sourceReady: true`, the observed running
  source generation, and the exact `attemptId` on the helper submission. A
  manual edit withdraws acceptance of the pending attempt; queued prompt bytes
  are not recalled, and a late submission is rejected rather than merged. A
  source exit during preparation returns the transfer to `preview` with the
  draft intact.
- Target requests carry a client-chosen `targetRequestId`. The same id with the
  same parameters returns the saved outcome (or waits on the in-flight start);
  different parameters conflict; a new id is admitted only after the previous
  attempt settled. At most 8 attempts per transfer. The ledger of transfers
  and attempts is bounded to 4,096 records per server generation and fails
  closed; active transfers holding text are bounded to 256. Nothing is
  evicted, so a delayed duplicate request can never create a second target.
- Delivery is an explicit user action carrying `targetReady: true`, the
  observed target `sessionId`, and the expected draft revision. The approved
  revision is frozen; the prompt is enqueued synchronously inside the
  coordinator's critical section, and a definite queue rejection leaves the
  transfer in `target_bound` with the draft intact. After acceptance the
  outcome is "unconfirmed" until an authenticated receipt or an observed
  target generation end. There is no timer, no idle detector, and no
  automatic resend.
- The helper verbs `transfer-submit`, `transfer-receive`, and `transfer-ack`
  use their own operation namespace; a transfer id is never interpreted as a
  peer turn and no failure falls through to peer authorization. Role is
  checked per transfer and exact generation: only the source generation may
  submit, only the bound target generation may receive, acknowledge, or query
  status. The helper writes and flushes the artifact before acknowledging, and
  retries only the receipt-only acknowledgement a bounded number of times.
  Acknowledgement retires the content grant; later receives are status-only.
- A target that ends before `delivered` settles the attempt, releases its
  claim, and returns the transfer to `preview` with the draft kept and
  `delivery: target_ended`; the user chooses a new target and delivers again
  explicitly. Abandon from `delivering`/`awaiting_receipt` is a discard that
  withdraws acceptance of later helper calls. Artifact text is erased on every
  terminal state; the ledger stores identities and outcomes only.
- Lock order: transfer coordinator -> session registry -> session record. The
  peer broker lock is never nested inside the coordinator; capability
  authentication releases it before dispatch, and revocation reaches the
  coordinator through an observer invoked outside the broker lock. Shutdown
  disables transfer admission and fails non-terminal transfers before the
  loopback listener is released.
- Browser: the entry point is **Continue in…** in **Manage sessions**, not a
  fifth general Menu action. The composer reuses the peer dialog shell, offers
  manual entry from the start (also for stopped sources), selects the target
  tab behind the dialog so the user can finish sign-in or folder trust, and
  never describes the result as a resumed process.
- Hiding the composer preserves the transfer. Reopening from either participant
  discovers active transfers from the server, including after a page reload.
  Save and Deliver use the loaded draft revision and remain disabled until it
  matches the metadata revision; stale draft responses cannot replace newer
  state or restore content after completion.
- Generation exit updates the session record before the revocation observer
  runs outside that lock. Target-start completion rechecks the current
  generation under the coordinator lock, and failed rollback records the actual
  generation for exact-identity cleanup.

Changes to the transfer coordinator, registry claims, helper verbs, bridge
dispatch, or transfer routes require `scripts/transfer-regression.py` **and**
`scripts/peer-review-regression.py` on both Windows and Linux.

## Workspace invariants

- `--project` / `CODEX_WEB_PROJECT_DIR` selects the canonicalized default
  directory for the primary terminal and for API requests that omit
  `directoryId`. It is not a filesystem sandbox.
- An authenticated browser may browse and launch in any absolute directory
  readable by the operating-system account running the server.
- Directory listings are non-recursive, contain directories only, are sorted,
  and return at most 10,000 entries with `truncated: true` when more exist.
- Session-create, directory list/resolve, and Favorite-upsert JSON bodies are
  independently capped at 256 KiB.
- The manual path endpoint accepts only an absolute server-side directory
  path. Never reinterpret it as a client-device path.
- A directory ID preserves native Windows UTF-16 or Unix path bytes for API
  round trips. It is an opaque transport encoding, not authorization,
  encryption, signing, or a security boundary.
- Decode and canonicalize a selected ID again immediately before use. Reject
  missing, non-directory, inaccessible, relative, or wrong-platform values.
- A browser may select only a directory and an allowlisted agent. It must
  never supply an executable, argument list, shell expression, or environment
  mutation through workspace or session APIs.
- Favorites and Recent are server-side state, not browser-local authority.
  They may become stale and must not bypass launch-time filesystem checks.
- Favorites are bounded to 100. Recent is deduplicated by native directory,
  newest first, and bounded to 30.
- Successful primary startup, new-session creation, and restart update Recent
  and the matching favorite's preferred agent. Failure to persist that
  convenience state must not turn an already-running PTY into an API failure.
- Workspace state is versioned and limited to 32 MiB (33,554,432 bytes) on
  both read and write. Invalid, unsupported, or oversized state must be
  quarantined rather than overwritten; an oversized pending write must be
  rejected before replacing either the file or in-memory state.
- Persist with a same-directory atomic replacement. A state directory must be
  a dedicated real directory, never a filesystem root, broad account/system
  directory, symlink, or Windows reparse point; the state file must be a
  regular non-link file.
- On Unix, create the dedicated state directory with mode `0700` and state
  files with `0600`. Existing targets must already be owned by the effective
  user and grant no group/other permissions. Reject unsafe existing targets;
  never silently `chmod` operator-managed paths.
- Persistence has an in-process mutex but no cross-process lock or merge.
  Concurrent server instances must use distinct state directories.

The server retains up to 16 MiB and 16,384 chunks of raw output per session.
A newly attached browser receives at most the newest 4 MiB. WebSocket replay ordering and
sequence checks exist to prevent replay/live gaps. Preserve those properties
when changing batching or reconnect behavior.

Browser connections negotiate `flowControl=1` and acknowledge processed binary
bytes. The outstanding window is 4 MiB, including replay; acknowledgements must
never exceed bytes sent. Flow control is per attachment and never stops a PTY.
Run `scripts/terminal-flow-regression.py` on Windows and Linux for output changes.
Restore requests use the original terminal UUID as `restoreRequestId`; the
registry owns lookup/start/commit beyond cancellation. Its 4,096-entry history
keeps deletion tombstones and refuses new identities when full. Workspace writes
likewise own the lock through both disk replacement and memory commit.
An accepted deletion during restore startup must retain that tombstone even if
startup rolls back or the delete request disconnects. Failed termination keeps
the same session retryable. Removing a deleted tab from a browser restore plan
must also remove its mapping from the in-flight progress map.
The restore table lock must be released before PTY startup; same-identity retries
wait only for that reservation. Only an explicit `restore_deleted` response may
skip a saved tab. Capacity, parameter conflicts, and full restore history must
retain pending tabs. Concurrent peer polls validate conditional responses against
their own requested snapshot and must not overwrite a newer request's cache.

## Browser and mobile invariants

- xterm.js owns ANSI/VT interpretation and terminal scrollback.
- Settings **Keep terminals when switching tabs** defaults to enabled, also
  for saved preferences missing this field. Retain at most six visited xterm
  views and their WebSockets; evict the least recently visited inactive view
  without stopping its PTY. Disabling retention releases hidden views while
  preserving the selected view. Content stays in memory only. A changed PTY
  generation or deletion invalidates that view. Only the selected view may
  accept new user input, focus, or send viewport resizes. An IME transaction
  accepted before switching may finish only in its original view; block fresh
  hidden DOM input before the IME guard, and allow only guard-owned completion
  events through that block. xterm protocol replies
  from background output stay routed to their own session. Background session
  metadata, errors, and connection status must not replace selected identity.
  Re-selecting the active tab must not reset connection status. Cover retention,
  opt-out, eviction, input/resize routing, restart, and removal with
  `scripts/session-cache-regression.py` on Windows and Linux in CI.
- Tab selection must not discard a pending session-list update. Apply its
  membership while preserving a newer user selection. A PTY generation change
  replaces the view once; do not additionally force a reconnect on the restart
  HTTP reply. Confirmation dialogs suppress terminal autofocus and desktop
  slash routing just like the other dialogs.
- A retained view retries an HTTP 429 attach rejection after 60 seconds,
  including while hidden. HTTP 401 stops automatic retries; switching away
  and back after a 60-second cooldown permits one new attempt. Switching
  tabs must not bypass either cooldown. Token changes retry immediately, and
  view disposal cancels all pending retries.
- Do not parse or rewrite Codex terminal text in the frontend.
- Do not enable `convertEol`; the PTY owns line endings.
- Resize messages must remain bounded to 20–500 columns and 5–300 rows.
- Mobile viewport changes are asynchronous on Android and iOS.
- Token entry must scroll within the available viewport height so validation
  errors, larger text, and the software keyboard cannot hide Connect. The
  agent picker keeps only its title and Refresh/Close controls outside the
  shared scroller for folder details and agent cards. Initial focus stays in
  the header so folder details remain visible. Main touch controls retain
  at least 44 px targets even under short-viewport rules; keep the tab scroller's
  narrow overlay arrows out of that minimum-width rule. Cover these cases
  with the synthetic mobile layout checks in `session-cache-regression.py`;
  Chromium viewport emulation is not real Samsung Internet/IME validation.
- Avoid resize loops that repeatedly call FitAddon while the visual viewport
  and hidden xterm textarea are moving.
- The mobile toolbar order starts with Enter and arrows, then history/control
  keys.
- Session tabs remain horizontally scrollable on overflow. **@cwt** stays
  outside the inner tab scroller so it remains discoverable. A desktop header
  reads left to right: identity area, then the
  ellipsis **Menu** trigger directly after the status dot, then the
  left-aligned tabs and **@cwt**. An expanded mobile header instead begins
  with **Menu**, then its collapse disclosure, then the identity; preserve the
  path's rightmost project name when it ellipsizes. On touch screens up to
  900 px, a ≥44 px disclosure may hide the identity/Menu row; the default and
  last user choice are browser-local. Its collapsed row retains the
  connection-status dot, disclosure, tabs, and **@cwt**. The tab strip is the
  only horizontal scroller in the header; the Menu trigger and disclosure
  must never sit inside it, and the Menu popover must not be clipped by it.
- The Menu trigger is titled and labelled "Menu", uses the ARIA menu-button
  pattern (`aria-haspopup`, `aria-expanded`, `aria-controls`, `role="menu"`
  with `role="menuitem"` entries, arrow-key navigation, Escape returns focus
  to the trigger), keeps a ≥44 px touch target at phone widths, and holds
  exactly the general actions: **New terminal** (capacity-disabled with its
  explanation), **Settings** (with the update badge), **Manage sessions**
  (with the `N/M` count), and **Full screen**. Settings itself keeps only
  preferences, updates, diagnostics, restart/terminate, and Forget token.
- The header identity shows live context, not branding: the active session's
  full native project path as visible DOM text (it may only visually
  ellipsize; the heading `title` repeats the complete value), the selected
  session's agent label, and a connection-status dot with a visually hidden
  text equivalent (color alone is insufficient). There is no manual reconnect
  control anywhere — WebSocket reattachment is automatic. Do not present the
  agent as an LLM "model" — the backend has no structured model field — and
  never derive identity from terminal text.
- On desktop, an unmodified `/` pressed outside an editable control or dialog
  is routed to the connected terminal and its browser default is suppressed.
  Do not intercept modified shortcuts, form input, dialogs, IME composition,
  or coarse-pointer/mobile input.
- Mobile `PgUp` and `PgDn` send the standard terminal Page Up/Page Down
  sequences to the selected PTY, just like the other terminal-input buttons.
  `Top` and `Live` manipulate only xterm's client scrollback.
  Scroll to these boundaries by the full buffer length so a stale relative
  viewport position after hidden output cannot leave the view short of an edge.
  Preserve an explicit boundary request through pending fit and viewport render
  callbacks. Cancel it on newer manual scrolling/input, tab hiding, or disposal.
  A skipped or failed trailing fit must also settle the request, so a later
  unrelated resize cannot revive it.
- A desktop session-tab tooltip contains only that session's full project path.
  Keep the lifecycle status in visually hidden accessible text rather than
  repeating the tab's visible agent/name information in the tooltip.
- The Android IME guard translates Gboard/Chrome replacement edits into the
  minimal terminal Backspace-plus-suffix sequence and restores xterm's helper
  textarea baseline so its keyCode-229 fallback cannot duplicate the word.
  Never record the replaced text in diagnostics.
- On coarse-pointer devices, xterm's custom vertical scrollbar keeps a narrow
  visible thumb but uses a 28 px draggable target. Desktop keeps xterm's
  default width. Preserve that desktop default by omitting the
  `overviewRuler` option entirely; never pass `overviewRuler: undefined`,
  because xterm reads its `.width` during construction.
- Diagnostics must not include token, keystrokes, or terminal content.
- Screenshots must use only synthetic fixtures. They must not contain account,
  organization, company, device, or host names; credentials; tokens; private
  addresses; terminal history; or personal filesystem paths.

For mobile changes, run unit tests and the relevant Python browser regression
where the required browser tooling is available.

Changes to folder selection, native path encoding, Favorites/Recent, or
per-session working directories also require on both Windows and Linux:

- frontend workspace model/picker and API-client tests;
- Rust filesystem, workspace-store, registry, and authenticated API tests;
- a disposable runtime check that browses a synthetic directory, launches a
  PTY there, and verifies its native working directory;
- persistence/reopen coverage, including a stale path and corrupt-state
  quarantine where the affected code changed.

## Authentication and security invariants

- HTTP API uses `Authorization: Bearer`.
- WebSocket uses the URL query token because browser WebSocket APIs cannot set
  the Authorization header.
- WebSocket Origin validation must remain enabled.
- Authentication comparison remains constant-time for equal-length tokens.
- Repeated failures remain throttled per address.
- The default project directory is canonicalized at startup. Workspace APIs
  may select other readable absolute directories after bearer authentication;
  `--project` is not an access-control boundary.
- All filesystem and workspace endpoints remain bearer-protected.
- Do not describe opaque directory IDs as a security mechanism. Authorization
  comes from the bearer token and the server account's operating-system
  permissions.
- Workspace state contains filesystem paths, reversible path IDs, and usage
  history. Keep it and its backups out of logs, diagnostics, screenshots,
  fixtures derived from real systems, and public issue reports.
- Do not add public-tunnel or router-port-forward automation.
- Do not log request URLs because the first URL contains the token.
- Do not log terminal input, output, or Codex credentials.
- Do not add silent browser-triggered CLI installation or update. Installing a
  host executable is a separate, security-sensitive operator action.
- Startup stdout intentionally prints the authenticated URL; document that
  redirected stdout or a service journal will retain this credential.
- Any remote-access documentation must prefer loopback or Tailscale and explain
  the risk of binding `0.0.0.0`.

## Required validation

Every code change, bug fix, refactor, or dependency update requires validation
on both Windows and Linux through GitHub Actions. This rule applies even when a
change looks platform-independent. Agents must not execute the commands in this
section locally; they define the CI validation contract. A Windows-only result
is incomplete, and platform results must never be skipped or simulated.

### Frontend

From `web/`:

```text
npm ci
npm test
npm run build
```

Do not run `npm update` unless dependency updates are the explicit task.

### Rust

From `server/`:

```text
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
```

On Windows without `link.exe`, use the installed GNU Rust toolchain and MinGW:

```text
rustup toolchain list
rustup component add --toolchain <installed-gnu-toolchain> clippy
rustup run <installed-gnu-toolchain> cargo test --all-targets --locked
rustup run <installed-gnu-toolchain> cargo clippy --all-targets --locked -- -D warnings
rustup run <installed-gnu-toolchain> cargo build --release --locked
```

### Package validation

Check:

- executable exists and runs `--version`;
- adjacent `web/index.html` exists;
- hashed JS and CSS assets referenced by `index.html` exist;
- root HTTP request returns 200;
- `/api/health` distinguishes command discovery from a running PTY;
- WebSocket input produces PTY output;
- `scripts/peer-review-regression.py` passes against the packaged executable
  on Windows and Linux when peer code is present or changed;
- `scripts/transfer-regression.py` passes against the packaged executable on
  Windows and Linux when transfer, registry-claim, or helper code changed;
- a redistribution archive contains a target-specific generated
  `THIRD_PARTY_LICENSES` bundle whose lockfile hashes and package inventory
  match that build.

### Documentation validation

When CLI flags, environment variables, UI labels, session semantics, build
outputs, or supported platforms change, update all affected Markdown files.
Documentation must reflect the current source and the latest verified
Windows/Linux behavior in the same commit. Recheck commands, versions,
platform claims, package layouts, UI labels, and limitations; do not copy
stale information forward.

Agent installation and update commands are time-sensitive external facts.
Before changing them, verify the current official OpenAI, Anthropic, and Google
documentation. Never infer a package name, download URL, checksum, or
permission flag from memory. Keep manual-install guidance separate from
read-only auto-detection.

Search for stale platform-specific text:

```text
rg -n "Windows|Linux|ConPTY|PTY|build.ps1|build.sh|--shell|CODEX_WEB_" *.md
```

## Build and release paths

Windows:

```text
.\scripts\build.ps1
dist\codex-web.exe
```

Linux:

```text
./scripts/build.sh
dist-linux/codex-web
```

The executable and `web` directory are one release unit. Do not deploy only
one half.

Official release archives also contain `release-package.json`. It is generated
only by the release workflow after the complete package and license inventory
exist; local `dist`/`dist-linux` output must remain ineligible for
self-install. Application updates use the fixed official repository, exact
target asset, immutable stable release metadata, GitHub and checksum-file
SHA-256 agreement, bounded safe extraction, and side-by-side state-directory
releases. Never accept a browser-supplied repository, URL, path, checksum,
command, or executable. Never overwrite the running package.

An update check is read-only. Applying an update requires explicit
session-termination confirmation and orderly public/peer/PTY shutdown. The
manually installed v0.2 executable is the stable root supervisor: its PID
survives worker updates, it alone launches generations, and workers must never
form a nested supervisor chain. Service `ExecStart` continues to name that
bootstrap package; built-in cleanup must never replace or delete it.

`pending.json` contains only schema, request ID, source version, and target
version. Derive package paths from the private state directory. Never persist
tokens, URLs, paths, commands, checksums, arguments, or environment values in
pending/active state. Pass the current token to a worker only through
`CODEX_WEB_TOKEN`, never argv or update files, and consume/remove it before
application threads start. Pass a fresh per-launch readiness nonce through the
private worker environment and consume/remove it there as well. A worker may
request another update generation only with reserved exit status `75` and a
matching pending source. A controlled same-version **Restart server** uses the
distinct reserved status `76`; the stable root must relaunch the exact current
generation and require the same authenticated nonce-bound readiness checks.
It must not create or mutate an update pointer. Pointer writes must remain
private and atomic. Resume only a strict pending transition that matches the
known active generation; clear an already committed pending record and
quarantine malformed or stale state.

Persist the matching pending transition while holding `update.lock`, release
that lock before activation delivery, and only then begin orderly shutdown.
The root must validate the deterministic package again, launch the candidate,
and require direct authenticated local readiness with proxies disabled, a
bounded response, the exact expected server version, and its per-launch nonce
before atomically changing the pointer that names the active and exact previous
versions. Failed validation, launch, readiness, or pointer commit must
terminate/wait the candidate, keep the pointer unchanged, and restart the exact
prior executable; rollback is successful only after the prior generation also
passes readiness. Preserve one rollback worker release.

Treat the root/worker marker, both reserved exit statuses, pending/active
schemas, and readiness exchange as a stable security protocol. A change may
require a manual full-archive launcher replacement because ordinary worker
updates cannot replace the running root. The v0.1-to-v0.2 transition is always manual.
The private same-version restart capability must be stripped from managed PTYs,
agent probes, updater probes, diagnostics, and logs.
For systemd keep `ExecStart` on the bootstrap package with
`Restart=on-failure`, `KillSignal=SIGINT`, and `KillMode=control-group`.

Any change to this boundary requires malicious-archive tests and
`scripts/updater-supervisor-regression.py` plus native packaged
update/readiness/rollback validation on Windows and Linux. Assert that the root
PID remains stable across two worker generations, no nested supervisor appears,
active state commits only after readiness, and a failed candidate returns to
the exact previous worker.

The Windows `dist` and Linux `dist-linux` directories are ignored artifacts.
Never infer that their contents belong in a commit.

`.github/workflows/release.yml` supports a non-publishing manual dry run.
Publishing remains tag-only from a strict `vMAJOR.MINOR.PATCH` tag at the
current `main` tip. The version must match `server/Cargo.toml`, the root Cargo
lock entry, `web/package.json`, and both package-lock root versions. Release
archives have one versioned root and exactly one Windows x86_64 or Linux
x86_64 glibc package.

`scripts/generate-third-party-licenses.py` is a fail-closed release gate.
Never weaken its reviewed license allowlist, missing-text checks, special
NOTICE assertions, lockfile hashes, deterministic ordering, or target binding
to make a dependency update pass. Review the new dependency and its actual
license files. Generated bundles and archives remain ignored output; commit
the generator/workflow changes, not the generated result.

Release actions remain pinned to full commit SHAs. Build jobs have read-only
repository permission. Only the final job may write release contents and
attestations; it must reject extra assets and existing releases, publish from
a verified draft, use the `release` environment, and never overwrite an
asset. Each native archive and both downloaded artifacts must pass
`scripts/validate-release-archive.py`; keep its safe-path, complete-layout,
target-manifest, architecture, extraction, and native version checks.

## Live-server safety

- Use a disposable high port for browser and PTY tests.
- Check the exact listener and PID before starting or stopping anything.
- Never kill a production process by a broad name match.
- Do not replace a running package in place.
- Source edits do not hot-reload a packaged backend.
- Restarting the Rust server destroys the in-memory registry and every live
  PTY; a stable token does not preserve those processes.
- Stop temporary services and verify their ports are closed unless the user
  explicitly asked to keep them running.

## Cross-layer changes

The WebSocket control and replay protocol is implemented in both Rust and
TypeScript. A protocol change must update both sides and their tests in the
same change. Do not accept a backend-only or frontend-only protocol mutation.

Changes to resize, replay, mobile focus, or viewport handling require the
frontend unit suite, production build, and the relevant real-browser
regression. Multiple clients attached to one PTY share both input and the PTY
size, so competing viewports are an intentional operational limitation.

## Git workflow

- Do not commit or push unless the user requests it.
- Do not change repository visibility or collaborator permissions unless
  explicitly requested.
- Before staging, inspect `git diff --check`, `git diff`, and `git status`.
- Stage only intended source, tests, scripts, and documentation.
- Preserve the repository's current visibility unless explicitly told
  otherwise. A public release requires a secret scan of the current tree and
  Git history before push.
- A user who only needs to clone should receive `read`, not `write` or `admin`.
- After push, verify the remote branch commit matches local `HEAD`.

## Definition of done

A change is complete only when:

1. The requested behavior is implemented in source, not only in generated
   output.
2. Relevant frontend and Rust tests pass on both Windows and Linux for every
   code change.
3. Both OS-specific paths are checked, including for changes that appear
   platform-independent.
4. A real PTY/runtime test is performed when process launch or terminal I/O
   changed.
5. Documentation contains current, verified information and matches the actual
   commands, versions, behavior, supported platforms, and package layout.
   Changes to agent discovery, catalog metadata, install guidance, or launch
   arguments are documented and validated on both Windows and Linux.
   Changes to workspace browsing or persistence also document `--project` as a
   default rather than a sandbox, bearer-token authority, state locations,
   limits, and stale-entry behavior.
   Changes to peer coordination document its supervised state transitions,
   dedicated-session and session-limit behavior, loopback capability boundary,
   artifact limits, same-account limitation, and Windows/Linux validation.
6. No secrets, generated artifacts, or unrelated changes are staged.
7. A release change also passes target-specific license generation, packaged
   peer and Chrome scroll/mobile regressions, archive-layout validation,
   checksums, and provenance configuration on both supported operating
   systems.
8. Live services created for testing are stopped unless the user asked to keep
   them running.
9. The final report distinguishes source changes, tests, commit/push state,
   deployment state, and any remaining limitations.
