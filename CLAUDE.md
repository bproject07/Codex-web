# Claude Code Instructions

Read and follow [AGENTS.md](AGENTS.md) before editing this repository. It is the
authoritative operational contract and contains the product invariants,
security boundaries, and required CI checks.

Do not run builds, compilers, tests, package scripts, or browser regressions
locally. GitHub Actions is the validation environment for this repository.
Make source and documentation changes locally and report validation as pending
until the GitHub workflow completes.

Do not start, stop, restart, replace, or overwrite a live packaged server unless
the user explicitly requests that exact runtime action.
