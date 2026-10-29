# NIST alignment record

This is an engineering crosswalk, not a certification, compliance claim, authority to operate, or completed risk assessment. Deployment owners still define system scope, data classes, risk tolerance, roles, and retained evidence.

Primary references:

- [NIST AI Risk Management Framework](https://www.nist.gov/itl/ai-risk-management-framework)
- [NIST AI RMF Playbook](https://www.nist.gov/itl/ai-risk-management-framework/nist-ai-rmf-playbook)
- [NIST AI 600-1, Generative AI Profile](https://nvlpubs.nist.gov/nistpubs/ai/NIST.AI.600-1.pdf)
- [NIST Cybersecurity Framework 2.0](https://nvlpubs.nist.gov/nistpubs/CSWP/NIST.CSWP.29.pdf)
- [NIST Secure Software Development Framework 1.1](https://www.nist.gov/publications/secure-software-development-framework-ssdf-version-11-recommendations-mitigating-risk)

## AI RMF functions

| Function | Project mechanism | Evidence | Limit |
|---|---|---|---|
| GOVERN | Named policy and state profiles, manual and bounded auto-safe approvals, host-led team and research ownership, threat model, release gate | controller settings, state profile, assignment JSON, research protocol and seals, decisions, release record | Human role assignment and organization policy remain deployment work. |
| MAP | Explicit host/sandbox/LAN boundary, named runtime contracts, route catalog, mount or sync records, service source and client lists, frozen research inputs and perspectives | architecture doc, sandbox record, driver-config digest, lease record, study manifest, active policy | The controller does not classify research data or infer impact level. |
| MEASURE | Gateway health, Docker or Kubernetes runtime proof, OCSF tail, verified evidence snapshot, package and workspace inventory, sync digest, isolation sentinels, automatic harness event and file-version capture, research method contracts, seals and comparison, state and artifact verification, catalog digest, tests | runtime proof, snapshot manifest, provenance graph and hashes, segment logs and workspace deltas, study verification, state event chain and export manifest, OCSF JSONL, sync report, audit chain, test output | API proofs are point in time; worker agreement is not scientific validation; host sentinels do not test every LAN device; one tool call can create and remove bytes between snapshots. |
| MANAGE | TTL leases, deny/revoke/close, fail-closed profile drift checks, bounded sync with pre-apply backup, mount rollback, evidence-before-delete, host-only merge | decision record, revoke and close audit events, sync backup, recovery directory | A compromised trusted host or cluster control-plane component is outside this control boundary. |

## Generative AI risks

| Risk area | Treatment |
|---|---|
| Excessive agency | Models propose access; OpenShell or host policy decides. Auto-safe custom actions are limited to loopback service and bounded catalog registration. |
| Data and credential exposure | Sensitive host paths are rejected, sync transport sends no host path, secrets are not mounted, control errors omit captured subprocess output, dashboard remains loopback. |
| Untrusted code and packages | Execution stays in OpenShell with declared Landlock mode and exact network policy. Package activity is observed through OCSF and package inventories. A best-effort VM profile is recorded as such. |
| Tool misuse | Service, network, peer, mount, and catalog actions have typed requests, validation, decisions, TTL or digest evidence. |
| Multi-agent conflict and correlated error | Coding workers get disjoint path ownership, separate no-hardlink clones, no remotes, and host-only collection and merge. Research workers get frozen inputs, separate perspectives and sandboxes, host seals, and an explicit external-validation requirement. |
| Accountability | Decisions, lifecycle changes, leases, harness events, model and parser metadata, catalog entries, and evidence snapshots are tied to local records and hash chains. |
| Recovery | Work folders are kept by default, destructive deletion captures evidence, mount recreation rolls back, failed worker setup is recovered rather than erased. |
| Long-horizon context and memory | Prompt-facing state is schema-checked, audience scoped, versioned, and token bounded. Research segments use fresh provider sessions and verified context epochs. The host records Pi or OpenCode events, custom native events, and output versions. Provider-returned reasoning, restricted logs, and content-addressed artifacts stay outside the prompt. Unemitted reasoning is neither claimed nor treated as authority. |
| Scientific method integrity | Bounded data samples are allowed for schema checks, QC, and tool preparation. Completed results declare a deterministic tool, statistical model, scientific model, human review, or a fully disclosed repeated LLM method. Scripts, queries, inputs, outputs, environment, seeds, failed attempts, and validation references remain under custody. |
| Tenant and evidence disclosure | PostgreSQL provenance uses forced row-level security and separate runtime and migration roles. The remote broker validates Access identity, resolves a site-owned tenant, applies invitation and group grants, and keeps task and artifact reads owner-only. Restricted local-dashboard evidence needs a short run-scoped lease and purpose. Broker artifact exports include restricted evidence for their owner, so site retention and download policy still matter. |

## CSF and SSDF notes

- Govern and Identify: boundary, platform matrix, route and asset records, explicit unsupported states.
- Protect: least mounts or bounded copy sync, private modes, mTLS, exact endpoints, declared Landlock mode, Kata RuntimeClass request, no agent Git remotes.
- Detect: gateway and runtime proofs, OCSF, active policy display, sync receipts, host audit verification, catalog and snapshot hashes.
- Respond and Recover: revoke, lease closure, deny, evidence capture, pre-apply sync backup, rollback, recovery workflow.
- SSDF: pinned base image digest, locked Rust dependencies, source review gates, branch separation, secret-free repository, and a documented release test.

## CSF 2.0 control-to-evidence matrix

`Implemented` means this repository has a mechanism and produces the named evidence. `Deployment` means the owner must supply or operate the control. `Partial` means the mechanism helps but does not cover the full category.

| CSF 2.0 category | Status | Mechanism and evidence |
|---|---|---|
| GV.OC, organizational context | Deployment | The boundary and trust assumptions are documented, but the owner defines mission, stakeholders, data classes, and legal scope. |
| GV.RM, risk management strategy | Partial | Manual and auto-safe modes, platform qualification gates, exact TTLs, and fail-closed profile checks support a local risk strategy. Risk tolerance and exception approval remain deployment records. |
| GV.RR, roles and responsibilities | Partial | Host operator, controller, OpenShell, harness, and sandbox roles are separated. Named people, alternates, training, and incident ownership remain deployment work. |
| GV.PO and GV.OV, policy and oversight | Partial | Typed policy profiles, approval records, verified remote decision receipts, evidence snapshots, and release gates support review. Scheduled review and independent oversight are not automated. |
| GV.SC, cybersecurity supply chain | Partial | Locked Rust dependencies, checksum-pinned OpenShell CLI assets, digest-pinned sandbox image, and release scans support provenance. Upstream runtime, image, and cluster component review remains deployment work. |
| ID.AM, asset management | Implemented | Gateway profiles, sandbox records, runtime proofs, service leases, teams, research studies, state runs, artifact links, catalog entries, and inventory snapshots identify managed assets. Unmanaged host and cluster assets are outside this inventory. |
| ID.RA, risk assessment | Partial | The threat model, unsupported-platform matrix, isolation probe, negative peer tests, and evidence limits identify technical risks. Mission and data impact analysis remain deployment work. |
| ID.IM, improvement | Implemented | Release qualification, troubleshooting records, audit events, and cleanup checks provide a repeatable change loop. A deployment owner decides when findings become corrective actions. |
| PR.AA, identity and access | Partial | OpenShell identity, controller ownership, mTLS LAN clients, Kubernetes Pod UID checks, per-lease capabilities, and host-only approvals bind local actions. The remote broker verifies a Cloudflare Access issuer, audience, signature, expiry, and stable subject, then applies invitation, membership, profile, and owner checks. IdP policy, MFA, account lifecycle, and Access configuration remain deployment controls. |
| PR.DS, data security | Implemented within scope | Private file modes, rejected sensitive mounts, bounded sync, pre-apply backups, decision-copy verification, digest receipts, and no secret fields cover controller-managed paths. Disk encryption and research-data classification remain host controls. |
| PR.PS and PR.IR, platform security and resilience | Partial | OpenShell policy, Landlock declaration, seccomp/runtime boundary, Kata request, loopback services, exact NetworkPolicies, process custody, and cleanup checks cover managed sandboxes. Kernel, hypervisor, CNI, cluster control plane, and host hardening remain trusted dependencies. |
| DE.CM, continuous monitoring | Implemented within scope | Loopback dashboard, `/metrics`, gateway and runtime proofs, effective policy, OCSF tail, listeners, leases, requests, research phases and segments, state collection scope, live provenance graph, artifact custody, decision receipts, and audit verification show controller-managed state. State values and restricted evidence stay out of default dashboard responses. This is not host EDR or a full packet trace. |
| PR.DS and PR.IR, state custody and recovery | Partial | Logical-agent IDs are separate from disposable runtimes. Release reconciles state, checkpoints, captures evidence, and retains the workspace. The broker adds durable task leases and owner-only state, provenance, result, and workspace artifacts on RWO storage. Shared PostgreSQL prompt state, object-store custody, signed external anchors, automatic retention, and multi-replica recovery remain deployment work. |
| DE.AE, adverse event analysis | Partial | Hash-chained audit records, OCSF snapshots, package and process inventories, failed request records, and bounded errors support analysis. Correlation, alert routing, and external audit anchoring are deployment choices. |
| RS.MA, RS.AN, and RS.MI | Partial | Access can be denied, client identities revoked, leases closed, sandboxes stopped, evidence captured, and work retained. Formal incident declaration, communications, forensics, and eradication procedures remain deployment work. |
| RC.RP, recovery execution | Implemented within scope | Sync backups, mount rollback, retained workspaces, recovery directories, profile drift checks, and verified cleanup cover controller-managed recovery. Cluster and host disaster recovery are outside the controller. |

## AI RMF profile status

| AI RMF outcome area | Status | Project treatment |
|---|---|---|
| GOVERN 1, policies and processes | Partial | Security boundary, approval policy, platform matrix, release gate, and evidence contracts are versioned. Organization policy and review cadence remain local. |
| GOVERN 2, accountability | Partial | Host decisions, team ownership, immutable assignment boundaries, state authorities, context epochs, lifecycle events, and evidence tie controller effects to local records. The remote broker links tasks to an Access principal and catalog snapshot. The loopback UI remains single-user, and organization role review remains deployment work. |
| GOVERN 6, third parties | Partial | OpenShell, Kubernetes, Agent Sandbox, runtime handlers, images, packages, and inference routes are named dependencies with qualification gates. Supplier assessment and contract terms remain deployment work. |
| MAP 1 and MAP 2, context and categorization | Partial | Named profiles, routes, data paths, authorities, and deployment boundaries support categorization. The controller does not decide a scientific study's impact level or data sensitivity. |
| MAP 4 and MAP 5, impacts and likelihood | Partial | Threat paths, unsupported states, negative access tests, exact grants, and evidence limits support impact analysis. Social, scientific, and downstream model harms need a use-case assessment. |
| MEASURE 1 and MEASURE 2, methods and evaluation | Implemented within technical scope | Unit and contract tests, driver qualification scripts, isolation sentinels, runtime proofs, policy reads, negative peer tests, sync and decision digest checks, and browser gates produce repeatable technical evidence. Model quality and scientific validity are separate evaluations. |
| MEASURE 3 and MEASURE 4, tracking and feedback | Partial | Dashboard metadata, low-cardinality metrics, OCSF, state events, content-addressed artifacts, audit events, snapshots, and troubleshooting records support monitoring. Alert thresholds, user feedback, retention, external hash anchoring, and periodic review are deployment choices. |
| MANAGE 1 and MANAGE 2, prioritization and treatment | Implemented within controller scope | Default deny, manual high-impact approval, bounded auto-safe actions, TTL leases, revocation, profile freeze, backup, rollback, and cleanup apply selected treatments. The deployment owner accepts residual risk. |
| MANAGE 3 and MANAGE 4, third-party risk and communication | Partial | Dependency pins, platform status, evidence exports, and explicit limits support review and communication. Vendor response, public disclosure, and stakeholder notification remain owner processes. |

## Evidence still needed per deployment

- approved system boundary and data categories;
- named operator and incident roles;
- retained release and live qualification output;
- vulnerability and container image scan results;
- an external anchor for the audit tail if truncation detection is required;
- periodic review of public-domain policy and issued client profiles;
- adapter-specific validation for any platform outside the tested matrix.
- cluster owner evidence that a requested Kata handler is installed, patched, and qualified on the selected nodes.
- a separate control and qualification record for any A2A, kagent, Rossoctl, warm-pool, Kueue, autoscaling, or distributed
  state adapter; none is supplied by the current controller.
- PostgreSQL backup, restore, TLS, role, principal-crossing, invitation-revocation, and append-only evidence before a
  shared deployment.
- Access policy, MFA, wrong-audience, raw-origin denial, tunnel validation, and client disconnect evidence for the
  remote broker.
- a separately authenticated and authorized multi-user web boundary before exposing browser provenance. The remote
  broker API and CLI do not turn the local dashboard into that surface.
