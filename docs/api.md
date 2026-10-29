# Automation interface

## CLI

The CLI is the primary host-agent interface. Pass `--output json` as a global option for structured output:

```bash
agent-sandbox --output json list
agent-sandbox --output json service list
agent-sandbox --output json request list
agent-sandbox --output json platform list
agent-sandbox --output json platform refresh <sandbox>
agent-sandbox --output json sync pull <sandbox>
agent-sandbox --output json research status <study>
agent-sandbox --output json research verify <study>
agent-sandbox --output json state list
agent-sandbox --output json state verify
agent-sandbox --output json provenance prepare
agent-sandbox --output json provenance graph <run-id>
```

Use argument arrays when starting the process. Do not build a shell string from model output. Sandbox `exec` is the one command that intentionally passes trailing arguments to a process inside the sandbox.

The host harness should keep these responsibilities:

- choose sandbox, route, profile, mounts, and task;
- decide manual access requests;
- assign team ownership;
- define research protocols, independent perspectives, route caps, and validation criteria;
- choose state profiles, context reset thresholds, audiences, and export data classes;
- collect and review worker commits;
- merge and publish from a host-only repository;
- close leases and delete or retain sandboxes;
- verify audit and catalog state.

Subagents may create typed request JSON but do not call the privileged host CLI.

## Loopback API

The dashboard serves a versioned local API:

| Method | Path | Use |
|---|---|---|
| GET | `/api/v1/health` | readiness without controller state |
| GET | `/api/v1/state` | current cached dashboard snapshot |
| GET | `/api/v1/state/stream` | SSE revision, digest, health, and stale-state feed |
| GET | `/api/v1/search` | paged metadata search across controller records |
| GET | `/metrics` | low-cardinality Prometheus text metrics |
| GET | `/api/v1/provenance-status` | selected backend, tenant, run summaries, and chain status |
| GET | `/api/v1/provenance/{run}` | bounded metadata graph for one run |
| GET | `/api/v1/provenance/{run}/stream` | SSE graph updates for one run |
| POST | `/api/v1/provenance/{run}/lease` | issue a short evidence lease for a stated purpose |
| POST | `/api/v1/provenance/{run}/event/{event}/evidence` | read one leased restricted event |
| POST | `/api/v1/provenance/{run}/evidence/search` | search leased restricted evidence for one run |
| POST | `/api/v1/provenance/{run}/artifact/{sha256}/content` | read one leased artifact |
| GET | `/api/v1/request-schema` | custom request schema |
| POST | `/api/v1/request/{sandbox}/{id}/approve` | approve a custom request |
| POST | `/api/v1/request/{sandbox}/{id}/reject` | reject with JSON reason |
| POST | `/api/v1/proposal/{sandbox}/{chunk}/approve` | approve an OpenShell proposal |
| POST | `/api/v1/proposal/{sandbox}/{chunk}/reject` | reject with JSON reason |
| POST | `/api/v1/lease/{id}/close` | close a lease and children |
| POST | `/api/v1/evidence/{sandbox}/capture` | capture and verify a new evidence snapshot |
| POST | `/api/v1/settings` | update the two auto-safe toggles |

Mutation and evidence calls must originate from the dashboard's exact loopback origin and carry their operation-specific
`X-Agent-Sandbox-Intent` header. Evidence reads also need a current run-scoped lease token. A custom host harness can
meet that contract because it already runs in the trusted same-user boundary, but the CLI is less coupled and gives
clearer process custody.

The schema 8 state response includes gateway health, runtime proofs, sync reports, host decision-delivery receipts,
latest verified evidence summaries, research summaries, logical-agent lifecycle and route-reservation summaries,
agent-state run summaries, audit-chain status, and the existing operating views. Research summaries exclude question, prompt, input, result, and
raw log bodies. Logical-agent summaries omit objective, role, owner, workspace, runtime arguments, and state values.
Agent-state summaries include counts, profile, subject, status, and epoch. Each run also has a bounded metadata trace
with collection scope, event sequence, authority, payload hash, artifact role, sensitivity, and content digest. It
includes no collection values, event payloads, or artifact bytes. The provenance graph adds event lineage, model and
harness metadata, and artifact links without returned content. The API is not exposed to a sandbox or LAN. It has no
user-authentication layer and is not safe to bind outside loopback. Other controller views include host paths and
policy detail, so do not proxy it through a sandbox service lease.

One shared observer refreshes controller state every two seconds. `/api/v1/state` returns its cached snapshot instead
of running a new platform probe per browser. The stream publishes one-second status frames containing a monotonic
revision, semantic state digest, observed time, health, and last-error time. Probe timestamps are excluded from the
digest, so a revision changes only when observed state changes. A disconnected browser marks the view stale and can
reconnect without creating a second observer.

`/api/v1/search` accepts `q`, `scope`, `limit`, and `cursor`. Scopes are `all`, `provenance`, `state`, `artifacts`,
`agents`, `requests`, `audit`, `research`, `teams`, `services`, and `catalog`. The query is limited to 128 characters,
pages to 100 rows, and the result window to 10,000 rows. This endpoint searches metadata, identifiers, labels, hashes,
and custody fields. It does not search state values, event payloads, artifact bytes, or returned reasoning.

Restricted evidence search is a separate POST. It needs the `search-evidence` intent header and a current token for the
exact run. The controller limits the query, page, and search window, returns bounded UTF-8 excerpts, and writes only a
query SHA-256 digest to the host audit record. Reading a matched event or artifact remains a separate audited call.

`/metrics` reports counts and integrity state without sandbox names, host paths, policy bodies, request content, network
addresses, or model metadata. It includes managed sandbox, logical-agent, allocated logical-runtime, lease, request,
team, research, state-run, state-event, artifact-link, catalog, gateway, audit-chain, evidence, and verified or
unverified decision-receipt gauges. Keep it on the same loopback listener. If a local collector scrapes it, that
collector joins the trusted same-user boundary.

## Remote broker API

The remote broker is a separate Axum service behind Cloudflare Access. It does not expose the loopback controller API:

| Method | Path | Use |
|---|---|---|
| GET | `/health/live` | process liveness, no identity data |
| GET | `/health/ready` | PostgreSQL schema readiness, no identity data |
| GET | `/api/v1/me` | verified principal, group quotas, assigned profiles with whether each is `offered` to run on its own and whether a research run may list it (`in_runs`, migration 060: true for the template-analysis method, false for the figure and dataset methods), each lab's `lab_data` (exactly one enabled dataset method is granted to it, migration 061), `change_stream` (whether the change stream is served), and the site's presentation names |
| POST | `/api/v1/labs/{lab}/campaigns` | a holder of `research.discovery.autonomous` creates a planned campaign with `campaign_id`, goal, constraints, starting points, profiles, budgets and optional stop-rule thresholds; HTTP 201 ([research campaigns](campaigns.md)) |
| GET | `/api/v1/labs/{lab}/campaigns?limit=` | up to one hundred campaign summaries for a current capability holder in that lab |
| GET | `/api/v1/campaigns/{campaign}` | the bounded campaign view with its branch tree, steps and newest one hundred records, for a current capability holder |
| POST | `/api/v1/campaigns/{campaign}/start` | starts a planned campaign after rechecking its lab method grants |
| POST | `/api/v1/campaigns/{campaign}/pause` | fences new steps and moves a campaign toward paused after its running research runs drain |
| POST | `/api/v1/campaigns/{campaign}/resume` | resumes a paused campaign after rechecking scope |
| POST | `/api/v1/campaigns/{campaign}/stop` | fences new steps, drains running research runs, then disables the campaign principal |
| GET | `/api/v1/campaigns/{campaign}/events?after=&limit=` | append-only campaign events after a numeric sequence, at most two hundred |
| GET | `/api/v1/campaigns/{campaign}/records?after=&limit=` | campaign memory records after a numeric sequence, at most two hundred |
| GET | `/api/v1/campaigns/{campaign}/planner-calls?after=&limit=` | append-only planner-call starts joined to their terminal outcome and context/reply digests, at most two hundred; terminal fields are null in flight and reads require the current campaign capability |
| GET | `/api/v1/campaigns/{campaign}/planner-texts/{sha256}` | one exact context or admitted reply named by that campaign's planner calls as `sha256`, `bytes`, and `text`, for a current campaign capability holder |
| GET | `/api/v1/changes/stream?after=` | the reader's change stream: SSE `ready`, `changed` with the keys to read again and whether work runs in each named project, `reset` when everything must be read again, resumable by position or `Last-Event-ID` ([change feed](change_feed.md)) |
| POST | `/api/v1/tasks` | idempotent durable task submission, returns HTTP 202 |
| GET | `/api/v1/tasks` | current principal's recent tasks |
| GET | `/api/v1/tasks/{task}` | the task and its objective, for the task's readers |
| POST | `/api/v1/tasks/{task}/cancel` | a cancellation request, by a person who may cancel the task |
| GET | `/api/v1/tasks/{task}/events` | paged lifecycle events, for the task's readers |
| GET | `/api/v1/tasks/{task}/events/stream` | reconnectable SSE lifecycle stream, for the task's readers |
| GET | `/api/v1/tasks/{task}/log/stream` | the runner's sanitized live output as SSE pieces, resumable with `Last-Event-ID`, ending with a `closed` event, checked when the attempt was closed ([live output](live_output.md)) |
| GET | `/api/v1/kinds` | the result kind registry the broker was built with; its digest is the `ETag` ([result kinds](result_kinds.md)) |
| GET | `/api/v1/tasks/{task}/artifacts` | researcher-visible artifact metadata for the task's readers, each with the kind its bytes hold under the current registry, and that registry's digest |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/kind` | one artifact's kind fields with the check's full receipt |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/download` | size and SHA-256 checked artifact stream; one `Range` gives 206 after the whole file is checked ([result tables and byte ranges](result_tables.md)) |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/contents` | verified member listing of a retained result package, each member with the kind its bytes hold, or the kind its writer declared ([result kinds](result_kinds.md)) |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/contents/file?path=` | one checked member of that package; one `Range` gives 206 after the package is checked |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/pages?path=&offset=&limit=&sort=&order=` | one page of a CSV or TSV table's stored values, in file order or sorted by one column |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/summary?path=` | each column's inferred type, counts and stored extremes, with a receipt bound to the table's digest |
| GET | `/api/v1/tasks/{task}/generated-code` | `{"generated_code": …}`: the model-written block, template and admission record, or `null` for a fixed method |
| GET | `/api/v1/tasks/{task}/query` | `{"query": …}`: the database query the lead wrote and the text that ran, its source, snapshot and receipts, or `null` for a task that ran no query ([guarded database queries](reference_sql.md)) |
| GET | `/api/v1/tasks/{task}/figure` | `{"figure": …}`: a figure version's request, its source analysis and every receipt, or `null` for any other task ([figure versions](figure_versions.md)) |
| GET | `/api/v1/tasks/{task}/figures` | `{"versions": […]}`: the figure versions made from an analysis that the reader may open, newest first |
| GET | `/api/v1/tasks/{task}/document` | `{"document": …}`: an answer document's request and every receipt, or `null` for any other task ([answer documents](answer_documents.md)) |
| GET | `/api/v1/research-runs/{run}/documents` | `{"documents": […]}`: the newest 50 answer documents made from a run that the reader may open, each with its newest receipt |
| POST | `/api/v1/projects/{project}/documents` | a person asks for a Word document, Excel workbook or PowerPoint slides of a run's answer (`run_id`, `kind`, `request_id`); the run must have stopped on its answer |
| GET | `/api/v1/figures/catalog` | `{"catalog": …}`: the render service's charts and settings as the worker last read them, with the reader's labs whose figure method is enabled, or `null` before the first read |
| POST | `/api/v1/projects/{project}/figures` | a person asks for a figure version from one result table of a finished analysis (`source_task`, `table_path`, `feature`, `controls`, `title`, `previous`, `request_id`); HTTP 202 with `{"task_id"}` |
| POST | `/api/v1/projects/{project}/figures/previews` | the SVG a version with these settings would have, drawn by the worker that reaches the render service, without making a version ([figure versions](figure_versions.md)) |
| PUT | `/api/v1/projects/{project}/figures/{figure}/manuscript` | marks one completed version (`{"version"}`) of a figure as the one in the manuscript |
| DELETE | `/api/v1/projects/{project}/figures/{figure}/manuscript` | clears that mark; 204 |
| GET | `/api/v1/tasks/{task}/template-analysis` | `{"template_analysis": …}`: a template analysis's request, its source analysis and every receipt, or `null` for any other task ([template analyses](template_analyses.md)) |
| GET | `/api/v1/tasks/{task}/template-analyses` | `{"template_analyses": […]}`: the template analyses made from an analysis that the reader may open, newest first |
| GET | `/api/v1/template-analyses/catalog` | `{"catalog": …}`: the render service's analysis templates and settings as the worker last read them, with the reader's labs whose template-analysis method is enabled, or `null` before the first read |
| POST | `/api/v1/projects/{project}/template-analyses` | a person asks for a template analysis of one result table of a finished analysis (`source_task`, `table_path`, `feature`, `settings`, `title`, `previous`, `request_id`); HTTP 202 with `{"task_id"}` |
| POST | `/api/v1/projects/{project}/datasets?request_id=` | a person sends a lab's table as the body, named by `X-Dataset-Name`; the upload guard decides and the decision is recorded with the dataset; HTTP 201 with `{"dataset", "created": true}`, or 200 with `created: false` for a request id already used ([lab datasets](lab_datasets.md)) |
| GET | `/api/v1/projects/{project}/datasets` | `{"datasets": […]}`: the project's datasets, newest first, each with the guard's decision, its intake and its preparations |
| GET | `/api/v1/projects/{project}/datasets/{dataset}` | `{"dataset": …}`: one dataset with every intake receipt and its profile, and each preparation with its specification and receipts |
| POST | `/api/v1/projects/{project}/datasets/{dataset}/preparations` | a person approves a preparation of a profiled dataset (`spec`, `title`, `previous`, `request_id`); HTTP 202 with `{"task_id"}`, or 400 with the specification's `path` and `reason` |
| GET | `/api/v1/tasks/{task}/reviews` | every person's review of the analysis, newest first ([a person's review](person_review.md)) |
| POST | `/api/v1/tasks/{task}/reviews` | a reviewer's decision, note and request id; HTTP 201 with the stored review |
| GET | `/api/v1/inbox` | `{"can_acknowledge": bool, "items": […]}`: what waits for the caller across their labs, each item with its kind, reference, version and when the caller saw it ([what waits for each person](inbox.md)) |
| POST | `/api/v1/inbox/acknowledgements` | a person marks a current item as seen (`kind`, `ref`, `version`, `request_id`); the same request returns the same row |
| GET | `/api/v1/projects/{project}/events/stream` | SSE for a project reader: `project_state` on connect, then `project_changed` each time the project's fingerprint changes (saved version, title, sharing, the reader's access, each analysis's status and attempt, each work session's status, decision count, deferral code and retries; lease renewals and a session polling its running analysis do not count), as `{"pulse", "busy"}` with no research content; read every 2 s while work runs and every 10 s otherwise; `stream_complete` with reason `renew` after 15 minutes and `stream_closed` when the project can no longer be read |
| POST | `/api/v1/projects/{project}/look` | a person records the revision they last looked at; `{"revision", "at"}`, forward only ([research projects](research_projects.md)) |
| GET | `/api/v1/projects/{project}/analyses?cursor=&limit=` | `{"analyses": […], "next", "total"}`: the project's analyses past the newest 200 the project read holds, newest first |
| GET | `/api/v1/projects/{project}/acknowledgements` | `{"acknowledgements": […]}`: who saw each of the project's review requests, by display name |
| GET | `/api/v1/projects/{project}/plan-ledger` | `{"revisions": […], "unrecorded_revisions": n, "truncated": bool}`: the newest 500 checkpoint revisions, oldest first, each with what wrote it and the plan steps it changed ([plan ledger](plan_ledger.md)) |
| GET | `/api/v1/projects/{project}/working-copies` | `{"working_copies": […]}`: for each analysis the sweep has seen, `kept_until`, `cleared_at` once every run volume is gone, and `volumes` ([working copies](working_copies.md)) |
| GET | `/api/v1/labs/{lab}/notes` | the lab's shared notes for a member, and whether the caller may share and renew ([lab memory](lab_notes.md)) |
| GET | `/api/v1/labs/{lab}/tools` | `{lab, tool_groups, tools}` for a member: the tool group versions the lab holds with their profiles, and each enabled profile the lab may run with `via_groups`, kind, source, template digest, configured setting and choice labels, the reviewed queries it offers and up to three accepted analyses the caller can open; `lab_not_found` for anyone else (migration 080) |
| POST | `/api/v1/projects/{project}/notes/share` | a lab lead or reviewer shares an observation from the current saved version; HTTP 201 with the note |
| POST | `/api/v1/notes/{note}/retire` | the sharer or a lab lead or reviewer retires a note with a reason |
| POST | `/api/v1/notes/{note}/renew` | a lab lead or reviewer moves a note's review-by date |
| GET | `/api/v1/labs/{lab}/library` | the lab's papers for a member, the caller's role and whether the lab has a corpus ([lab library](lab_library.md)) |
| POST | `/api/v1/labs/{lab}/library/papers?request_id=` | a member adds a PDF; HTTP 201 with the paper, or 200 when the lab already had those bytes |
| GET | `/api/v1/labs/{lab}/library/papers/{paper}` | one paper with its abstract and figure captions from the document service |
| GET | `/api/v1/labs/{lab}/library/search?q=` | passages of the lab's papers ranked by words |
| POST | `/api/v1/labs/{lab}/library/papers/{paper}/remove` | a lab lead or whoever added the paper removes it with a reason |
| POST | `/api/v1/labs/{lab}/library/papers/{paper}/labels` | a member adds a typed label; a person removes one |
| GET | `/api/v1/knowledge/commons?kind=&after=` | a signed-in person's view of one page of the commons records of one kind with their baseline marks, through the knowledge service ([knowledge actions](knowledge_person.md)) |
| POST | `/api/v1/knowledge/resources/{id}/baseline` | a signed-in person marks a commons record baseline for every lab, or withdraws the mark (a paper with its parts); the knowledge service decides and records it in the person's name |
| GET | `/api/v1/knowledge/commons/collections` | the commons collections by source prefix with what each has marked |
| POST | `/api/v1/knowledge/commons/collections/baseline` | a signed-in person who leads the commons marks a whole collection, or withdraws the mark |
| GET | `/api/v1/knowledge/labs/{lab}/proposals` | a signed-in person's view of the links proposed from a lab's records, with whether the person may review them |
| POST | `/api/v1/knowledge/relations/{id}/review` | a signed-in person who curates or leads the lab accepts a proposed link, or rejects it with a note |
| GET | `/api/v1/tasks/{task}/lineage` | where the task came from: who asked and the method as recorded at submission, the plan step as first recorded (a later revision beside it), inputs, code, outputs, review state and named gaps; `?version=2` adds each attempt's sealed inputs, bound execution and registered outputs; `?version=3` adds the readable upstream task graph and terminal sources in one consistent read |

Every JSON read under `/api/v1/` answers with a weak `ETag` of its body and answers a matching `If-None-Match` with
304 and no body, so a view that reads again after a change notice transfers nothing when its answer is the same
([change feed](change_feed.md)).

`/api/v1/me` also returns `site`. When the broker runs with `--site-organization` (or
`AGENT_SANDBOX_BROKER_SITE_ORGANIZATION`), `site.organization` is that name, for example the research center the labs
belong to; otherwise `site` is empty. The value is presentation only: plain text of at most 120 characters, trimmed,
with no control characters or angle brackets. It grants nothing and names no person.

Project, research-run, collaborator, evidence-package and administrator routes are listed in
[research projects](research_projects.md) and [research coordination](research_coordinator.md).

The workstation sends its interactive token to the edge as `Cf-Access-Token`. Cloudflare sends
`Cf-Access-Jwt-Assertion` to the origin. The broker accepts only the origin assertion and verifies its exact issuer,
audience, RS256 signature, expiry, and JWKS. It ignores email headers and client tenant values. The tunnel must also
require Access validation so a direct origin route cannot bypass the edge policy.

Every POST and DELETE route requires `X-Agent-Sandbox-Intent: mutate`. The typed client adds it. This makes
mutations non-simple browser requests and rejects an authenticated cross-site form submission before any state change.
Task submission also requires a bounded `Idempotency-Key`.

A task outside a project is read and cancelled only by the person who submitted it. A task in a project is read by the
people who may read the project and cancelled by the people it allows to cancel ([research projects](research_projects.md)).
Group role labels do not grant oversight access. There is no browser CORS surface,
credential endpoint, arbitrary file route, Kubernetes proxy, OpenShell proxy, or remote approval API. See
[remote broker](remote-broker.md).

## Why there is no MCP server

The local operations fit a typed CLI and loopback API. A local Team Lead can call `agent create`, `agent run`,
`agent status`, `agent release`, and `agent retire` with global `--output json`; these commands keep the stable logical
identity separate from disposable compute. The loopback HTTP API remains observation and local approval control. It
does not expose logical-agent mutation routes because it is not an authenticated multi-user task broker.

The reviewed Pi, OMP, and OpenCode Team Lead adapters add owner-scoped request listing and rejection. Approval is off
unless the launcher names a low-risk kind in `ENRICHEDLAB_TEAMLEAD_APPROVAL_KINDS`. Supported kinds are loopback
`host-service`, workspace-bounded `catalog-output`, and `domain-read`. Domain reads also need an exact hostname in
`ENRICHEDLAB_TEAMLEAD_APPROVAL_DOMAINS`. LAN services, peer links, mounts, write domains, and all unlisted domains stay
human-only. The recorded decision includes the Team Lead owner and its bounded rationale.

For cluster-owned tasks, the typed `agent-sandbox-remote` client and Pi/OpenCode adapters call the authenticated broker
API. An MCP server would add another privileged parser, authentication path, and capability-discovery surface without
changing the enforcement layer. A future host harness can wrap the typed CLI while keeping OpenShell and controller
decisions below the model. A shared adapter must expose bounded lifecycle actions, authenticate a canonical tenant and
principal, and inherit the same state, request, audit, idempotency, and approval records. It must not expose arbitrary
shell execution.
