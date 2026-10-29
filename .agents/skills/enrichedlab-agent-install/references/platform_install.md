# Platform install paths

## Apple Silicon macOS with OrbStack Docker

Required checks:

```bash
docker context show
docker info
openshell --version
```

Use a dedicated workspace root. The dashboard must remain on `127.0.0.1`. The controller talks to OrbStack through
the Docker API; it does not use OrbStack Kubernetes. If `docker info` times out, stop. A data-preserving OrbStack
Docker restart may be used after inspecting its status. Never use a reset as sandbox cleanup.

## Linux with Docker Engine

Check the exact Docker socket ownership and rootless/rootful mode. Install the systemd user service only when the user
session manager is available. The repository's build and unit tests do not prove host networking, Landlock, or
forwarding on a new distro. Run the Docker live qualification before claiming support.

## Direct OpenShell VM

Use `--profile vm`. Workspaces use bounded push/pull sync and reject host bind mounts. Confirm the gateway advertises
the VM driver. Record `best-effort` Landlock if the guest kernel cannot meet hard Landlock; do not rewrite that as an
equivalent hard control.

## Kubernetes and Kata

The cluster owner installs and owns NVIDIA OpenShell, the Kubernetes SIG Agent Sandbox controller/CRDs, the runtime
handler, the namespace, and default-deny policy. This repository does not install or assume those controls.

Run the read-only preflight with an exact kubeconfig, context, namespace, and RuntimeClass before configuring the
profile. Keep peer NetworkPolicy writes off unless the operator grants the narrow namespace permission and supplies a
private relay address.

Use a tagged OpenShell chart. NVIDIA marks the Kubernetes deployment path experimental and warns about breaking
changes. Record the exact chart, controller, RuntimeClass handler, CNI, and node runtime in the local qualification.

The controller can use an existing in-cluster PostgreSQL service for provenance. PostgreSQL does not remove the
single-writer rule for local prompt state and artifact files. A shared cluster dashboard also needs user authentication
and tenant authorization; the shipped loopback dashboard must not be exposed through a Service.

## WSL2 and Windows

NVIDIA documents WSL2 with Docker Desktop as experimental. Treat the controller as a Linux build inside WSL2 and run
a separate live qualification. Native Windows services, paths, ACLs, and runtime handling are not implemented here.
Do not run the macOS or Linux user-service installer from native PowerShell and call that supported.

## Podman and other engines

The configuration parser accepts Podman. Runtime identity, peer custody, forwarding, cleanup, and live evidence have
not passed a release gate. Leave the claim as unqualified until those tests exist and pass on the exact engine.
