# Service and approval procedure

## Host web app

Bind the app to sandbox loopback, then open one lease per target port:

```bash
agent-sandbox service open <sandbox> \
  --target-port <sandbox-port> \
  --local-port <host-port> \
  --ttl-minutes <minutes> \
  --label <purpose>
```

The controller calls:

```text
openshell forward service <sandbox> --target-port <sandbox-port> --target-host 127.0.0.1 --local 127.0.0.1:<host-port>
```

An app started through blocking `exec` must daemonize, run in a service manager, or stay in another shell. Record the
lease ID and close it when the user is done.

## LAN access

Issue one client certificate per device or person. Create an mTLS child lease from an active host lease. Bind one
assigned host address, never `0.0.0.0`, and use a `/32` source when practical. Keep the TTL short. Revocation closes
all leases for that client. Return certificate paths, never certificate-key contents.

## Peer access

Use `agent-sandbox service peer <source> <target>`. Do not share workspaces or open a LAN listener. The lease owns the
source policy, relays, and any exact Kubernetes NetworkPolicies. Closing it must remove all of them.

## Requests

Workers write strict request JSON under `/sandbox/control/requests`. Review the exact requested host, method, port,
mount, client, source network, catalog path, purpose, and expiry. The host decision is canonical. A copied decision in
a VM or Kubernetes sandbox is delivery, not authority.

Auto-safe mode may cover only loopback services and workspace-bounded catalog entries when both operator toggles are
enabled. LAN, peers, domains, mounts, and NVIDIA Policy Advisor proposals stay manual.

## Network evidence

Use active OpenShell policy, OCSF events, the host listener list, the isolation sentinel, runtime proof, and the
controller lease graph together. None covers a compromised kernel, gateway, control plane, or same-user host process.
