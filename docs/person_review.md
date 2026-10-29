# A person's review of an analysis

A person with a project's `review` capability records a decision on one of its analyses: accepted, needs changes or
rejected, with a note. The review names the reviewer from the signed-in account and fixes the result files it covered
by their SHA-256, so a later reader knows who looked at which bytes.

A review is a person's reading of the results and code. It is not statistical validation, it never changes a task, a
result or its lineage. The research lead reads the newest person's review of each analysis: a rejected
analysis's results are withheld from it and can support no finding or lesson.

## Records

Migration 023 adds `broker_task_reviews`. Each row holds the task, its attempt and lease generation at the time, the
reviewer's principal, the client's request id, the decision, the note and `outputs`: every researcher-visible artifact
of the task as `{artifact_id, relative_path, sha256}`. Rows are immutable (the shared mutation trigger refuses update
and delete); a later review is a new row and the newest is current.

`broker_record_task_review(tenant, actor, task, decision, note, request)`:

- The caller must read the task, and the task must belong to a project where the caller holds `review`. A task outside
  a project has no reviewer (`review_forbidden`).
- Only a person's account records a review (migration 024). A service account can hold `review`, for example as the
  owner of a project it created, but its decision would not be a person's reading (`review_person_only`). Since
  migration 030 a person is a principal whose recorded class is `person`, named by the authenticator that verified it;
  an identity from any other authenticator is not counted as a person. The live
  canary's service account recorded two reviews under migration 023; they stay, since rows are immutable, and read as
  a service account's.
- The note is trimmed of spaces, tabs and line breaks. `needs_changes` and `rejected` need one; any note is at most
  2,000 characters (`review_invalid`).
- The analysis must have finished: completed, failed, cancelled or stopped for review (`review_not_finished`).
  Acceptance also needs a completed or stopped-for-review analysis with at least one result file
  (`review_nothing_to_accept`).
- The same reviewer and request id returns the stored review; a different decision, note or task under that id is
  `idempotency_conflict`.

Every reader of the task reads its reviews, newest first. A reader of the project reads the newest review of each
analysis. Each review carries the reviewer's display name, never an email, `reviewer_kind` (`person`, or
`service_account` for a row recorded before migration 024) and `covers_current_outputs`, which is false once the task's
result files differ from the ones the review covered.

## API

| Method | Path | Use |
|---|---|---|
| GET | `/api/v1/tasks/{task}/reviews` | `{"reviews": […]}`, newest first, for any reader of the task |
| POST | `/api/v1/tasks/{task}/reviews` | `{"decision", "note", "request_id"}`; returns HTTP 201 with `{"review": …}` |
| GET | `/api/v1/projects/{project}/reviews` | `{"reviews": […]}`, the newest review of each analysis in the project |

The POST needs `X-Agent-Sandbox-Intent: mutate`. Errors map to 403 (`review_forbidden`, `review_person_only`), 400
(`review_invalid`) and 409 (`review_not_finished`, `review_nothing_to_accept`, `idempotency_conflict`). The API stays out
of service until migrations 023 and 024 and the grants on the record, read and package functions exist.

The project evidence package reads `broker_project_review_records` in its repeatable-read snapshot and writes
`tasks/<analysis>/reviews.json`, oldest first; the manifest counts `person_reviews` and the README names them.

## Workspace

The analysis page has a Review section: what a review claims, the reviews newest first with each reviewer, note and
what it covered, and, for a reviewer of a finished analysis, a form with the three decisions. Acceptance is disabled
until the analysis has result files. A send carries one request id per decision and note, so a repeated click saves
once. The newest review also appears beside the analysis's status, in its "Where this came from" record, in the
project report's checks and results, and in the detailed view. The lab index says for each project how many finished analyses a person has
reviewed (migration 026 adds `reviewed`, `rejected` and `changes` to the project list's `task_counts`). A service account's row is named as one, takes the quiet
mark instead of a decision's, says it is not a person's review, and never counts an analysis as reviewed.

## Checks

`scripts/qualify_remote_broker.sh person-review` applies the migrations twice and checks the capability rule, the
people-only rule and each review's reviewer kind, the finished-analysis and output binding, the note rules, idempotency, immutability, newest-first reads, outsider
refusals, the package snapshot and `covers_current_outputs` after a result file changes. The workspace's browser
regression records a request for changes and an acceptance and reads them on the analysis page and the project report
at desktop and phone widths in both themes.
