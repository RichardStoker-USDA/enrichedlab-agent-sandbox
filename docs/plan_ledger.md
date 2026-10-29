# Plan ledger

A project's plan lives inside each saved checkpoint, and checkpoints are immutable, but the checkpoint
alone does not say what changed a step or when. The plan ledger records that as typed rows.

Migration 036 adds two append-only tables. `broker_plan_revisions` has one row per checkpoint revision: what wrote it,
its run and turn when the research coordinator wrote it, the principal for a direct checkpoint, the number of step
changes and the checkpoint's time. `broker_plan_events` has one row per plan step that revision changed: the step's
action key and number in the plan, what changed (`planned`, `removed`, `text`, `task`, `status` or `results`), the
status before and after, the analysis id, the number of result references, and the step's title and reason when it
was planned or either changed.

The writer names itself. The four checkpoint writers call one internal function, `broker_write_checkpoint`, which
saves the checkpoint and records its changes in the same transaction:

| Writer | Label | Also recorded |
|---|---|---|
| `broker_commit_research_turn`, the lead's turn | `lead_turn` | run and turn |
| `broker_reconcile_research_run`, statuses from analysis records | `analysis_record` | run; for a step that reached a finished status, the analysis's status and error code |
| `broker_archive_research_plan`, finished steps moved into an archive page (migration 048) | `plan_archive` | run; each moved step as `removed` |
| `broker_checkpoint_project`, a direct checkpoint through the API | `person` or `service_account`, from the author's principal class | the author |

The writers keep their signatures and grants. The recorder, the comparison and the replay have no grant, so no caller
chooses a label or writes a row. An analysis's status and error code are kept only when reconciliation moved a step to
`completed`, `failed` or `uncertain` from a finished analysis, because a finished analysis cannot change afterwards.

One pure function, `broker_plan_changes`, compares two plans by action key. The live recorder and the replay both use
it, so the stored rows always equal a fresh comparison of the checkpoints. The migration replays every checkpoint that
existed before it, oldest first. A revision a recorded turn produced is labeled `lead_turn` with that run and turn;
every other earlier revision is `recorded_before_ledger`, because reconciliation and a direct checkpoint left no mark
that tells them apart. Applying the migration again records nothing new. A checkpoint written by an older function
body while the migration commits has no rows until the next write of that project, which records it first with the
same label.

A project reader reads `GET /api/v1/projects/{project}/plan-ledger` (`broker_project_plan_ledger`): the newest 500
revisions, oldest first, each with its step rows, `unrecorded_revisions` (checkpoints without a ledger row) and
`truncated`. Anyone else gets `project_not_found`. The API stays out of service until migration 036 and that grant
exist. The research coordinator's context is unchanged: it already carries each step's current analysis record, error
code and review.

The project page lists under each planned step what happened to it, dated and in plain words: "The agent planned it
and started its analysis", "Its analysis finished", "Its results need a check: its compute worker stopped
responding", "You revised its reason". A revised step keeps its earlier reasons behind a disclosure, and steps taken
out of the plan are listed with who took them out (`clients/workspace/src/plan_ledger.ts`).

A step the archive writer moved is not listed with the steps taken out of the plan; the project page counts those steps
in one sentence instead, since a long project moves many (`archivedStepCount`).

PostgreSQL checks: `scripts/qualify_remote_broker.sh plan-ledger` and, for the archive writer, `plan-pages`.
