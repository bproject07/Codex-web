# Future Work

This file records ideas that are intentionally not implemented yet. It is not
a promise of compatibility or a release schedule.

## Update hardening

- Consider embedded cryptographic verification of GitHub release/Sigstore
  attestations and the exact signer workflow. The current updater requires
  GitHub API attestation metadata tied to this repository for both assets, but
  it does not verify the Sigstore bundle locally. Do not make `gh` a hidden
  runtime dependency or claim checksum verification is provenance.
- Consider an explicitly opt-in unattended update policy only after there is a
  reliable idle definition for every PTY and peer thread. Never silently
  terminate active work.

## Further sharing improvements

Scoped invitations, read-only guest terminals, owner-approved proposals and
SQLite recordings are implemented; see [OPERATIONS.md](OPERATIONS.md#controlled-session-sharing).

- Validate exact multiline prompt delivery against each real provider TUI
  before enabling it. Multiline proposals can be reviewed, but automatic
  delivery currently requires an explicitly edited single line.
- Optional authenticated accounts could verify a guest's identity across
  devices. A locked display name and bearer credential do not verify a person.
- A searchable conversation export would require an explicit provider-specific
  source. Raw terminal recordings preserve ANSI output and geometry, not a
  reconstructed provider chat.
