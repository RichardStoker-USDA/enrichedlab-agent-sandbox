# Service access

## Host-only service

Run the app on sandbox loopback. Do not bind it to every sandbox interface.

```bash
agent-sandbox exec worker-one -- sh -lc 'nohup python3 -m http.server 8000 --bind 127.0.0.1 >/sandbox/work/service.log 2>&1 < /dev/null & printf "%s\n" "$!" >/sandbox/work/service.pid'
agent-sandbox service open worker-one \
  --target-port 8000 \
  --local-port 38080 \
  --ttl-minutes 30 \
  --label report
```

`exec` waits for the remote command. Start a long-running app with its own service manager, keep it open in a
separate shell, or use the tested `nohup` form above. The app process remains inside the sandbox and is removed with
that sandbox.

The controller uses `openshell forward service`, not `openshell forward start`. The former accepts both `--target-port` and `--local`; the latter maps one numeric port to the same port inside the sandbox. A local port of `0` asks the OS for a free loopback port.

Create independent leases for apps with several ports. Each lease has its own target port, host port, label, TTL, process, and audit record.

## LAN mTLS service

Initialize the local CA and issue one profile for each device or person. The command prints credential paths, not credential values.

```bash
agent-sandbox pki init
agent-sandbox pki issue lab-tablet
```

Create the host lease first. Use its ID as the LAN parent:

```bash
agent-sandbox service lan <host-lease-id> \
  --bind <assigned-host-ip> \
  --local-port 38443 \
  --source <client-ip-or-narrow-cidr> \
  --client lab-tablet \
  --ttl-minutes 30
```

The bind address must be assigned to the host and must not be loopback, wildcard, or multicast. IPv4 sources must be `/24` or narrower. IPv6 sources must be `/64` or narrower. A client must pass the TLS CA check, match a fingerprint named on the lease, and remain unrevoked. Import the generated PKCS#12 file and trust the local CA on a browser device before opening `https://<assigned-host-ip>:38443/`.

Revoke a profile when the device should lose access:

```bash
agent-sandbox pki revoke lab-tablet
```

Revocation closes every active lease naming that profile. New TLS connections also check the live PKI index.

## Sandbox peer service

A source sandbox can reach one target service without sharing workspaces:

```bash
agent-sandbox service peer source-one target-one \
  --target-port 8000 \
  --source-port 39080 \
  --ttl-minutes 30
```

Docker and VM profiles create a loopback-only host forward for the target and give only the source sandbox an exact temporary OpenShell endpoint for `host.openshell.internal:<parent-port>`. A relay process runs inside the source sandbox and listens at `127.0.0.1:<source-port>`. The target workspace is never mounted into the source.

Docker and VM lease records report both access forms:

- `source_endpoint`: use `host.openshell.internal:<parent-port>` directly from the approved source;
- `bind`: use `127.0.0.1:<source-port>` for clients that need a source-local endpoint.

For clear HTTP through the source-local endpoint, send a `Host` header equal to `source_endpoint`. OpenShell checks the HTTP authority after the CONNECT tunnel. Other TCP protocols use the source-local endpoint without an HTTP header.

The Docker and VM relay reaches the host alias through OpenShell's explicit proxy. The tested package-managed macOS gateway does not need a `host_gateway_ip` change, and the OpenShell gateway documentation says to leave that setting unset for package-managed gateways. Do not edit gateway configuration to force a Docker bridge address on OrbStack. OrbStack NAT also removes useful source-IP identity at a host-loopback relay, so this controller does not claim host-side source-IP attribution. Isolation comes from the source-only OpenShell policy and a relay process inside that source sandbox.

Docker peers record the source container ID and network identity and reject a target on another isolated runtime network. VM peers need sync transport and record the source OpenShell runtime ID. Their ready and stop files are copied into the source sandbox, not treated as host mounts. Both paths verify live source identity before starting the relay. Podman peers are rejected because that identity path has not been qualified.

Kubernetes peers use a different path. The controller verifies both OpenShell runtime IDs and the independently observed source and target Pod UIDs and IPs. It starts a TLS relay on the target Pod, creates one ingress NetworkPolicy selecting only the target and named source Pod plus one egress NetworkPolicy selecting only that source and target port, and adds the target Pod IP and relay port only to the source OpenShell policy. A 256-bit per-lease capability authenticates the source inside TLS. Consumers use the source-local `bind` address. The Kubernetes `source_endpoint` is a custody record for the internal relay and is not a direct application endpoint. Closing the lease removes both processes, the exact OpenShell endpoint, both NetworkPolicies, and remote capability material.

This path needs `manage_peer_network_policies = true` on the Kubernetes observer and namespace-scoped permission to get, create, and delete NetworkPolicies. It does not create a Service, Ingress, NodePort, host listener, or namespace-wide rule. OpenShell 0.0.116's policy-update parser cannot represent an IPv6 literal endpoint, so this release rejects an IPv6 target Pod instead of weakening the rule.

## Tool placement and concurrency

Choose the lifecycle from the tool's state and startup cost:

| Tool shape | Placement | Scaling path |
|---|---|---|
| cheap, task-specific script or untrusted package | worker sandbox | one copy per worker; removed with the sandbox |
| bounded batch analysis with queued inputs | short-lived sandbox or Kubernetes Job | queue depth and resource quotas; one result contract per job |
| stateless HTTP or MCP service with costly startup | separate governed service | request concurrency, warm minimum, bounded scale-out, lease access |
| database, Milvus, instrument gateway, or mutable singleton | separate stateful service | explicit client limits, backup, health, and maintenance policy |

gVisor and Kata are runtime isolation choices, not autoscalers. [KEDA ScaledJob](https://keda.sh/docs/2.20/concepts/scaling-jobs/)
fits queued long work where one job consumes one message. [Knative Serving](https://knative.dev/docs/serving/autoscaling/)
fits stateless request services that can scale from request concurrency. Do not scale a database, vector store, or
instrument session to zero through either pattern without a separate state and recovery design.

Kubernetes Agent Sandbox warm pools reduce sandbox allocation latency. They do not add tool replicas or model request
capacity. This controller does not manage warm pools, KEDA, or Knative in the current release; those are deployment
adapters that need their own identity, quota, evidence, and cleanup tests.

Give workers a shared service only when concurrency, warm state, licensing, GPU use, or central validation makes a
per-sandbox copy worse. Every service still needs an exact endpoint grant, per-client or per-run identity when the
protocol supports it, request limits, and an artifact or event link for outputs used in a result.

## Agent request files

Copy a request example to the sandbox inbox using its request UUID as the filename:

```bash
cp request.json /sandbox/control/requests/8fdf61e5-e8c7-4ec5-a5ef-e70bfa2ed4bf.json
```

The controller copies valid input into immutable host-normalized state and hashes the source. Invalid or oversized input is recorded once without blocking other requests. Decisions are written to host state and the sandbox decision directory. Docker gets a read-only bind; VM and Kubernetes receive a remote copy and do not get the host decision directory.

Review requests with the dashboard or CLI:

```bash
agent-sandbox request list worker-one
agent-sandbox request get worker-one <request-id>
agent-sandbox request approve worker-one <request-id> --reason 'Approved for this bounded task.'
agent-sandbox request reject worker-one <request-id> --reason 'Use the existing report lease.'
```

The reviewed Team Lead adapters can list and reject requests for their own active worker. Approval is disabled unless
the launcher enables a supported low-risk kind. Read [harnesses](harnesses.md) before granting that capability.

OpenShell Policy Advisor remains the first path for network denies because it sees the denied operation and can propose a narrow rule. Every OpenShell proposal stays manual, including for sandboxes using the controller's auto-safe mode. The custom domain request is a manual fallback. Approved active policy is saved to the controller policy source so it remains after restart or mount recreation.

## Cleanup

```bash
agent-sandbox service list
agent-sandbox service close <lease-id>
```

Closing a host lease closes LAN or peer children first. Stopping or deleting a sandbox closes leases where it is either the target or peer source. Supervisors mark early OpenShell exits as failed instead of leaving an active record.

Each host and peer lease records both its supervisor PID and the child OpenShell PID. `service list` checks live custody before reporting an active lease. If a supervisor was killed, the controller verifies the recorded child command and process group before stopping it. It then closes child LAN leases, removes peer policy, and closes an owned peer parent. A failed custody check is retained in the lease record instead of signaling an unverified PID.
