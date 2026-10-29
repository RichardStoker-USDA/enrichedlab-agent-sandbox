# Lab memory: shared notes

The research lead saves notes (observations with evidence, and hypotheses) in a project's saved versions, and its next
decision in that project reads them. Lab memory lets a lab carry a checked observation into its other projects: a
person who is a lead or reviewer of the lab shares the note, and the lead reads it, as background, in the lab's other
projects that start from the same data or use the same methods.

A shared note is prior knowledge, not a result. No finding may rest on one; findings still bind to result cells of the
project the lead is working in. Sharing validates nothing. It records that a named person judged the note worth
reusing after a person accepted every analysis behind it.

## Records

Migration 027 adds `broker_lab_notes` and `broker_lab_note_events`. Both are immutable (the shared mutation trigger
refuses update and delete). A note holds the lab, the origin project and saved version, the statement and its SHA-256,
`evidence` (one entry per analysis: its references, plan-step title, method, result files as
`{artifact_id, relative_path, sha256}` and the id of the person's acceptance it rests on), `methods` (the evidence
analyses' methods), `data` (the starting data of the origin project at that version), who shared it, an optional reason
(at most 600 characters) and a review-by date. Retiring and renewing are appended events; the origin project's access
history records `note_shared`, `note_retired` and `note_renewed`.

`broker_share_lab_note(tenant, actor, project, revision, statement, evidence_refs, reason, review_days, request)`:

- The actor must read the project (`project_not_found`), be a person (`note_person_only`) and be a `lead` or
  `reviewer` of the project's lab (`note_forbidden`).
- The project must be lab-visible, so sharing discloses nothing a lab member could not already read
  (`note_project_private`).
- `revision` must be the project's current saved version (`note_stale`), and it must hold an observation with exactly
  this statement and these references (`note_invalid`). References are the project's result files or the data it
  started from, with at least one result file. `review_days` is 30 to 730, default 180.
- For every analysis the note rests on, the newest review by a person must be an acceptance that covers its current
  result files (`note_evidence_unaccepted`). A service account's review never counts.
- A note from the same project with the same statement that is not retired refuses a second share
  (`note_already_shared`). The same request id returns the same note; the same id with other content is
  `idempotency_conflict`.

`broker_retire_lab_note` needs a reason and a person who shared the note or is a lab lead or reviewer.
`broker_renew_lab_note` needs a lab lead or reviewer and sets the review-by date 30 to 730 days from now. Neither acts
on a retired note (`note_retired`).

## State

`broker_lab_note_state` decides a note's state each time it is read, in this order:

1. `retired`, with the retiring person's reason.
2. `expired`, once the newest review-by date (the share's, or the newest renewal's) has passed.
3. `paused`, with `cause`: `evidence_files_changed` when an analysis's current result files differ from the ones the
   note bound, or `evidence_review_changed` when an analysis's newest person's review is not an acceptance. The first
   analysis, in the note's order, that fails decides the cause.
4. `active`.

Nothing rewrites the record: a new acceptance of the same files makes a paused note active again.

## What the lead reads

`broker_research_run_lab_notes` is lease-fenced like the results read. It returns the lab's active notes that are not
from the run's own project and that the run's actor may read, where the note's starting data matches the project's
current starting data or its methods overlap the run's allowed methods. Each carries its statement and SHA-256, its
methods and those that match, whether the data matches, and when it was shared as a fixed-width UTC time. No sharer,
reviewer or reason reaches the model.

The coordinator reads a lab's notes only when it names that lab in `--lead-text-labs`. Its context
(version 5 and later; see [research coordinator](research_coordinator.md)) orders the candidates itself (matching
data, then more matching methods, then newest shared, then identifier) and takes them until the next would pass 8
notes or 4,096 bytes. The rest are counted in `lab_notes_omitted`. A note whose statement reads like instructions to an
agent is withheld by the fixed screen (`untrusted-text-screen/v1`) and counted in `lab_notes_withheld`; the turn records
its id, statement SHA-256 and the rule that held it, and the note stays shared in the lab. Each note shows its statement, the date it
was shared, whether it has the same starting data and which of the run's methods it shares.

## API

| Method | Route | Result |
|---|---|---|
| GET | `/api/v1/labs/{lab}/notes` | `{lab, can_curate, notes}` for a member of the lab; `can_curate` is true for a person who is a lead or reviewer |
| POST | `/api/v1/projects/{project}/notes/share` | `{revision, statement, evidence_refs, reason?, review_days?, request_id}`; HTTP 201 with the note |
| POST | `/api/v1/notes/{note}/retire` | `{reason, request_id}`; the note as it now stands |
| POST | `/api/v1/notes/{note}/renew` | `{review_days, request_id}`; the note as it now stands |

The API and the coordinator refuse to start without migration 027 and their grants.

## Workspace

"What the agent noted" offers "Share with the lab" on an observation to a lab lead or reviewer when the project is
lab-visible and a person accepted every analysis it rests on; otherwise it names the analyses still to accept. A
shared note says so with its state. "Shared notes" lists the lab's notes in use, those not in use and, folded, the
retired ones, with who shared each, its project, the analyses it rests on and the actions the reader may take. The
project page lists the shared notes the agent read at its latest decision, from the turn's `context_inputs`.

## Checks

`scripts/qualify_remote_broker.sh lab-notes` runs the PostgreSQL suite: every refusal above, the binding of files and
acceptances, idempotency, the state rule through a request for changes, a later acceptance, a past review-by date, a
renewal and changed files, the readers, the coordinator's read and its lease fence, immutability and the audit
events. `cargo test --locked --test broker_research_runs` checks the order, the limits and that the same records in
any order give the same context bytes.
