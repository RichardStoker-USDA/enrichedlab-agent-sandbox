# Research projects and continuity

Projects organize long-running research independently of sandbox lifetimes. Migration 010 adds collaboration and
revisioned state to the broker. It does not install a lead-agent service, authorize generated code, or prove that an
agent can complete a research goal. Those capabilities use these records through separate execution adapters.

## Access

A project belongs to one tenant, one research group and one owner. The owner must remain an enabled group member.
A private project is visible only to its owner and explicit grantees. A lab project is readable by enabled members
of its group. Reading never implies permission to contribute, steer or cancel. Explicit grants provide `read`,
`review`, `contribute`, `steer`, and `cancel`; only the owner can manage grants. `review` lets a person record a decision
on the project's analyses ([a person's review](person_review.md)); it is not an automatic approval or a runtime
permission. A person who is a `lead` or `reviewer` of the lab may share an observation from a lab project with the lab
once a person accepted the analyses it rests on ([lab memory](lab_notes.md)); a private project's notes stay in it.
Each lab also keeps a library of papers its members add and label ([lab library](lab_library.md)); it is readable by
the lab's members only and is separate from projects. Each analysis's run volumes are working copies, kept until a
retention date and then cleared; its results and the package stay ([working copies](working_copies.md)).
Removing group membership removes project access even if an old grant remains. Cross-group sharing is not implied by a common institution or email domain.

Existing tasks remain private unless submitted as new project work. Task views, events, scientific history and
artifact downloads use the same database authorization predicate. Cancellation uses a distinct capability.
Restricted artifacts stay restricted. Project visibility does not grant database, object-store, model, Kubernetes
or tool credentials.

Human oversight is separately enrolled by the operator in `broker_project_administrators`. This is not exposed as an
API mutation and is never assigned to a harness identity. Enrollment alone opens no private record. An administrator
starts a read-only inspection with a reason; the database records it and grants access for 30 minutes. The reason and
expiry are visible in project audit history. Kubernetes service-account principals cannot start an inspection.

## API

All paths are below `/api/v1`. Mutations require the existing mutation-intent header and verified authentication.
The API role has only named functions, not table access. Apply migration 010 and rerun the runtime grant script before
activating the new API. Older API/worker binaries retain compatible task and schema-readiness function signatures.

| Path | Behavior |
|---|---|
| `POST /projects` | Create a private or lab project with a caller-generated UUID, group and title. Identical retry returns the same ID. |
| `GET /projects` | List authorized project summaries, bounded by `limit`. Since migration 020 each summary also names who started it (`owner`: principal, display name, person or service account), its `question` (the goal at the current checkpoint, or null), the caller's own `capabilities`, the `latest_run` (status, code, decision count and limit) and `task_counts` (total, active, finished, failed; since migration 026 also `reviewed`, the finished analyses whose newest review by a person still covers their current result files, and of those `rejected` and `changes`), so a lab index can group and filter without opening each project. A service account's review never counts. Since migration 040 the counts leave out figure versions, which are drawings, not analyses. A project the caller cannot read is still not listed. Since migration 054 each summary also carries the caller's `last_look`. |
| `GET /projects/{id}` | Project, current checkpoint, task associations, caller capabilities, and `inspection_expires_at` while the caller reads through an administrator inspection. Since migration 040 a task that is a figure version also names its `figure`: title, chart, source analysis, table and the version it edits ([figure versions](figure_versions.md)). Since migration 058 a template analysis names its `template_analysis` the same way ([template analyses](template_analyses.md)). Since migration 054 it also carries the caller's `last_look` (`{"revision", "at"}`, both null before the first look), `analyses_total` and `analyses_next`, the cursor for the analyses past the newest 200 it holds (null when it holds them all). Each queued analysis that is to run carries `queue`: `ahead`, how many of the site's runnable queued analyses were submitted before it (the claim takes the oldest whose lab and person have a free slot), or null when it cannot start as things stand; and `lab_full` and `person_full`, whether the lab's or the person's active analyses are at their limit now. Any other analysis has `queue: null`. Counts only; no other analysis is named. |
| `POST /projects/{id}/grants` | Owner replaces one member's explicit capabilities. Empty capabilities revoke the explicit grant. |
| `POST /projects/{id}/inspect` | Enrolled human administrator starts a reasoned, recorded inspection. |
| `DELETE /projects/{id}/inspect` | The administrator ends their own inspection early; the end is recorded. |
| `GET /projects/{id}/members` | Owner only: the owner, current grants, and group members who can be invited. |
| `GET /projects/{id}/package/preview` | Counts and coverage of the evidence package without building it. |
| `GET /projects/{id}/package` | Readers download one verified `tar+gzip` evidence package; the export is recorded. |
| `GET /admin/projects` | Enrolled administrators list every project in the tenant; each listing is recorded. |
| `GET /projects/{id}/audit` | Access changes and inspection records. |
| `POST /projects/{id}/checkpoints` | Append a checkpoint with `expected_revision`, unique `request_id` and typed state. |
| `GET /projects/{id}/checkpoints` | Immutable history, paged backward by `before` revision and `limit`. Since migration 054, `after` reads forward instead: `{"checkpoints": […], "next_after": n or null}`, revisions after `after` oldest first, at most `limit` (1 to 100), with the revision to pass next while more exist. `before` and `after` together are refused. |
| `POST /projects/{id}/look` | Since migration 054, a person records the revision they last looked at (`{"revision": n}`, at most the current one) and gets `{"revision", "at"}` back. A lower revision than the stored one leaves it unchanged; the same or a higher one moves `at` to now. Service accounts cannot record a look. |
| `GET /projects/{id}/analyses` | Since migration 054, `{"analyses": […], "next": cursor or null, "total": n}`: the project's analyses newest first in the project read's item shape, `limit` 1 to 200 (default 200), after the opaque `cursor` that the project read's `analyses_next` or the previous page's `next` returned. |
| `GET /projects/{id}/plan-ledger` | Since migration 036, each revision's writer and the plan steps it changed ([plan ledger](plan_ledger.md)). |
| `GET /projects/{id}/context` | Deterministic bounded context projection, with revision, source hash and omitted-lesson count. |
| `POST /tasks` | Optional `project_id`, `action_key` and `parent_task_id` attach new work atomically to a project. |

Project action keys deduplicate across collaborators and reconnects. The key identifies one intended action, not
every request with similar text. An intentional replicate uses a new action key and records its reason and seed in
the method. A changed request under an existing key conflicts. A parent must belong to the same project. Existing
group/profile grants and execution quotas still apply to each submitter; project grants do not expand them.

## Review, evidence packages and administration

Migration 014 adds what a scientist needs to review a project and take its record home, and what an administrator
needs to oversee without silent access.

- Project evidence package. `broker_project_package_snapshot` reads every retained record the caller may read in one
  STABLE snapshot: project, every plan version, research runs with their accepted turns and every deferral, access
  history, and each task's execution record, events, artifacts, recoveries and scientific history. It refuses a
  project whose records exceed 48 MiB, 2,000 tasks, 200,000 events or 20,000 artifacts instead of truncating. Since
  migration 073 a reader that is not a person gets no package of a project any of whose tasks holds the lab's own data
  (`broker_project_package_scope`): a package carries each result's files and stored digest whole, which migration 059
  already kept from such readers one artifact at a time. The API
  builds a deterministic `tar+gzip` (fixed timestamps and ordering) with `manifest.json`, a plain-language
  `README.md`, the database records as JSON, each retained file read and checked against its recorded size and
  SHA-256, and `SHA256SUMS`. The same snapshot adds the admitted code blocks (migration 015), every person's review
  (migration 023) and the [rerun kit](rerun_kit.md) (`rerun/`: a standard-library Python verifier that checks, unpacks
  and reruns every analysis, and a Slurm script). A file whose bytes no longer match is left out and named in the manifest. Headers carry
  the archive and manifest SHA-256. At most two packages build at once. Each completed export is recorded in the
  project's access history with its checksums and size.
  Migration 068 registers digest-only package inventories under a transaction-scoped project-and-disclosure-scope
  lock. The three independent revision chains are ordinary readers with reasoning, ordinary readers without
  reasoning, and administrator inspections. A package is
  named `<lab>_<project_slug>_<project_id_prefix>_r<revision>_<UTC timestamp>`; title slugs are lowercase ASCII with
  underscores and a 48-byte bound. Any repeat of earlier content in the same scope reuses its revision and timestamp,
  including an X, Y, X sequence. The
  v2 manifest lists runs, turns, tasks and papers by ID and digest and reports added, removed and digest-changed items
  since the preceding revision. `runs/<run>/` is self-contained: it holds the run, individual turn records and
  literature ledgers, kept reasoning available to the caller, result digests, task origins and deferrals. Exact kept
  reasoning requires both current lab membership and the ordinary project-reader rule; an administrator inspection
  never grants it. Inspection and ordinary-without-reasoning packages keep touched paper identifiers but omit the
  lab-library metadata, keeping each disclosure chain uniform. A revision's comparison never reads another scope's
  inventory. Export audit rows and the requesting
  reader do not change content identity. Artifact availability is verified before revision allocation. The new package
  root appears only as the outer folder and in the manifest; the rerun kit retains its byte-identical stable contents.
- Retained package members. `GET /tasks/{task}/artifacts/{artifact}/contents` lists the members of a retained R
  result package (at most 5,000 members and 2 GiB expanded) with each member's SHA-256 and its match against the
  package's own `SHA256SUMS`. `.../contents/file?path=` streams one member with its checksum header. The browser checks
  every member again before showing it. Entries are read raw, within fixed bounds the tar library does not apply on
  its own: at most 20,000 entries of every kind, a GNU long name or link of at most 4 KiB, a PAX header of at most
  64 KiB, and 2.25 GiB decompressed in all, skipped entries included. A package that passes them, or holds a PAX size
  or an extended sparse entry, is refused (`artifact_not_a_package`, or 413 `package_too_large_to_inspect` for the
  entry and stream bounds). Two listings or table parses run at once, each off the request threads.
- Collaborators. The owner sees who holds access and which enabled group members can be invited; workload
  identities are never offered. Removing access is a grant with no capabilities.
- Administration. `broker_is_project_administrator` requires a separate enrollment of an enabled human principal.
  Listing the directory is itself recorded in `broker_admin_audit`. An administrator can end an inspection before it
  expires, and the project view reports an active inspection to that administrator only.
- Research run deferrals. `broker_research_run_events` keeps every coordinator deferral with its fixed reason code,
  including rejected model replies, after the run's failure counter resets. Consecutive waits for running tools in
  one turn collapse to one row. Rows are append-only.

Migration 015 adds model-written code in a reviewed template (see [research coordination](research_coordinator.md)).
`broker_task_generated_code` keeps the exact admitted block with its task, append-only and checked against its
digest; the package snapshot reads every block in the project in the same repeatable-read transaction and writes each
as `tasks/<task>/generated_code/block.R` with `record.json`, and the README says the code is unvalidated until a
scientist reviews it. `GET /api/v1/tasks/{task}/generated-code` returns the block, template and admission record to a
caller who can read the task.

Migration 016 records who asked for each task and serves its lineage.

- Origins. `broker_task_origins` is written in the transaction that creates a task: `person` when the API submits
  it (through `broker_submit_person_task` or `broker_submit_person_project_task`, which the API role executes
  instead of the underlying functions), or `research_run` with the run and turn when a coordinator turn commits it
  (a trigger on the turn row). Rows are append-only, and a replayed submission keeps its first origin. The migration
  fills earlier tasks once: from the turn record that submitted them, as `person` for a task outside any project
  (only its principal could submit one), and otherwise as `unrecorded` with the account named and no requester
  guessed.
- Lineage. `GET /api/v1/tasks/{task}/lineage` returns `enrichedlab.task-lineage.v1` to a caller who can read the
  task: the origin with the account's display name (never its email), the plan step that names the task, the
  project's input selection from the checkpoint current at submission, the method and its class (`agent_built` for
  admitted code, `vetted_service` for a recorded platform tool job, `agent_built` again for a reviewed-template
  profile with neither, `agent_session` for a model working with a profile's tools, `configured` for a recipe with
  settings ([configured methods](configured_methods.md)), `reviewed_query` for a reviewed template with typed parameters, or `guarded_query` for a written database query with its `query`
  record ([guarded database queries](reference_sql.md)), `vetted_service` with a `render` record for a figure
  version ([figure versions](figure_versions.md)), `vetted_service` with an `analysis` record for a template
  analysis, its input class `prior_output` recorded by `analysis_request` ([template analyses](template_analyses.md)),
  and `vetted_service` with a `dataset` record for a lab dataset's intake (input class `object_data`) or preparation
  (`prior_output`), recorded by `dataset_request` ([lab datasets](lab_datasets.md))),
  the admitted code, the execution record, the
  researcher-visible outputs with their provenance class, the review state and a list of gaps. Migration 017 sets that order: a template task with no
  admitted block and no tool job stays `agent_built` and names the gap `code_not_recorded`. A gap names a part
  the records do not hold. Since migration 039, `inputs.classes` names what kind of data the task read: a query's
  source as registered (`classes_recorded_by` is `query_source`), a template analysis's earlier result
  (`analysis_request`), a lab dataset's file or kept grid (`dataset_request`), otherwise the classes the input
  custodian recorded
  for the latest attempt's sealed copies (`input_custodian`); a task with inputs and neither names the gap
  `input_class_not_recorded`. The workspace shows it as "Where this came from" on every analysis, and the
  workbench trace uses the recorded origin instead of inferring one from the runs it has loaded.
- History, not the current catalog (migration 018). A task records its method (profile name and any template it
  was submitted under) in the transaction that creates it, whichever role submits it, and the lineage class comes
  from that record, so a later rename or template registration does not relabel earlier work. Tasks from before 018
  are bound from the catalog's dated records: a template registered after a task was submitted is not bound to it,
  and the lineage names the gap `method_recorded_later`. The plan step is the one first recorded for the task, with
  its current status; a later revision that changed its wording appears beside it as `current`, with its revision.
- Per attempt (migration 019). `GET /api/v1/tasks/{task}/lineage?version=2` returns
  `enrichedlab.task-lineage.v2`: version 1 for the same reader plus `attempts`, one entry per attempt with the
  inputs its custodian sealed (reference, class, sealed digest, custody transaction and the source snapshot), the
  execution its worker bound (image digest, template or tool, parameter digest) and the outputs it registered, each
  with the digest that attempt registered and whether it is still the task's current file. Output registration
  writes that record in the same lease-fenced call, so a retry that writes the same path no longer erases the first
  attempt's digest. The input and execution records have lease-fenced writers
  (`broker_record_attempt_input` for the custodian role, `broker_record_attempt_execution` for the worker role).
  The direct worker records the image digest an attempt will run and, as its parameter digest, the canonical digest
  of the exact Sandbox object it submits (command, arguments, limits and input volumes), after marking it running and
  before creating its Sandbox, and does not create the Sandbox when that record fails; its readiness needs
  schema 19 and that grant. The input custodian records each sealed copy whose source volume its own configuration
  classifies (`custodian.inputSources`, see [input_custodian.md](input_custodian.md)), after the seal and before
  admission; an attempt whose source is not classified names `attempt_inputs_not_recorded`, and attempts from before
  the worker's release name `attempt_execution_not_recorded`. A database input must name its query digest, snapshot
  and WAL position, and an object input its source version and digest. The records are immutable and no runtime role reads the tables.
  Version 2 keeps version 1's gaps, so `input_class_not_recorded` is absent from both when the latest attempt
  recorded its inputs.
- Upstream drill-down (migration 065). `GET /api/v1/tasks/{task}/lineage?version=3` returns
  `enrichedlab.task-lineage.v3`: the same version 2 body, with `upstream.nodes`, `edges`, `sources`, `withheld` and
  `limits`. A node keeps only its recorded title or method label, method class and record, attempt executions and
  registered outputs, review state and gaps. An edge exists only when an analysis request, dataset request, figure
  render request or sealed input record explicitly names another task's registered output; the broker does not infer
  one from a name, path or time. A custodian record names an output only through its `artifact_id` or
  `source_artifact_id` source field. Query registrations, dataset files, library papers and other sealed source-volume
  inputs are terminals and include identifiers, digests, sizes, classes and source snapshots, never rows or file
  contents. Each node uses the ordinary task-lineage reader check in the same transaction. An unreadable producer is
  not a node, edge or source, and increments `withheld`. A readable consumer's own records keep what version 2 shows of
  them, which can name that producer's task and output digest. The breadth-first
  walk visits a task once, includes root depth 0 through depth 16 and at most 200 nodes. `limits.truncated` and
  `limits.reason` (`depth` or `nodes`) say when a readable branch stopped early.

The API's readiness check requires schema 19 and its grants, and the direct worker's requires schema 19 and the
execution grant, so apply migration 019 and its grants before either image. Since version 3 the API's check also
requires migration 065 and the API role's `broker_task_lineage_v3` grant, so apply both before that API image.

## Asking for changes

The research workspace's "Ask for changes" is not a separate record. It appends one checkpoint whose constraints end
with `Scientist's instruction: <text>` (at most 900 characters; the checkpoint still holds at most 32 constraints of at
most 1,024 bytes) and whose `next_action` leads with the instruction, then starts a run with the latest run's allowed
profiles, decision limit and parallel limit. The lead reads the instruction with the goal and constraints on every
later turn, and every later checkpoint keeps it. The checkpoint's author and time attribute the instruction and its
revision versions it; the workspace shows it under "Instructions to the agent" and in the project's notebook.

Each decision records the checkpoint revision its context came from, so the workspace says when the agent read an
instruction: the first decision whose context revision is at or after the one that added it and whose checkpoint still
holds it (`instructionHistory` in `clients/workspace/src/standing.ts`). A person who may steer the project withdraws an
instruction after a confirmation in the page; that saves a checkpoint without it, and when it led the next action the
newest remaining instruction leads again. A withdrawn instruction stays listed, struck through, with who withdrew it
and whether the agent had read it; the same text given again later is a new instruction.

## Checkpoints and context

A checkpoint contains the goal, constraints, plan, lessons, next action and versioned workspace specification. The
workspace records image/source digests, input and tool references and an optional preparation receipt. The trusted
preparer must resolve and verify those references before execution; the model's assertion of readiness is not proof.

Each plan step has a stable action key, state, optional broker task and evidence references. Uncertain work remains
explicit. A task reference must belong to that project and action; a completed task claim requires a terminal
successful or review-required broker result. Completion still does not mean scientific validity. Observations require
evidence references, and hypotheses remain labeled hypotheses. References are not themselves proof that the cited
object supports a claim. A scientist checks the retained material.

Writes compare the expected revision inside a row-locked transaction. One of two competing writers succeeds;
the other receives a conflict and must read the accepted revision before changing the plan. An identical request ID
is safe to retry. Old checkpoints and audit records reject update/delete through database triggers. Their content is
research state, not general logs: do not store credentials, hidden reasoning or unrestricted transcripts here.

The context projection preserves the goal, constraints, workspace, next action, all action identities/statuses and
unfinished or uncertain steps. It prioritizes constraints, then evidence-linked observations, then hypotheses.
Completed detail and omitted lessons stay retrievable in checkpoint history. It returns a byte budget, not a tokenizer
estimate; a model adapter must additionally respect its actual token budget. If mandatory state cannot fit, the call
fails explicitly rather than silently discarding the research objective or unresolved work.

The model must reconcile uncertain actions against task state before resubmitting. Durable action keys prevent a
second submission; worker lease fences still govern attempts and committed results. Neither mechanism claims
exactly-once effects in arbitrary external tools. Tool adapters need their own idempotency/reconciliation contract.

## Deciding checks

`scripts/qualify_remote_broker.sh projects` exercises the real PostgreSQL migration and restricted API role: private
access, collaborative reads, denied outsider access and cancellation, grant revocation, logged admin inspection,
concurrent checkpoint writes, reconnect/history recovery and cross-collaborator action deduplication.
`cargo test --locked --test broker_projects` checks context retention and refusal of invalid checkpoint structure.
`scripts/qualify_remote_broker.sh review` applies migration 014 twice and checks deferral retention, owner-only
member listing, audited administrator directory and inspection, early inspection end, the package snapshot and its
limits, and export recording against the restricted roles.
`scripts/qualify_remote_broker.sh person-review` checks migration 023's review records: who may record one, what it
binds, idempotency, immutability, readers and the package snapshot.
`scripts/qualify_remote_broker.sh lineage` applies migration 016 twice and checks origins for a person's project and
standalone tasks and a research run's task, the lineage's steps, inputs, method, code and gaps, the refusal of an
outsider, the API role's loss of the underlying submission functions, append-only origins, and the one-time fill of
earlier tasks.
Site activation and real researcher authentication need separate live evidence. Local PostgreSQL success is not
cluster deployment or a scientific validation claim.
