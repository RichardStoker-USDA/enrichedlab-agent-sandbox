# Contributing

Use a branch from `development`. Keep changes scoped, add tests for policy and lifecycle behavior, and update the matching doc in the same commit.

## Branches

- `development` is the integration branch and GitHub's default branch, so Dependabot alerts and security pull
  requests follow the code that fixes land in. Every change lands there after the local gate below passes; topic
  branches stay local and are deleted once merged.
- `main` holds verified releases. It only moves forward, by fast-forward, after a release is verified; it is never
  committed to directly.
- Repository rulesets refuse deletion and force pushes on `development` and `main`, and `main` also requires linear
  history. Never rewrite either branch.
- Dependabot opens grouped pull requests against `development` each week for Cargo, both npm projects (`frontend`,
  `clients/workspace`) and GitHub Actions, waiting seven days after a release before proposing it; security fixes come
  without that wait. Take one by running the local gate on its branch and fast-forwarding `development` to it, or by
  landing the same versions in your own commit; Dependabot closes a pull request once its update is on `development`.
- `cargo audit` reads `.cargo/audit.toml`; every ignored advisory there names why it does not apply and when to drop it.
- CI runs on every push to `development` and `main` and on pull requests to them, on Linux and macOS.

## What stays out of this repository

This repository is public. A lab's own tools (reviewed queries over its database, its source declarations), a
site's deployment values, host and network names, identities, credentials and research data live in private
repositories and are given to the broker at deployment. `scripts/scan_public_boundary.sh` runs in CI over the tree
and its history and refuses key material, home paths, and references to private notes such as decision numbers,
review or run ids and reviewer names; code and docs explain themselves instead. Site markers are not kept here: set
`PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE` to a private file of patterns (`allow:` lines name permitted public text), and
CI reads the same lines from the optional `PUBLIC_BOUNDARY_MARKERS` Actions secret.

Run the local gate before opening a pull request:

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

Live OpenShell tests must use disposable names and work folders, short lease TTLs, and a cleanup check showing no test sandbox, listener, or supervisor remains. Never add route credentials, controller state, PKI files, OCSF logs, host inventories, or private deployment details to the repository.

Commits use a short imperative subject. Do not add generated attribution or tool trace files.
