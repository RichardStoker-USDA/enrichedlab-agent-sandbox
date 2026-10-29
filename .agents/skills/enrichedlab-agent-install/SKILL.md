---
name: enrichedlab-agent-install
description: Install or upgrade the EnrichedLab Agent Sandbox controller on macOS or Linux, or configure its supported NVIDIA OpenShell VM and Kubernetes profiles. Use after the setup skill has selected a platform and before live qualification. Covers safe path migration, user services, focused skill links, and explicit unsupported-platform boundaries.
license: MIT
---

# EnrichedLab agent install

Prerequisites are Rust 1.93 or newer, Git, OpenSSL, NVIDIA OpenShell, and a selected runtime. Kubernetes setup also
needs `kubectl` and an existing OpenShell deployment.

Read [references/platform_install.md](references/platform_install.md) for the selected platform only.

## Preflight

1. Confirm the repository is clean or record the exact reviewed commit being installed.
2. Run `uname -s` and `uname -m`, inspect tool versions, and check the selected runtime without changing it.
3. Resolve every install, config, state, and workspace path to an absolute path. Reject home, `/`, broad project roots,
   credential folders, container sockets, and Kubernetes identity folders as workspace mounts.
4. If legacy `openshell-agent-sandbox` config or state exists, use the installer migration. It copies owner-private
   bytes to the new EnrichedLab paths and keeps the old paths as rollback. Historical evidence manifests stay bound
   to their original path; the installer keeps a private migration copy outside the new active evidence index. Do not
   merge two non-empty state trees or rewrite an evidence manifest to make its path check pass.
5. Back up an existing binary, user service, and skill target before replacement.
6. Default provenance to embedded SQLite. Select PostgreSQL only when an existing approved database service, tenant,
   migration identity, runtime identity, TLS path, backup owner, and live qualification are in scope.

## Install

Run the repository installer with the selected paths and profile. A common macOS Docker install is:

```bash
scripts/setup_local.sh \
  --workspace-root /absolute/dedicated/workspace \
  --profile docker \
  --install-service \
  --install-skills
```

The script builds locked Rust dependencies, configures one named gateway profile, copies legacy state only when the
destination is empty, installs a loopback user service when asked, and runs `doctor` unless `--skip-doctor` is explicit.
Do not pass `--skip-doctor` for a finished setup.

For PostgreSQL, run `provenance configure`, apply migrations with the migration identity, apply
`migrations/postgres/grant_runtime.psql`, then remove the migration DSN from the service environment. Never persist a
DSN in the controller config, skill, command transcript, or repository. Finish the PostgreSQL rows in the qualification
skill before calling the backend installed.

For an existing OpenShell CLI, let the script use the resolved binary. Use `--install-openshell-cli` only after the
user approves the pinned download. Never pipe a remote installer into a shell from this skill.

## Skill install

`scripts/install_agent_skills.sh` links every focused skill as one source set. Use explicit targets or `--all`; named
targets include Codex, Claude Code, OpenCode, Pi, OMP, and GitHub Copilot. Add another Agent Skills-compatible harness
with `--target-dir /absolute/skills/directory`. Preflight every destination before changing any of them. Use
`--migrate-legacy` only when replacing this repository's prior single skill. OMP, Codex, and Claude Code operate the
host controller; disposable model workers still use the qualified Pi or OpenCode runtime adapter.

## Platform limits

- macOS OrbStack/Docker is a tested path. OrbStack Kubernetes is not implied.
- Linux Docker is source-qualified here and needs a live host qualification on each runtime/distro combination.
- Direct VM and Kubernetes behavior is gateway and deployment specific.
- NVIDIA's OpenShell Kubernetes Helm path is upstream experimental. One tested K3s/Kata deployment does not make a
  generic production Kubernetes claim.
- WSL2 is an experimental Linux path. Native Windows is not installed by this project.
- Podman is a configuration type, not a qualified runtime in this release.

Finish by reading `../enrichedlab-agent-qualify/SKILL.md`. Do not report installation complete from build success alone.
