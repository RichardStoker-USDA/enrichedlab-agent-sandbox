# Getting started

The controller runs outside each subagent. NVIDIA OpenShell supplies the gateway, sandbox lifecycle, and lower policy
layer. Pick one platform below, finish its preflight, install the controller, then run the live qualification for that
same platform.

Tested release pins are OpenShell 0.0.116 and Kubernetes SIG Agent Sandbox 1.0.0. The controller installer can download
the checksum-pinned OpenShell CLI, but it does not install or start an OpenShell gateway.

## Let an agent do the setup

From the cloned repository, tell a host coding agent:

```text
Read .agents/skills/enrichedlab-agent-setup/SKILL.md and install and qualify this on my current host.
```

The skill asks about the platform, retention, inference, service access, and whether it may make live changes. It then
uses the install and qualification skills. A chat-only agent follows the same checks but asks you to run each command.

## Mac with OrbStack

OrbStack is used as a Docker engine in this path. Its optional Kubernetes cluster is not involved.

1. Install and start an OpenShell Docker gateway using NVIDIA's instructions.
2. Check the gateway and engine.
3. Run the controller installer.
4. Run the disposable qualification.

```bash
openshell --version
openshell status
docker context show
docker info

scripts/setup_local.sh --install-service --install-skills
export PATH="$HOME/.local/bin:$PATH"
scripts/qualify_sandbox.sh --profile docker-local
```

Open `http://127.0.0.1:32100/`. A pass needs sandbox creation, isolation checks, evidence capture, and verified cleanup.
The dashboard being reachable by itself is not a sandbox pass.

After adding a private inference route, qualify named-agent continuity with non-sensitive work:

```bash
scripts/qualify_continuity.sh \
  --profile docker-local \
  --route <private-route> \
  --exercise-harness
```

The second allocation must use another runtime generation while keeping the same agent and state run IDs. Retire the
test identity after review; retirement retains its evidence. The script performs that retirement and leaves no active
runtime. Use `--harness pi` to qualify the Pi path instead of OpenCode.

For an upgrade from the prior product path, add `--migrate-legacy`. Historical evidence stays bound to its original
private path. The installer retains another private copy outside the new active evidence index, while new snapshots use
the new path. Do not rewrite an old manifest to make it appear current.

To remove only the dashboard service:

```bash
scripts/remove_dashboard_service.sh
```

## Linux with Docker Engine

Use the same Docker commands. The source is tested on Linux, but each distro, engine, network setup, and OpenShell
gateway still needs its own live qualification:

```bash
scripts/setup_local.sh --profile docker --install-service --install-skills
export PATH="$HOME/.local/bin:$PATH"
scripts/qualify_sandbox.sh --profile docker-local
```

The service uses a systemd user unit when systemd is available. The installer prints the exact path and dashboard
command.

## Existing Kubernetes or K3s cluster

NVIDIA marks the OpenShell Kubernetes Helm path experimental. This repository has passed its controller checks on one
K3s/Kata reference deployment. That result does not qualify another cluster.

Before changing the local profile, the cluster owner must provide:

- a NetworkPolicy-enforcing CNI;
- Kubernetes SIG Agent Sandbox CRDs and controller;
- an OpenShell gateway using the Kubernetes compute driver;
- a `RuntimeClass` whose handler is installed on the selected nodes;
- default-deny policy and an operator-controlled gateway access path;
- read-only observer access to Sandbox CRs, Pods, RuntimeClasses, and NetworkPolicies.

Run the read-only preflight:

```bash
scripts/preflight_kubernetes.sh \
  --kubeconfig "$HOME/.kube/config" \
  --context research \
  --namespace agent-sandboxes \
  --runtime-class kata-qemu
```

Then register the gateway profile:

```bash
scripts/setup_local.sh \
  --profile kubernetes \
  --profile-name kubernetes-kata \
  --gateway kubernetes \
  --namespace agent-sandboxes \
  --runtime-class kata-qemu \
  --kubeconfig "$HOME/.kube/config" \
  --context research \
  --install-service \
  --install-skills

export PATH="$HOME/.local/bin:$PATH"
scripts/qualify_sandbox.sh --profile kubernetes-kata
```

The profile uses bounded copy sync and sends no host mount into the Pod. A passing runtime proof ties the OpenShell
sandbox to one controller-owned Pod and records its `RuntimeClass` and handler. It is API evidence, not CNI packet
evidence or microVM attestation.

Peer NetworkPolicy changes are off by default. Enable them only after the `--peer` preflight passes and the observer has
namespace-scoped get, create, and delete rights for NetworkPolicies. The controller accepts only its lease-owned names.

## Direct microVM profile

Start an OpenShell gateway that uses the VM compute driver, then select it:

```bash
scripts/setup_local.sh \
  --profile vm \
  --profile-name vm-local \
  --gateway vm \
  --install-service \
  --install-skills

export PATH="$HOME/.local/bin:$PATH"
scripts/qualify_sandbox.sh --profile vm-local
```

VM workspaces use bounded sync. Some guest kernels do not expose Landlock. Use `--landlock best-effort` only after the
qualification records that limit. A VM boundary does not turn a missing Landlock check into a pass.

## Windows and Podman

Native Windows is unsupported. NVIDIA documents WSL2 with Docker Desktop as experimental; this release has not live
qualified that path. Run the Linux controller inside WSL2 only as a separate experiment and do not report it as a
supported Windows install.

Podman exists as a configuration type. Runtime identity and peer custody are not qualified, so the setup skill must not
select it as a supported install.

## Provenance storage

Keep the default embedded backend for a laptop or one controller:

```bash
agent-sandbox provenance configure --backend embedded --tenant local
agent-sandbox provenance prepare
```

Use PostgreSQL only when the database service, backup owner, tenant, TLS, and separate roles are ready. Provide DSNs as
runtime environment variables, not command arguments or config values:

```bash
agent-sandbox provenance configure --backend postgres --tenant <tenant-id>
agent-sandbox provenance prepare
```

Follow [the PostgreSQL migration guide](../migrations/postgres/README.md), then run the PostgreSQL rows in
[release qualification](release.md). PostgreSQL shares provenance events and evidence leases. It does not make local
prompt-state files or artifact bytes active-active. See [cluster deployment](cluster-deployment.md).

The local logical-agent registry remains embedded and single-controller even when provenance uses PostgreSQL. Do not
put multiple controller replicas over that local registry.

## Researcher client for a shared cluster

A researcher workstation does not install a Kubernetes controller or receive cluster credentials. The site operator
deploys the broker and gives the researcher an Access-protected HTTPS origin, group, and profile. Install
`cloudflared` and the source-matched `agent-sandbox-remote` binary, then use the remote setup skill:

```text
Read .agents/skills/enrichedlab-agent-remote/SKILL.md and connect this workstation to the approved broker.
```

The browser login uses the site's identity provider and MFA. `agent-sandbox-remote me` must show the intended identity,
quota, and profile before a task is submitted. Closing the workstation after HTTP 202 does not cancel the cluster
task. See [remote broker](remote-broker.md).

## Site-owned shared broker

The shared path is separate from the single-operator Kubernetes profile above. It runs an authenticated broker API,
one fenced controller worker, PostgreSQL task custody, and on-demand OpenShell sandboxes in the cluster. Start with:

```text
Read .agents/skills/enrichedlab-agent-broker/SKILL.md and prepare a private GitOps deployment for this cluster.
```

The public manifests contain no credentials or live values. Do not expose the local dashboard, OpenShell gateway,
Kubernetes API, database, or persistent volumes to researchers. NVIDIA marks its Kubernetes setup experimental, so
the site must run the full live matrix for its exact K3s or Kubernetes release, CNI, RuntimeClass, storage, and node
pool.

## Private files

Keep these outside Git:

- local `controller.toml` and `routes.json`;
- kubeconfigs, OpenShell identities, API keys, client certificates, and PKI keys;
- workspaces, scientific data, audit logs, snapshots, and service logs;
- hostnames, addresses, model endpoints, and live runtime receipts.

The examples use placeholders. The installer creates controller config and state with owner-only modes.
