# EnrichedLab Agent Sandbox

EnrichedLab Agent Sandbox runs disposable coding and research agents behind a host-controlled security boundary. The
Rust controller creates sandboxes through [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell), limits mounts and
network access, opens services through short leases, and keeps approvals, evidence, and final integration on the host.

The public repository contains no inference endpoints, credentials, host paths, or cluster coordinates. You connect it
to an OpenShell gateway and model routes that you already control.

## Set it up with a coding agent

Clone the repository and give your host coding agent this instruction:

```text
Read .agents/skills/enrichedlab-agent-setup/SKILL.md. Set up this repository for me, ask only for choices you cannot
discover safely, qualify the exact platform, and give me the setup receipt described by the skill.
```

The setup skill detects whether the agent can run commands. An agent with shell access does the work and stops before
changes that need your authority. A chat-only agent gives one command at a time and checks each result. It routes to
separate install and qualification skills so a weaker model does not have to interpret one large instruction file.

`SKILL.md` is uppercase because that is the filename defined by the
[Agent Skills specification](https://agentskills.io/specification). The focused skills are in
[`.agents/skills`](.agents/skills).

## Manual Mac or Linux setup

This short path is for an existing OpenShell Docker gateway. On macOS, OrbStack can supply the Docker engine.

```bash
openshell status
docker info
scripts/setup_local.sh --install-service --install-skills
export PATH="$HOME/.local/bin:$PATH"
agent-sandbox doctor
```

The script builds `agent-sandbox`, creates owner-only config and state, registers the selected gateway profile, links
the focused skills, and installs the loopback dashboard service. It does not install an OpenShell gateway, change a
Kubernetes cluster, or configure a model endpoint.

Defaults:

- workspaces: `~/agent-sandboxes`
- config: `~/.config/enrichedlab-agent-sandbox`
- private state: `~/.local/state/enrichedlab-agent-sandbox`
- dashboard: `http://127.0.0.1:32100/`

Use [getting started](docs/getting-started.md) for a direct VM or an existing Kubernetes/Kata cluster. Do not use the
Kubernetes commands without reading the platform status first.

## Remote cluster agents

The authenticated broker lets Pi, OpenCode, or the remote CLI submit a task to Kubernetes and disconnect. Cloudflare
Access handles the site's identity-provider and MFA flow. The broker verifies the signed origin assertion, binds the
principal to an invited research group, applies group and per-user quotas, and keeps task, state, provenance, reasoning,
workspace, and artifact custody after OpenShell releases the sandbox.

Researchers receive one HTTPS origin and the `agent-sandbox-remote` client. They do not receive Kubernetes, OpenShell,
database, storage, or controller credentials:

```bash
agent-sandbox-remote configure --server https://agents.example.invalid
agent-sandbox-remote login
agent-sandbox-remote me
agent-sandbox-remote submit \
  --group <assigned-group> \
  --profile <assigned-profile> \
  --objective-file /absolute/path/objective.txt \
  --idempotency-key <stable-request-key>
```

The public Kubernetes files are reserved-value templates, not a deployable site configuration. Read
[remote broker](docs/remote-broker.md) for the client, Pi/OpenCode adapters, private Flux overlay, database roles,
Access checks, and current RWO scaling boundary.

## Browser research workspace

The separate [research workspace client](clients/workspace/README.md) connects to the authenticated broker's task,
event and artifact APIs. It includes a loopback synthetic preview, investigation submission and reconnection,
execution history, and checksum-verified downloads. It does not replace the operator dashboard or grant new broker
capabilities. Its README records the current limits and same-origin deployment contract.

## Research broker

The broker's research side lets a research lead model work a lab's question through many steps without a browser or a
sandbox staying open. The model chooses methods and explains results; result values reach records, figures and
documents through code, never retyped by the model. Results stay unvalidated until a person reviews them.

- [Durable research coordination](docs/research_coordinator.md): the `coordinate` process that plans steps, proposes
  analyses and records each decision after a methods check and statement checks.
- [Research projects](docs/research_projects.md) and [campaigns](docs/campaigns.md): revisioned, long-running projects,
  and programs of bounded runs over one lab.
- [Guarded database queries](docs/reference_sql.md) and [reviewed query tools](docs/reviewed_queries.md): read-only SQL
  that the guard admits, and named, typed questions over a registered source.
- [Template analyses](docs/template_analyses.md) and [figure versions](docs/figure_versions.md): reviewed R code and
  figures that no model wrote.
- [Lab datasets](docs/lab_datasets.md), [lab library](docs/lab_library.md) and [lab notes](docs/lab_notes.md): a lab's
  own tables, papers and shared observations, each checked on the way in.
- [Result digests](docs/result_digests.md), [a person's review](docs/person_review.md) and
  [answer documents](docs/answer_documents.md): what the lead reads of a result, a scientist's decision on it, and
  Word, Excel and PowerPoint files written from stored results.
- [Evidence custody](docs/evidence_custody.md) and the [rerun kit](docs/rerun_kit.md): digest-addressed storage for every
  byte a record names, and a package a scientist can check and rerun on a laptop or an HPC.

## First sandbox

The default public route is `none`, so this test needs no model:

```bash
agent-sandbox create --name first-worker --route none
agent-sandbox exec first-worker -- sh -lc 'printf "ready\n" > /sandbox/work/result.txt'
agent-sandbox audit isolation first-worker
agent-sandbox audit snapshot first-worker
agent-sandbox delete first-worker --trash-workspace
```

To run an agent, configure a private inference route from [the route example](examples/routes.json), then use its local
name:

```bash
agent-sandbox create --name code-worker --route local-agent
agent-sandbox run code-worker --harness open-code -- 'Inspect the assigned files and write the result under /sandbox/work.'
```

Model credentials stay in the OpenShell gateway or provider integration. Do not put them in `routes.json`.
Use `--harness pi` for the pinned Pi runner. Both runners stream tool, usage, output, error, and provider-returned
reasoning events into the provenance store and return a run ID.

For a subagent that must keep its name, role, lessons, and work after compute is deleted, register a logical agent:

```bash
agent-sandbox agent create cluster-helper \
  --domain cluster-ops \
  --state-profile cluster-operator \
  --objective 'Inspect declared cluster state and retain evidence-backed operating lessons.' \
  --route local-agent

agent-sandbox agent run cluster-helper -- '<one bounded task and acceptance check>'
```

The run starts a fresh provider session and allocates a sandbox only when needed. It checkpoints state and releases
compute afterward. Use `--keep-runtime` only for an intentional warm session. Read
[named-agent continuity](docs/continuity.md) before enabling retries or connecting a Team Lead.

## Dashboard and service ports

The dashboard shows the state the controller can prove: gateway observations, sandbox records, runtime proofs, policy,
OCSF events, approvals, service leases, workspace-copy receipts, evidence snapshots, teams, research studies, catalog
digests, agent-state run metadata, and the host audit chain. One Rust observer feeds live revisions to Svelte over SSE.
Global metadata search, paged audit review, and Provenance replay do not put restricted bytes into the normal state
response. The dashboard stays on loopback. It does not prove that a compromised host, cluster control plane, CNI, or
hypervisor is safe.

A web app inside a sandbox must listen on sandbox loopback. Open one lease per port:

```bash
agent-sandbox exec code-worker -- sh -lc 'nohup python3 -m http.server 8000 --bind 127.0.0.1 >/sandbox/work/service.log 2>&1 < /dev/null &'
agent-sandbox service open code-worker --target-port 8000 --local-port 38080 --ttl-minutes 30 --label preview
agent-sandbox service list
```

Open `http://127.0.0.1:38080/`, then close the lease by ID. The controller uses `openshell forward service` with separate
target and host ports. It does not use the ambiguous `openshell forward start <port>` form. LAN access is a separate,
mTLS-authenticated lease with narrow source networks. See [service access](docs/services.md).

## Independent research workers

The research workflow snapshots the question, protocol, inputs, perspectives, and selected tool-catalog entries. Each
worker gets a separate sandbox and output tree. The host seals results, compares normalized answers, and keeps enough
methods and rerun metadata for review. Agreement is recorded, never reported as scientific validation.

```bash
agent-sandbox research create crop-check \
  --question 'Calculate the requested result and record every transformation.' \
  --input /absolute/path/to/input.csv

agent-sandbox research add crop-check direct \
  --perspective 'Use a direct reproducible calculation.' \
  --route local-agent \
  --harness open-code

agent-sandbox research add crop-check independent \
  --perspective 'Use an independent method and check assumptions.' \
  --route local-agent \
  --harness pi

agent-sandbox research launch crop-check --max-parallel 2 --max-segments 8
agent-sandbox research collect crop-check direct
agent-sandbox research collect crop-check independent
agent-sandbox research compare crop-check
agent-sandbox research verify crop-check
```

Each research worker also gets schema-checked working state. The model sees only the collections and JSON pointers
selected by its profile. The controller automatically records Pi or OpenCode tool metadata, token classes, output-file
versions, context epochs, and bounded raw logs outside the prompt. Each segment starts a fresh provider session from
the fixed assignment and current scoped view. Bind-mounted local workers reconcile state and version output after
completed tool events. Kubernetes and VM workers reconcile and version output after each segment pull.

A custom harness can ingest reviewed OpenAI-compatible streams or the strict EnrichedLab native JSONL schema. Codex,
Claude Code, and OMP can operate the host CLI, but their private transcript formats are not silently treated as
evidence. Disposable workers continue to use the qualified Pi or OpenCode runtime adapter.

```bash
agent-sandbox state list
agent-sandbox state verify
agent-sandbox state export <run-id> --output-dir /absolute/new-review-package
```

The dashboard's Provenance replay shows a live graph and time slider for steps, tools, state changes, returned
reasoning, and artifacts. Lane labels pan and zoom with the graph. Restricted bytes and restricted search need a short
run-scoped lease. SQLite is the local default; PostgreSQL adds a shared
tenant-scoped event plane for controller shards. Read [provenance](docs/provenance.md) before changing storage.

Routes can point at local inference or approved API-backed inference through OpenShell. Provider API billing and a
consumer chat subscription are separate unless that provider documents a harness login path. Read
[research teams](docs/research.md) and [agent state](docs/agent-state.md) before using scientific or controlled data.

## Platform status

| Path | Status in this release |
|---|---|
| Apple Silicon macOS with OrbStack Docker | Reference path, tested by the project; requalify on your host |
| Direct OpenShell VM on macOS | Tested by the project, with a recorded Landlock limit on the tested guest |
| One K3s/Kata deployment | Reference path, tested on one deployment; requalify every cluster |
| Authenticated multi-user Kubernetes broker | Reference path; identity, network, storage, and load gates required per site |
| Linux Docker Engine | Source qualified; live runtime qualification needed per host |
| Docker Desktop on macOS | Not live qualified here |
| Podman | Configuration type only, not qualified |
| WSL2 with Docker Desktop | Upstream OpenShell path is experimental; not live qualified here |
| Native Windows | Unsupported |

NVIDIA marks the OpenShell Kubernetes Helm path experimental. This project has one tested K3s/Kata reference, not a
generic production-Kubernetes claim. Read the full [platform matrix](docs/platforms.md).

## Security scope

OpenShell enforces filesystem, process, and network policy below the agent harness. This controller adds host-only
decisions, exact service leases, mTLS LAN access, bounded workspace sync, runtime observations, private state modes,
evidence capture, tool digests, and host-led Git integration. A sandbox cannot approve its own request.

The controller does not protect against a compromised operator account, host kernel, container or microVM runtime,
OpenShell gateway, Kubernetes control plane, CNI, or model provider. Read [SECURITY.md](SECURITY.md) before adding host
mounts, LAN listeners, API routes, or cluster permissions.

## Docs

- [Getting started](docs/getting-started.md)
- [Agent-led setup and harnesses](docs/harnesses.md)
- [Daily operations](docs/operations.md)
- [Service and peer access](docs/services.md)
- [Research workers](docs/research.md)
- [Scientific method contract](docs/scientific-methods.md)
- [Agent state and provenance](docs/agent-state.md)
- [Named agents and disposable compute](docs/continuity.md)
- [Provenance timeline and evidence](docs/provenance.md)
- [Cluster deployment design](docs/cluster-deployment.md)
- [Authenticated remote broker](docs/remote-broker.md)
- [Team branches](docs/teams.md)
- [Dashboard and evidence](docs/evidence.md)
- [Platform support](docs/platforms.md)
- [Architecture](docs/architecture.md)
- [Google AX assessment and integration plan](docs/ax_integration_review.md)
- [Automation API](docs/api.md)
- [NIST CSF and AI RMF mapping](docs/nist-ai-rmf.md)
- [Release qualification](docs/release.md)
- [Troubleshooting](docs/troubleshooting.md)

## License

MIT. See [LICENSE](LICENSE).
