# Future Work

This file records ideas that are intentionally not implemented yet. It is not
a promise of compatibility or a release schedule.

## Update hardening

- Consider embedded verification of GitHub release/Sigstore attestations after
  the checksum-and-immutable-release updater has proven stable. Do not make
  `gh` a hidden runtime dependency or claim checksum verification is
  provenance.
- Consider an explicitly opt-in unattended update policy only after there is a
  reliable idle definition for every PTY and peer thread. Never silently
  terminate active work.

## Controlled session sharing

Add an explicit way to share access without disclosing the server-wide owner
token.

A safe design should provide:

- a separate cryptographically random share token;
- a grant limited to one selected managed session;
- explicit `read-only` or `read-write` permission;
- an expiry time and immediate server-side revocation;
- no ability to list, create, restart, terminate, or delete other sessions;
- a visible owner indicator showing active guests and their permission;
- audit events containing only lifecycle metadata, never terminal content,
  keystrokes, Codex credentials, or raw tokens;
- rate limiting and the existing strict Origin validation;
- clear behavior when the owner restarts or removes the shared session.

The current authenticated URL is not a sharing link. It grants full read/write
control over all sessions managed by the server. Until scoped grants exist,
share it only with someone who is trusted with the same terminal authority as
the owner.

All clients attached to one PTY also share its dimensions. A sharing design
must account for competing mobile and desktop resize events, for example by
letting the owner control the PTY size while read-only guests render locally.
