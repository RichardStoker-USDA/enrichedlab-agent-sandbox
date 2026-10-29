# Security model

## Reporting a vulnerability

Report a suspected vulnerability privately through GitHub's private vulnerability reporting (the repository's Security
tab, "Report a vulnerability"). Do not open a public issue for it. Include the affected commit, the boundary it
crosses and the smallest steps that show it.

## Boundary

The protected host assets are files outside declared mounts, host credentials, container runtime sockets, local services, other sandboxes, and LAN devices. The sandboxed model, generated code, downloaded packages, websites, and service clients are untrusted.

OpenShell is the enforcement layer for filesystem, process, and network access. `agent-sandbox` is a local operator and policy controller. The model or harness may propose access. OpenShell or the host controller decides below the harness.

The following components are trusted:

- the host OS kernel and current OS user;
- the OpenShell gateway, supervisor, and configured compute driver;
- the configured Docker, VM, or Kubernetes runtime and, for Kubernetes, its control plane, CNI, Agent Sandbox controller, and requested RuntimeClass handler;
- this controller binary, its configuration, and its private state;
- the host agent or person approving requests.

A process running as the same host user can read controller state, call the loopback dashboard, signal lease supervisors, and operate Docker or OpenShell if that user already has those rights. The dashboard is not an OS-user isolation boundary.

## Default controls

- No home directory, agent configuration directory, secret directory, Docker socket, SSH directory, or broad host root may be mounted.
- Controller-created directories use mode `0700`; state files, keys, logs, and decisions use mode `0600` on Unix.
- Landlock is a hard requirement by default. Best-effort mode is an explicit compatibility choice.
- Network access is exact-host and exact-port. Public-domain validation rejects IP literals, wildcards, `.local`, `.internal`, and localhost.
- Host service leases bind `127.0.0.1` and have a TTL.
- LAN listeners require mTLS, an exact assigned bind address, source CIDRs no broader than `/24` for IPv4 or `/64` for IPv6, named client profiles, and a parent loopback lease.
- Peer links add one exact temporary endpoint only to the assigned source policy and run the relay inside that source sandbox. Driver-specific source identity is recorded for custody, not treated as a host-side source-IP authentication claim.
- Mounts are static. Approval captures evidence, persists the current active policy, recreates the sandbox, and rolls back on failure.
- VM and Kubernetes profiles reject host bind mounts. Workspace copies have file and byte limits, reject unsafe paths and escaping symlinks, and write a digest receipt. Applying a remote pull backs up the host tree first.
- Kubernetes profiles freeze the requested RuntimeClass in the sandbox record. A bounded read-only observer checks the matching Sandbox CR, Pod, RuntimeClass handler, and namespace NetworkPolicies.
- Workers have separate Git clones and no remotes. The host rejects commits that do not descend from the frozen base or touch unowned paths.
- Research workers get separate workspaces, frozen input and tool copies, bounded outputs, and host-owned seals. The dashboard omits question, prompt, input, result, and raw log bodies. Worker agreement is not validation.
- Named-agent dashboard records omit the objective, role, workspace path, runtime arguments, and state values. The
  browser receives lifecycle and session metadata only.
- Logical-agent identity is separate from every sandbox and provider session. Per-agent locks prevent overlapping
  identity use. File-locked allocation reservations make route-cap checks atomic while letting cold creates proceed
  concurrently; abandoned reservations are reconciled with controller and gateway state before reuse.
- Named-agent release reconciles remote work and state before deleting compute. A custody failure keeps the runtime.
  Task retry is one attempt by default and does not make tool side effects idempotent.
- Normal OpenShell command failures omit captured stdout and stderr. Research runner output is bounded and kept in owner-only study state for custody; it is not returned in controller errors or dashboard data.

The logical-agent registry, SQLite state, local artifacts, and loopback dashboard are one trusted user's boundary. They
are not a shared web authorization system. A multi-user deployment needs an authenticated server-side broker,
canonical tenant and principal identity, capability checks, row-level database isolation, fenced task leases, and an
external artifact store. Browser users and models must not receive Kubernetes credentials or direct access to the
OpenShell gateway.

## Secrets

Do not mount secret directories. Do not put credentials in request JSON, route files, research questions, task prompts, environment files, image layers, Git history, or dashboard fields. Use an OpenShell credential provider or another policy-bound server-side broker. This release does not implement a credential broker.

The local CA private key and client private keys stay under the controller PKI directory. Commands return their file paths, never key or password values. Revoking a client closes its active LAN leases, and each new connection checks current revocation state.

## Service risks

A host-loopback lease makes the sandbox service reachable by every process running as the same host user. A LAN lease makes it reachable by clients that satisfy both source CIDR and mTLS checks. Application authentication and authorization still belong to the application when users have different rights.

The LAN proxy terminates TLS on the host and sends the connection to the parent loopback forward. Peer relays run inside their approved source sandbox and reach a loopback-only parent forward through OpenShell's explicit proxy. Docker uses container and network custody; VM and Kubernetes use the OpenShell runtime ID. Do not replace these paths with raw `0.0.0.0` OpenShell forwards.

## Evidence limits

The host audit log is hash chained. It detects edited records and internal breaks, but an attacker with state-directory write access can truncate the tail unless its last hash is anchored elsewhere. Evidence snapshots are point-in-time records. OpenShell documents that gateway-pushed OCSF buffers may drop events; the sandbox JSONL files are the stronger source for snapshot custody.

The isolation probe tests the declared workspace transport, active policy, recorded Landlock mode, OpenShell settings, and short-lived sentinels on assigned host-private interfaces. Kubernetes runtime proofs show API bindings, not CNI enforcement or hypervisor attestation. A direct VM guest without Landlock remains best-effort even when its VM boundary is present. These checks do not prove isolation from a compromised kernel, cluster control plane, runtime, gateway, or an untested LAN device.

## Reporting

Do not include credentials, private keys, tokens, private network details, or sensitive logs in a public issue. Report a security problem privately to the repository owner before public disclosure.
