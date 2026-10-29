# Operations

## Prerequisites

- Rust 1.93 or newer for building this release;
- NVIDIA OpenShell, tested at 0.0.116;
- one connected OpenShell gateway and workspace per enabled gateway profile;
- Docker compute for bind profiles, the OpenShell VM driver for direct guests, or the Kubernetes driver plus its Agent Sandbox controller;
- a qualified RuntimeClass and runtime handler before enabling a Kubernetes Kata profile;
- `git`, `openssl`, and `lsof` on the host;
- a pinned OpenShell sandbox image.

The default image is pinned by digest in `src/cli.rs`. Replace it only after building, scanning, and qualifying the replacement.

## Configuration

For a local user install, use the setup script. It builds the locked Rust source, creates private directories, copies examples without replacing existing files, records an enabled gateway profile, and can install the dashboard as a user service:

```bash
scripts/setup_local.sh --install-service
export PATH="$HOME/.local/bin:$PATH"
```

Use `--profile vm` or `--profile kubernetes` for a non-Docker gateway. The Kubernetes form needs an absolute kubeconfig when one is used, a named context when the file has several, and the expected RuntimeClass. Run [the Kubernetes preflight](getting-started.md#existing-kubernetes-or-k3s-cluster) first. Peer-policy mutation is a separate opt-in.

Copy the examples, then edit only non-secret route metadata:

```bash
mkdir -p ~/.config/enrichedlab-agent-sandbox
cp examples/controller.toml ~/.config/enrichedlab-agent-sandbox/controller.toml
cp examples/routes.json ~/.config/enrichedlab-agent-sandbox/routes.json
chmod 700 ~/.config/enrichedlab-agent-sandbox
chmod 600 ~/.config/enrichedlab-agent-sandbox/controller.toml ~/.config/enrichedlab-agent-sandbox/routes.json
```

Route `workspace` must name an OpenShell workspace that already has the intended inference route. The model identifier and harness options must match that endpoint. Route files contain no bearer token or model credential.

`git_committer_name` and `git_committer_email` set the host-side identity used only for controller-created team merge commits. The defaults are local, non-routable values and do not depend on a global Git configuration. Change them in private controller configuration when an operator identity should appear in project history.

`default_gateway_profile` selects one entry under `gateway_profiles`. Keep disabled profiles in the file as templates, then enable a profile only after its gateway and runtime are present. `required = true` makes `doctor` fail when that gateway is unhealthy. Kubernetes and VM profiles must use sync transport and `bind_mounts_enabled = false`.

Profiles can be changed without hand-editing TOML:

```bash
agent-sandbox platform configure --name vm-local --gateway vm --driver vm --required --replace --set-default
agent-sandbox platform set-default docker-local
agent-sandbox platform remove vm-local
```

Replace and remove fail while any managed sandbox names the profile. Kubernetes configuration also requires `--kubectl-bin` with an absolute executable path. The stored observer command uses the canonical path, so a user service does not depend on an interactive shell's `PATH`.

A Kubernetes observer can run local `kubectl` or a batch SSH command. Give it read-only RBAC for Sandbox CRs, Pods, RuntimeClasses, and NetworkPolicies. A profile that needs peer links may set `manage_peer_network_policies = true` and add namespace-scoped get, create, and delete permission for NetworkPolicies. No other Kubernetes resource mutation is implemented. Keep its kubeconfig outside the repository and pass an absolute path; the controller stores the canonical target. A command starting with `sudo` must include `-n`. The SSH form uses a checked host and argument-quoted command, but SSH key custody still belongs to the host operator.

The gateway's Docker driver must permit the exact bind mounts passed by the controller. Back up its configuration before any change. On the tested package-managed macOS gateway, leave `host_gateway_ip` unset. Peer mode uses OpenShell's explicit proxy and does not need a bridge address forced into gateway configuration.

## Normal lifecycle

```bash
agent-sandbox doctor
agent-sandbox platform list
agent-sandbox create --name worker-one --route local-agent --gateway-profile docker-local
agent-sandbox run worker-one -- 'Complete the assigned task.'
agent-sandbox list
agent-sandbox stop worker-one
agent-sandbox start worker-one
agent-sandbox delete worker-one
```

For VM or Kubernetes work, replace `docker-local` with its enabled profile. Initial content is pushed after create. Pull changes into a private snapshot before replacing host work:

```bash
agent-sandbox sync pull worker-one
agent-sandbox sync pull worker-one --apply
agent-sandbox platform refresh worker-one
```

An applied pull writes a recoverable backup under private state. Review the snapshot and sync report before deleting that backup through a separate retention process.

Delete keeps the host workspace. `--trash-workspace` is accepted only for a unique controller-created folder below the configured root. The platform trash is used rather than permanent removal. Evidence is captured before deletion unless `--skip-evidence` is explicit.

For a stable named agent on disposable compute, use the logical-agent lifecycle instead of ordinary `create` and
`delete`:

```bash
agent-sandbox agent create cluster-helper \
  --domain cluster-ops \
  --state-profile cluster-operator \
  --objective '<durable objective>' \
  --route local-agent
agent-sandbox agent run cluster-helper -- '<bounded task>'
agent-sandbox agent status cluster-helper
```

The run releases compute by default. `agent start` and `--keep-runtime` keep a runtime allocated; `agent release`
reconciles and deletes it. `agent retire` seals the state run and retains its files. Read
[named-agent continuity](continuity.md) before enabling retries.

For a team, `team stop` keeps worker runtime records for restart. `team delete` removes all worker sandboxes and their controller runtime records while keeping the team project and outputs:

```bash
agent-sandbox team stop study
agent-sandbox team delete study --skip-evidence
```

Delete a running team without `--skip-evidence` when final runtime evidence is needed. A stopped sandbox cannot supply a new live OCSF snapshot, so deleting it later needs the explicit skip or an operator-approved restart before capture.

## Policy changes

Use OpenShell Policy Advisor for denied network actions. Review pending chunks:

```bash
agent-sandbox proposal pending worker-one
agent-sandbox proposal approve worker-one <chunk-id>
```

For a host-selected public HTTPS domain:

```bash
agent-sandbox allow worker-one docs.example.org
agent-sandbox allow worker-one uploads.example.org --write
agent-sandbox deny worker-one docs.example.org
```

Every live policy update is followed by a read of the effective OpenShell policy and an atomic controller copy. This preserves approved rules across restarts and mount recreation.

## Requests

Validate an example before use:

```bash
agent-sandbox request schema --output json
```

The filename must equal `<request_id>.json`. Input is capped at 64 KiB and uses strict fields. Bad input is recorded once under private controller state and does not stop other requests.

Run the dashboard as a user service if continuous auto-safe reconciliation is wanted. Keep the bind on loopback. Service-manager configuration must pass absolute binary, root, state, and config paths, and should restart only on failure with bounded backoff.

The provided installer writes and validates a per-user launchd plist on macOS or systemd user unit on Linux. It passes exact controller, OpenShell, and container-runtime paths. Existing service files are copied to private controller state before replacement. `scripts/remove_dashboard_service.sh` disables the service and keeps that backup and all controller state.

## Agent state

Use the state skill and [state guide](agent-state.md) before changing profiles, schemas, audiences, or export retention.
Routine checks are:

```bash
agent-sandbox state list
agent-sandbox state verify
```

The automatic research path is `research launch --max-parallel <count> --max-segments <count>`. It starts a new Pi or
OpenCode session for each segment, watches JSONL, reconciles semantic proposals, versions output, and checkpoints before
another segment. A direct sandbox `run --harness pi|open-code` creates provenance but no prompt-facing state unless a
research or custom harness workflow links one. `agent run` always uses the logical agent's scoped state run.

For embedded storage, prepare once and inspect a run:

```bash
agent-sandbox provenance prepare
agent-sandbox --output json provenance graph <run-id>
```

For PostgreSQL, `prepare` is a controlled migration action and needs the separate migration-owner DSN. Remove that DSN
from the long-running service, then use `graph` and the dashboard with the runtime identity.

Use `provenance ingest` for a reviewed OpenAI-compatible or native custom-harness stream. Do not parse a private
transcript format without a pinned adapter and fixtures.

Do not edit the SQLite database, WAL, PostgreSQL provenance tables, artifact tree, accepted proposal, or event file by
hand. A context reset needs a checkpoint followed by a new provider session. Export to a new directory and leave
restricted bytes and event payloads off unless the destination and data class were reviewed.

## Recovery

1. Stop new approvals and service openings.
2. Run `agent-sandbox service list` and close active leases.
3. Run `agent-sandbox list` and compare controller records with `openshell sandbox list --all-workspaces`.
4. Run `agent-sandbox platform list` and `platform refresh`; on Kubernetes, compare the proof with the live Pod and RuntimeClass.
5. Verify `agent-sandbox audit verify`.
6. Capture evidence for a live sandbox before deleting or recreating it.
7. For sync transport, run a non-applying pull before recovery changes.
8. Restore the backed-up gateway configuration if a gateway change caused the failure.
9. Run `agent-sandbox doctor` and one disposable host-forward test.

Observed failure signatures and their fixes are kept in [troubleshooting](troubleshooting.md). Add a short entry there when a release qualification finds a repeatable operator or runtime failure.

The legacy migration copies records and leaves the source state untouched:

```bash
AGENT_SANDBOX_LEGACY_STATE_DIR=/absolute/legacy/state agent-sandbox migrate-legacy
```

Do not merge two non-empty state directories.

## Upgrades

Build and test on `development`, capture the current installed binary and configuration outside the repository, and install the candidate from a clean commit. Run unit, Clippy, state, dashboard, host-forward, LAN denial/allow, request, evidence, and cleanup gates. Promote the same commit to `main`; do not rebuild different source under the same version.
