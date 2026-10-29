# Evidence and dashboard

## Live view

`agent-sandbox dashboard` binds to configured loopback, default `127.0.0.1:32100`. The server rejects non-loopback binds. Mutating requests also need a matching loopback Host, same-origin Origin, `Sec-Fetch-Site: same-origin`, and an operation-specific intent header.

Those checks stop normal cross-site browser requests and DNS-rebinding hostnames. They do not authenticate processes running as the same OS user. Keep the dashboard on loopback and use host OS account controls for its operator boundary.

One Rust observer refreshes controller state every two seconds. The browser receives revision and health updates over
server-sent events, fetches a new cached snapshot only after a semantic revision change, and marks the view stale if
the feed stops. The normal view has no refresh button. `Reconnect` appears only when the feed is stale or unavailable.

The live surface shows:

- named gateway health and expected versus observed driver;
- per-sandbox runtime proofs, including Kata RuntimeClass and Pod binding when configured;
- workspace-sync direction, digest, snapshot, applied state, and backup custody;
- remote decision-copy digest, size, verification time, and append-only receipt history;
- the latest verified evidence manifest with process, package, and listener counts;
- controller-owned sandboxes and OpenShell state;
- full effective policy for each sandbox;
- a bounded tail of sandbox OCSF JSONL events;
- custom requests and OpenShell Policy Advisor proposals;
- service leases and expiration;
- team ownership and integration state;
- research study, worker route and phase, seal, and comparison status without question or result bodies;
- agent-state run, profile, context epoch, collection scope, event-authority timeline, payload hashes, and artifact
  custody metadata without state values, event payloads, or artifact bytes;
- catalog digests;
- recent host audit-chain records.

The header search covers bounded metadata across agents, runs, state, artifacts, requests, audit, research, teams,
services, and catalog records. `Ctrl+K` or `Cmd+K` focuses it. Audit uses server-side search and 50-row pages inside a
bounded scroll region. Search responses omit state values, restricted event bodies, artifact bytes, and raw reasoning.

The dashboard runs an auto-safe reconciler every two seconds. It can apply only loopback service and workspace-bounded catalog requests, only for a sandbox created with `--approval-mode auto-safe`, and only when the matching operator setting is enabled. LAN, peer, domain, and mount requests remain manual. OpenShell Policy Advisor remains manual and is not changed by this controller setting.

For sync profiles, the host uploads each decision, downloads that exact remote path, and compares its SHA-256 digest and size with the host's canonical outbox before writing a delivery receipt. The current receipt and append-only history stay in private host state. A legacy or mismatched receipt is shown as unverified and fails strict decision integrity. The remote decision is a delivered copy, not the authorization boundary.

Prometheus text metrics are available at `/metrics` on the same loopback listener. They contain counts and pass/fail gauges only. The endpoint does not export names, paths, request bodies, policy content, host addresses, model routes, or certificate data.

`Capture evidence` uses the same same-origin and explicit-intent check as other dashboard mutations. The server runs
the capture in a bounded worker and publishes the resulting state revision. A failed capture returns a bounded error
and does not mark an incomplete directory as verified.

## Evidence snapshot

```bash
agent-sandbox audit snapshot worker-one
```

A snapshot writes a private timestamped directory containing:

- controller and OpenShell sandbox records;
- active policy and settings;
- service leases;
- decision-delivery receipts or an explicit decision-integrity error;
- a SHA-256 workspace inventory;
- process names without command arguments;
- Debian, Python, and global Node package inventories when present;
- mount information;
- a point-in-time host TCP listener list;
- the latest runtime proof and remote sync report for sync-transport sandboxes;
- bounded copies of exact `openshell*.log` files from sandbox `/var/log`, including OCSF JSONL;
- a manifest hashing every captured file.

Each capture gets a timestamp plus a random suffix. `capture-status.json` starts as `capturing` and changes to `complete` only after the required files are present. An interrupted directory is retained as incomplete evidence. Log capture is limited to 32 files, 16 MiB per file, and 64 MiB total. OpenShell restricts its normal download command to the sandbox workspace, so the controller reads the named log files through authenticated sandbox execution instead of claiming that `/var/log` was downloaded as a directory.

`agent-sandbox delete` takes a snapshot first unless `--skip-evidence` is explicit. `catalog verify` and `audit verify` are separate because catalog artifacts and the host audit log have different custody.

## Isolation probe

```bash
agent-sandbox audit isolation worker-one
```

The probe checks the workspace transport, absence of broad host roots, Landlock mode in the controller record, Policy Advisor and OCSF settings, and absence of private destinations in the effective policy. Bind transport verifies mount modes. Sync transport verifies remote workspace and control paths plus the most recent sync report without claiming host mount immutability. It also creates short-lived listeners on assigned host-private interfaces and asks the sandbox to connect. On macOS it separately tests `host.openshell.internal` against a host-loopback sentinel because OrbStack routes that alias through its VM rather than exposing the Linux bridge address. A received connection fails the probe.

For Kubernetes, capture includes the bounded observer proof. A passing proof ties the OpenShell sandbox label to one Sandbox CR and Pod and compares the requested and observed RuntimeClass. It records the runtime handler and summarized NetworkPolicies. This shows API state at capture time; it is not a CNI packet trace or a hypervisor attestation.

The sentinel shows whether that sandbox reached the tested host interfaces at that time. It does not probe every LAN device, prove kernel integrity, or replace packet capture. Run it only when no intentional peer lease is active because a peer lease adds a narrow private endpoint by design.

## Audit properties

The host audit file is JSONL with a sequence, previous hash, and record hash. `agent-sandbox audit verify` catches edits and gaps inside the retained chain. It cannot detect clean tail truncation unless an operator anchors the latest hash outside the state directory. The release process should record the final hash in signed release evidence or another append-only system.

OpenShell's sandbox OCSF files are stronger for snapshot custody than the gateway push buffer, which OpenShell documents as able to drop buffered events. Package inventories show observed installed state. They do not reconstruct every downloaded response byte.

## Host validation

Use these checks together. No single check proves containment.

```bash
agent-sandbox doctor
agent-sandbox audit isolation <sandbox>
agent-sandbox audit snapshot <sandbox>
agent-sandbox audit verify
agent-sandbox service list
lsof -nP -iTCP -sTCP:LISTEN
```

The dashboard gives the gateway and runtime bindings, sync receipts, latest verified snapshot, live policy, request and
proposal queues, leases, OCSF tail, teams, research state, agent-state custody metadata, catalog, and audit records. It
does not return canonical state values or event payloads. `lsof` confirms which host address owns each listener.
Host-only leases must show `127.0.0.1`; LAN listeners should exist only for approved leases. The isolation probe
verifies tested paths and network sentinels at one point in time. OpenShell policy and OCSF show allowed and denied
network activity. Package and workspace inventories show installed or changed state at snapshot time. A compromised
host, cluster control plane, gateway, runtime, kernel, or same-user process can invalidate this evidence boundary.

Provenance replay is an interactive metadata graph, time slider, and evidence inspector. It shows collection scope,
authority, epoch, event parents and hashes, harness, model route, inference parser, artifact role, and sensitivity. Pi
and OpenCode tool events, returned output, and provider-returned reasoning appear as separate nodes. The default graph
response contains no restricted content.

An operator can issue a short evidence lease for one run and one stated purpose, then read a selected event or artifact.
The same lease can search restricted evidence for bounded excerpts. Issuing, searching, and reading with the lease add
host audit records. Raw harness JSONL and returned reasoning remain owner-only. A
private state export may include those bytes only after reviewing the destination, data class, and retention rule. See
[provenance](provenance.md).

`service list` is also a reconciliation action. It checks the recorded supervisor and child process custody, marks a broken lease failed, removes a matched orphan child, and closes dependent resources. Preserve the resulting lease record and audit entries when investigating an unexpected process exit.
