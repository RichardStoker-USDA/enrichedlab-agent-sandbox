# Release qualification

Run from a clean `development` commit. Use disposable sandbox names, short TTLs, non-sensitive sample data, and a dedicated test state directory. Keep the resulting report outside the public repository if it contains host or network detail.

## Source gate

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release --locked
npm --prefix frontend ci
npm --prefix frontend run check
npm --prefix frontend run build
npm --prefix frontend audit --audit-level=moderate
for script in scripts/*.sh; do sh -n "$script"; done
node --check web/app.js
node --test tests/*.test.mjs
scripts/validate_agent_skills.sh
scripts/scan_public_boundary.sh
scripts/qualify_remote_broker.sh
git diff --check
git status --short
```

Review dependencies and scan the release binary and selected container image with current local tools. Record tool versions and findings.

Run `cargo audit --deny warnings`. A dependency advisory or unmaintained direct dependency blocks promotion until it is removed, upgraded, or recorded as an approved release exception.

Inspect the image working directory. It must not claim `/sandbox/work`, which OpenShell reserves for the controller's workspace mount. Build the provided image and create a disposable sandbox from that exact tag or digest.

## Runtime gate

- `agent-sandbox doctor` passes.
- Every required profile passes `agent-sandbox platform list` and names the expected OpenShell driver.
- A sandbox is created with hard Landlock, Policy Advisor, and OCSF enabled.
- A process can write `/sandbox/work` and the request inbox. Bind profiles cannot write the decision mount. Sync profiles prove that the decision path is a remote copy rather than a host mount and compare it with the host delivery receipt during qualification.
- A known public package endpoint works and an undeclared host is denied.
- OpenShell Policy Advisor is manual even when the sandbox uses controller `auto-safe`; a proposal can be rejected and an approved narrow proposal persists in the policy source.
- A target app bound to sandbox `127.0.0.1` is reachable through a host lease whose local port differs from the target port.
- Two independent target ports can be leased at once.
- No raw LAN listener exists for a host-only lease.
- An mTLS LAN lease accepts the named client from the allowed source and rejects no certificate, a different client, and a disallowed source.
- Revoking the client closes its lease and blocks later connections.
- A Docker or VM peer link works from its assigned source through both the reported direct endpoint and source-local relay, and is absent from another sandbox. Closing it stops the source process, removes the source policy endpoint, and closes the owned target lease.
- Killing a host supervisor and then listing leases removes the verified orphan OpenShell child and listener. Its dependent LAN lease also closes.
- Killing a peer supervisor and then listing leases stops the source relay, removes the temporary source policy, and closes the owned target forward.
- A valid custom request gets one decision in the read-only mount. Bad input does not stop other requests.
- A sync-profile decision gets a host receipt only after the controller downloads the remote copy and verifies its exact digest and size. A changed or missing remote copy fails strict decision integrity.
- A mount request captures evidence, recreates the sandbox, preserves active policy, and exposes only the approved path.
- The isolation probe passes with no intentional peer lease active.
- A snapshot manifest verifies and contains sandbox OCSF files.
- State mutations reject stale versions and unauthorized collection roots; accepted and rejected outbox files remain.
- `state verify` checks every collection schema, event chain, and stored artifact digest.
- A standard state export includes a valid manifest and omits restricted bytes and event payloads. An explicitly approved
  full export includes them and stays in a private test destination.
- Synthetic Pi, OpenCode, OpenAI-compatible, and native JSONL fixtures record their declared tool, token,
  returned-content, reasoning, and output-version events without copying raw content into prompt state. Per-step or
  per-turn tokens are summed for usage and the largest observed total sets context pressure.
- Missing terminal usage, malformed JSONL, capture truncation, observer failure, or a harness error event cannot
  produce a passing segment.
- A fresh `data-science` worker installs the selected pinned Pi or OpenCode runner through its declared npm registry
  rule before its first segment. Blocking that bootstrap produces a private `controller-bootstrap` capture.
- Two incomplete segments use separate provider sessions, checkpoint between them, increment the context epoch, and do
  not pass a continuation or session ID to Pi or OpenCode.
- A named agent is registered once, starts runtime generation one, records a state lesson, releases compute, starts a
  different runtime generation, and sees the same scoped state without reusing a provider session.
- `scripts/qualify_continuity.sh` automates that live gate, can exercise either pinned harness, retires the test
  identity, and leaves no active runtime while retaining the evidence. Pass `--root`, `--state-dir`, `--config-dir`,
  `--openshell-bin`, and `--docker-bin` when the installed controller overrides their defaults. The qualification and
  dashboard must use the same path set.
- Two processes cannot run the same logical agent concurrently. Different agents can allocate concurrently only up to
  the route cap. Retry is one attempt by default and rejects zero, more than eight, or a wait above 30 seconds.
- Provider attempts default to a 3600-second deadline, reject values below 30 or above 86400 seconds, classify an
  expired attempt as `attempt-timeout`, checkpoint it, and run automatic runtime release. The process-capture unit gate
  proves a stuck child is stopped rather than waiting without a bound.
- Route slot reservations remain visible while cold creates run. An interrupted create is reconciled against both the
  controller record and gateway; an unowned live runtime or unknown custody fails `doctor`.
- A normal named-agent run releases its sandbox after evidence and state checkpointing. `--keep-runtime` keeps it only
  when explicit. A failed sync pull or state reconciliation preserves the runtime for recovery.
- Collecting the worker stores every bounded segment log as a restricted content-addressed artifact.
- A direct Pi and direct OpenCode run return a provenance run ID and append a terminal event even when the harness
  fails. Truncated or malformed capture cannot return a successful run.
- Bounded scientific QC samples pass the result contract. A result that uses language-model decoding as computation
  fails unless it declares `llm-method` with model revision, engine, prompt, sampling, repeats, and evaluation evidence.

## PostgreSQL provenance gate

- Migrations run with a separate owner DSN; the long-running controller has only the runtime DSN.
- The runtime role is `NOSUPERUSER NOBYPASSRLS`, does not own the tables or schema, and cannot create schema objects.
- Two tenant configurations cannot read each other's runs, events, or evidence leases.
- Event update and delete fail for the runtime identity and the migration owner.
- Concurrent appends to one run produce one sequence and valid previous-hash chain.
- A remote DSN without TLS is rejected. Loopback-only plaintext is accepted for disposable local qualification.
- An evidence lease is scoped to one tenant and run, expires, increments use count, and cannot read another run.
- Dashboard graph loading against PostgreSQL does not block or panic the async runtime under concurrent polling.
- Backup and restore preserve event ordering, hashes, RLS policies, append-only triggers, and schema migration state.

## Remote broker gate

- The client sends the interactive token as `Cf-Access-Token`; only Cloudflare supplies
  `Cf-Access-Jwt-Assertion` to the origin.
- The broker accepts only the exact Access issuer and audience with RS256, expiry, current JWKS, a stable subject, and
  a bounded verified email. Missing keys fail closed.
- An invited identity binds once. Removing its declarative invitation disables access and removes group membership;
  re-adding it is tested before use.
- The API and worker use distinct function-only PostgreSQL roles and cannot read tables or call each other's functions.
- Task submission is idempotent, a changed replay conflicts, and the queue cap rejects excess work.
- Total group and per-principal active limits hold under concurrent claims.
- A second principal cannot list, read, cancel, stream, list artifacts for, or download another principal's task.
- Worker renewal, expiry, retry, cancellation, and terminal commit obey the lease generation fence. Provider attempts
  and task-disruption attempts are separate bounded values.
- API restart and client disconnect do not cancel an accepted task. Worker shutdown stops its child process group;
  expiry requeues within the task-attempt limit.
- A passing task exports complete scoped state and provenance, including owner-approved restricted reasoning and tool
  I/O, before terminal success. Export failure produces `task_custody_incomplete`.
- Workspace copy rejects symlinks and non-regular files, stays within configured file and byte limits, and hashes each
  artifact. Server and client both verify a download's size and SHA-256.
- Every broker Kustomize package renders and passes client dry-run. The example's zero digest and reserved values must
  remain visibly non-deployable.
- A live site test proves the Access edge, tunnel, raw-origin denial, NetworkPolicies, database TLS, exact OpenShell
  gateway, sandbox placement and RuntimeClass handler, one/three/five-agent capacity offered to users, cleanup, and
  database plus volume restore.
- One RWO controller and artifact tree has one worker leader. Kueue and SandboxWarmPool behavior is not claimed because
  the broker does not integrate them.

## VM and Kubernetes gate

Run this section for every enabled sync profile.

- Driver configuration contains no host bind mounts.
- Initial create pushes the bounded host workspace and records its digest.
- A remote edit appears in `sync pull` snapshot without changing host work.
- `sync pull --apply` backs up the old host tree, applies the snapshot, and records both paths.
- A host service whose host and target ports differ works through `openshell forward service`.
- A peer relay is reachable only from its named source and is removed with its source policy rule on close.
- A Kubernetes peer creates exactly two lease-labelled NetworkPolicies, uses an authenticated TLS relay without a Service or host listener, rejects an unrelated sandbox and a bad capability, then removes both policies and capability files on close.
- The snapshot contains the runtime proof and remote sync receipt.
- Delete removes the OpenShell sandbox and controller runtime record while keeping host work by default.

For a Kubernetes Kata profile, also verify one matching Sandbox CR and Pod, the requested RuntimeClass on that Pod, its installed handler, intended node selection, non-host namespaces, container image identities, restart counts, and namespace NetworkPolicies. Save `platform refresh` output in private release evidence. A healthy OpenShell gateway response without the independent Kubernetes proof does not pass this gate.

For a direct VM profile, save a host-runtime receipt for the guest or hypervisor process if the deployment needs proof below the OpenShell driver identity. If the guest kernel lacks Landlock, record and approve `best-effort`; do not report hard Landlock.

## Team and catalog gate

- Two workers receive separate clones, branches, assignments, and non-overlapping ownership.
- A worker commit inside its owned paths collects and merges.
- A host merge succeeds with system and global Git configuration disabled and no inherited author or committer variables.
- A deletion or rename of an unowned path is rejected.
- A commit not descended from the frozen base is rejected.
- No worker clone has a Git remote.
- Own and shared output mounts match the team record.
- `team stop` retains workers for restart; `team delete` removes their runtime resources while keeping team files and outputs.
- Catalog registration rejects symlinks, verifies unchanged output, and detects a content change.

## Research gate

- One synthetic study snapshots a fixed question, protocol, input, and two distinct perspectives.
- Workers use separate sandboxes and cannot read sibling study workspaces.
- Route `pool_cap` and study output file and byte limits fail closed.
- Selected catalog tools are copied only after digest verification; later changes are detected.
- Collection rejects changed input, question, protocol, perspective, assignment, tool, result identity, or escaping evidence path.
- Two complete workers seal outside their workspaces, compare, and reverify.
- Comparison records normalized answer groups and keeps `external_validation_required = true`; it never reports consensus as validation.
- Stopping and deleting workers leave the study, recovery data, logs, outputs, and seals under host custody.

## Dashboard gate

- The server rejects a non-loopback bind.
- Security headers and CSP are present.
- A cross-origin or missing-intent mutation is rejected.
- Active policy and recent OCSF sections render without inserting untrusted HTML.
- Gateway, runtime proof, sync, evidence-summary, research-summary, agent-state summary, and audit-chain sections render from live state.
- Logical-agent identity, lifecycle, active runtime, generation, state profile, session count, and last result render
  without returning prompt-state values.
- The Agent state disclosure renders collection scope, event authority and hashes, and artifact custody while excluding
  collection values, event payloads, and artifact bytes from the API response.
- The shared state SSE feed reports a stable semantic revision, marks a stopped feed stale, reconnects, and does not
  block a clean server shutdown while a browser connection is open.
- Global metadata search routes to exact views. Audit search is server-paged at 50 rows inside a bounded scroll region.
- Provenance replay renders a live Svelte Flow graph, run selector, filters, time slider, step controls, baseline
  comparison, model and parser metadata, and linked artifact custody. Lane labels pan and zoom with their rows; the
  initial fit and labeled minimap remain usable.
- Restricted event and artifact content cannot be read before a short purpose-bound lease is issued. Cross-origin
  lease, search, or evidence requests fail and every successful search and read reaches the host audit chain.
- Graph truncation and the browser node cap are visible. A large run does not freeze navigation.
- Decision receipt state renders, and `/metrics` returns low-cardinality Prometheus text without names, paths, request content, policy bodies, network addresses, or model metadata.
- `Capture evidence` creates a verified snapshot and publishes the changed state revision.
- Manual request and proposal decisions work.
- Auto-safe reconciliation applies only the two enabled action types.
- Light and dark themes persist, and the page remains usable at desktop, tablet, and phone widths.

If no supported browser surface is connected to the qualification session, record the visual theme and responsive checks as blocked by that environment. Do not replace them with a different browser-control path. The API, security-header, same-origin, static-markup, and automated UI tests still run, but they do not count as visual browser evidence.

## Cleanup gate

- All test leases are stopped or expired and their supervisor PIDs are gone.
- All test sandboxes are deleted through the controller.
- No test listener remains on loopback or a LAN address.
- No temporary OpenShell peer rule remains.
- Test work data is moved to trash or an explicit recovery location.
- The controller audit chain still verifies.
- `docker ps` shows no test sandbox container.
- Repository status contains only intended source changes; no state, key, log, trace, or evidence file is staged.

Promote the exact qualified commit to `main`. Tag and publish only after the cleanup gate passes.
Before a public visibility change, verify that local and remote `main` and `development` point to the intended release
commit and rerun the public-boundary scan across every reachable ref.
