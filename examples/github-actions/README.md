# GitHub Actions template

`ci.yml` mirrors the active `.github/workflows/ci.yml` workflow. It runs formatting, tests, Clippy, shell and dashboard
syntax checks, focused-skill validation, reachable-history disclosure checks, and locked release builds on Ubuntu and
macOS, plus a RustSec dependency audit. `actions/checkout` is pinned to a full commit SHA and fetches full history for
the disclosure gate.

Keep both copies byte-for-byte identical. A repository contract test blocks drift. Keep GitHub Actions permissions at `contents: read` and review any action SHA change before merging it.
