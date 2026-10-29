# Result digests

A result digest (migration 021) is a bounded, deterministic excerpt of one retained result package. The
research lead reads digests to know what an analysis found; the workspace fills the values of the lead's findings from
them. A digest validates nothing and is never a scientist's approval.

## Who writes it

The direct worker writes it in the same lease-fenced sequence that registers a task's artifacts
(`sandbox::SandboxRuntime::finish`), for each researcher-visible artifact with role `result`, from the durable
artifact root its retainer wrote to. `digest::build` resolves the path inside `tasks/<task>/`, verifies the file's size
and SHA-256 through the package listing, reads the selected members once more while hashing them, and returns a
digest whose `status` names the outcome. `broker_record_artifact_digest` stores it once per artifact and SHA-256 for
the worker's current lease; the same digest again is accepted and a different one is refused. Rows are immutable.
A digest that cannot be built or recorded never fails the task: the worker logs `result_digest_not_recorded` with a
fixed code, and the artifact has no digest.

## Packages registered before digests

Packages registered before the worker built digests get them from the operator (migration 035).
`agent-sandbox-broker digest-packages` (the root from `--artifact-root` or, by default, the broker's configured
`AGENT_SANDBOX_BROKER_ARTIFACT_ROOT`, which is not the volume's mount path) reads one JSON line per package on standard input
(`task_id`, `relative_path`, `size_bytes`, `sha256`, at most 100) and writes one line per package (`task_id`,
`relative_path`, `artifact_sha256`, `status`, `digest`) with the worker's own `digest::build`; it only reads under the
artifact root, so it can run in any container that mounts the root read-only, and it opens no database or network
connection. `broker_backfill_artifact_digest(tenant, task, path, sha256, status, digest)` records each line for a task
that has ended, with the task's attempt and lease generation and `source` `operator_backfill` (the worker's rows say
`worker`); the same digest again is accepted, another for the same bytes is refused, and no runtime role may call it.
Readers join digests by artifact and bytes, so a backfilled digest reads like any other.

## Contents (`enrichedlab.result-digest.v1`)

| Field | Content |
|---|---|
| `status` | `summarized`, `no_result_tables`, `not_a_package`, `integrity_failed`, `too_large` or `unreadable` |
| `package` | member count, whether a checksum list is present, whether every member matched it |
| `tables` | each `results/**.csv` member whose checksum matched (at most 12): path, SHA-256, bytes, rows, column names, `missing` (for each listed column some row leaves empty, writes as exactly `NA` or lacks because the row is shorter than the header, how many rows over the whole table, also when the cells are withheld; counted only for the listed columns; left out when column names repeat), and `cells` as written when the table has at most 200 rows (50 at first; a lead call is shown the first 50), 32 columns (16 until a test run: the reviewed FoodData Central normalization writes 18 or 19) and 8 KiB; otherwise `cells_omitted` names why (`too_large`, `too_many_rows`, `long_cells`, `duplicate_columns`, `irregular_rows`, `unreadable`, `digest_budget`; a lead call may also withhold a template analysis's table cells as `context_budget` when its whole digest does not fit the context, without changing the digest); a table under `results/lab_data/`, a lab's prepared dataset, is always `lab_data`, and one under `results/lab_derived/`, an analysis's rows computed from it, always `lab_derived` (row values are not sent to the model; [lab datasets](lab_datasets.md)). Each table also has `summary` (the [step checks](research_coordinator.md#step-checks)): for each listed column over every row, whether every value is a finite decimal number (`numeric`; `Inf`, `NaN` and hexadecimal are not), the smallest and largest number as written (`min`, `max`, never for a table under a lab folder), and the count of distinct values (`distinct`, or `distinct_capped` past 20,000); left out when column names repeat. A digest is also withheld when read: the research lead and a service account read a table under either lab folder without cells whatever task wrote it, and for a task made from the lab's data every table but `results/statistics.csv`, whatever the stored digest holds; since migration 073 such a table's summary also loses `min` and `max` and keeps its counts, including a table whose cells were already left out for size, and a reader that is not a person gets no project package of a project any of whose tasks holds the lab's data, since a package carries each result's files and stored digest whole |
| `figures` | path and SHA-256 of each `results/**` figure (at most 12) |
| `parameters` | `run_parameters.json` when it is a JSON object of at most 2 KiB with no NUL character (PostgreSQL's jsonb cannot store one) |

Members under `input/` are never read. Cells are strings of at most 160 characters with control characters replaced
(64 at first: 2,451 of 7,793 SR Legacy food names are longer, the longest 134, so nearly every list of
twenty foods withheld its cells and the research lead could not state findings from it);
an empty column name becomes `column_<n>`. A digest is at most 16 KiB as PostgreSQL stores it (`digest::text`, with a
space after each `:` and `,` and numbers printed without an exponent), the measure migration 021 checks: the largest
inline cells are dropped first, then the parameters, then the largest column summaries (marked `summary_omitted`), then
the largest `missing` counts (marked `missing_omitted`), then trailing tables. The stored SHA-256 is computed by PostgreSQL over the stored form, so any reader can check it.

## Readers

- The coordinator reads the digests of its run's planned analyses (at most four result packages per analysis) and the
  project's newest findings through `broker_research_run_results`, only while it holds the run's lease, and checks each
  against its step's expectation from the summaries (step checks); the lead is shown the outcome, not the summaries.
- The API serves `GET /api/v1/tasks/{task}/artifacts/{artifact}/digest` to anyone who may read the artifact; a readable
  package without a digest answers `status: not_recorded`.
- The workspace shows the lead's answer and findings on the project report. Each value is the cell its placeholder
  names, read from the stored digest, shown to at most four significant digits (whole numbers as written), with the exact text and its file,
  row and column in the element's title. A value that cannot be read is shown as missing, never guessed.

## Checks

`cargo test --locked digest` covers the live package layout, a member that disagrees with its checksum list, hostile
cells, unreadable packages, the size bound and the backfill's input and output lines.
`scripts/qualify_remote_broker.sh result-digests` covers the database boundary, the backfill included, and checks that
the worker's measure of a digest near the bound equals PostgreSQL's `octet_length(digest::text)`. The summary tests
(`each_column_is_summarized_over_every_row`, `distinct_values_are_counted_up_to_their_limit`,
`a_number_is_a_finite_decimal`) cover rows past the inline limit, a lab table and the distinct cap;
`scripts/qualify_remote_broker.sh lab-datasets` checks that migration 073 withholds a lab's smallest and largest values
from a reader that is not a person, and fails with the migration's rule set back to migration 059's.
