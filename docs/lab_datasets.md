# Lab datasets

A person who may add to a project can send it a table the lab keeps: a CSV or tab-separated text file or an Excel
workbook, however the instrument or the spreadsheet wrote it. The upload guard checks the file and rebuilds it as a
cell grid; the broker keeps the grid, profiles it, and shows the person where the table is, what each column holds and
what to decide before analysis. The person then approves a preparation: a typed specification of which rows and
columns to read and how. The broker's own executor applies it and writes the table that the template analyses and
figures read. No model writes, reads or changes a cell on the way.

## The method

The dataset method is a profile on route `server-dataset`. A lab needs exactly one enabled one granted to it
(`dataset_method_unavailable` otherwise). Like the figure method it runs only from a person's request
(`broker_request_only_route`, migration 059):

- The profile list marks it `offered: false` and `in_runs: false`.
- A person's direct submission of it is refused (`profile_forbidden`), in a project or not.
- A research run that lists it is refused at start (`broker_run_excluded_route`, migration 060).
- Its intake and preparation tasks hold none of a research run's places (migration 060): they are not analyses.

The profile's `image_ref` is the broker image by digest: the executor runs in the worker, so that image is what ran,
and the lineage names it.

## Sending a file

`POST /api/v1/projects/{project}/datasets?request_id=UUID` with the mutation intent header. The body is the file.
`X-Dataset-Name` is its name, percent-encoded UTF-8 as a file picker gives it, and `Content-Length` is required. The
extension chooses the type the guard is told:

| Extension | Declared type |
|---|---|
| `.csv` | `text/csv` |
| `.tsv`, `.tab`, `.txt`, `.dat` | `text/tab-separated-values` |
| `.xlsx` | `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet` |
| `.xlsm` | `application/vnd.ms-excel.sheet.macroenabled.12` |
| `.xls` | `application/vnd.ms-excel`: the guard blocks it (`type_not_allowed`) and keeps no copy; the decision is recorded and the person saves the file as .xlsx or CSV |

The guard reads a table by its content, so a text table under a workbook's name is still read as text.

The API checks, in order, before it reads the body: the intent header, the name (one path component of at most 255
characters with no control or direction characters), the extension, then in PostgreSQL that the caller may add to the
project, is a person, and that the lab has its dataset method (`broker_dataset_upload_check`). Then the guard must be
configured (for a new request ID), the length must be 1 byte to 32 MiB, and the upload holds its weight of the API's
upload memory (the library's 384 MiB budget, at least 1 MiB an upload) before the body is read.

A request ID already used in the project is a retry: the API reads the body and compares the name, type, size and
SHA-256 with the recorded dataset. The same file answers that dataset with HTTP 200 and `created: false` and is not
sent to the guard again; a dataset whose intake waits has it started first, and keeps waiting while the lab's queue is
still full. Another file, or the same file under another name, is HTTP 409 `idempotency_conflict`.

| Status | Code | When |
|---|---|---|
| 403 | `mutation_intent_required` | no intent header |
| 400 | `dataset_name_invalid` | no name, a path, a control or direction character, or over 255 characters |
| 415 | `dataset_type_unsupported` | an extension the list above does not name |
| 404 | `project_not_found` | the caller may not add to the project |
| 403 | `person_required` | the caller is a service account |
| 403 | `dataset_method_unavailable` | the lab has no single enabled dataset method |
| 503 | `dataset_guard_unconfigured` | the API has no guard caller |
| 411 | `dataset_length_required` | no `Content-Length` |
| 400 | `dataset_empty` | a length of 0 |
| 413 | `dataset_too_large` | over 32 MiB, stated or sent |
| 429 | `dataset_upload_busy` | no upload memory within 30 seconds |
| 400 | `dataset_incomplete` | the body ended early or took over 5 minutes |
| 503, 429, 413 | `dataset_guard_unavailable`, `dataset_guard_busy` or `dataset_upload_quota`, `dataset_too_large` | the guard gave no decision |

The API sends the file to the guard (use case `lab-dataset`) with the lab, the person's principal ID, the project and
the name, and keeps only the file's SHA-256, size and name. `broker_record_dataset` records the guard's decision with
the dataset whatever it is: `allowed`, `quarantined` or `blocked`, the findings that did not allow it, and the policy
version. The decision is recorded first; for an allowed file the same transaction records the grid's digest and size
and then submits the intake task. When the lab's queue is full the decision stays recorded and the intake waits:
`intake_task` is null and `intake_waiting` is true. A held or blocked file has no intake; the guard keeps it under its
own retention. The answer is HTTP 201 with `{dataset, created: true}`.

`POST /api/v1/projects/{project}/datasets/{dataset}/intake` with the mutation intent header starts a waiting intake
(`broker_start_dataset_intake`) and returns `{dataset}`; a started intake is returned as it is. A person who may add to
the project calls it: `person_required` for a service account, `dataset_not_allowed` (409) for a held or blocked file,
`group_queue_full` (429) while the queue is still full. The dataset record never changes otherwise: a trigger allows
only that one change, from no intake task to one, and a check keeps a task to a dataset with a grid.

The API's guard caller is `--dataset-guard-config` (`AGENT_SANDBOX_BROKER_DATASET_GUARD_CONFIG`): an owner-only JSON
file `{"schema": "enrichedlab.guard-caller.v1", "service_url", "token_file", "use_case": "lab-dataset", "tenant"}`.
The API refuses to start with a caller for any other use case, so a lab's file is never checked under another policy.

## The intake

The worker reads the guard's rebuilt copy with its own token: `--dataset-fetcher-config`
(`AGENT_SANDBOX_BROKER_DATASET_FETCHER_CONFIG`), an owner-only JSON file
`{"schema": "enrichedlab.guard-fetcher.v1", "service_url", "token_file"}`. The guard must name that token as a fetcher
for the API's caller and the use case `lab-dataset`. A worker without it fails an intake with
`dataset_guard_unconfigured`, and refuses to start with it but without migration 059 and its grants.

The intake fetches the grid (at most 96 MiB) and checks its size and digest against the guard's decision; a copy that
does not match, or a guard that cannot be reached, fails the attempt with `dataset_guard_unavailable`. The guard keeps
a rebuilt copy for a day under the current policy, so the intake must run within that time; the broker's package
keeps the grid from then on. The worker profiles the grid (`dataset_profile.rs`) and keeps a package:

- `source/grid.ndjson.gz`: the grid, `enrichedlab.cell-grid.v1`, as the guard wrote it;
- `profile.json` and `profile_detail.json`;
- `spec.json`: the package's record, with the grid's and the profile's digests;
- `README.md` and `SHA256SUMS`.

The receipt is `profiled`, with the profile when its stored form fits in 2 MiB.

## The profile

`enrichedlab.dataset-profile.v1` holds the source's type, digest, encoding and delimiter, and for each sheet its name,
size and hidden flag, where the table is, each column's name, unit and type, counts, and issues. Names, units and
types are guesses from the sheet's layout until a person approves a preparation. Names and units are the text of the
rows the layout takes as the header and units rows: when that guess is wrong, as for a table of text only whose first
full row of text is taken as the header, a name holds a data row's text. Each column says where its name came from
(`name_from`: `header` or `letter`) and its unit (`unit_from`: `units_row`, `header` or none), and the profile is read
by the project's people only (below); a model is given names only from an approved preparation. Missing values are
counted by token from a fixed list (`NA`, `N/A`, `-`, `.`, `NaN`, `null` and the rest in `MISSING_TOKENS`) and empty
cells.

The profile holds at most 4,194,304 row shapes over all sheets (a grid with more rows is refused with
`dataset_grid_too_large`) and reads names, units and kinds from the first 512 columns of the first 40 rows; a sheet
wider than that counts the rest in `columns_omitted`.

Where the table is: one header row, or two when a heading spans the columns under it (`Glucose (mg/dL)` over `0`,
`15`, `30`); the basis (`labels_over_numbers` when header text sits over a column of numbers, `first_text_row` for a
table of text only); a units row; title rows above; the first and last data rows; blank rows; summary rows such as
Mean or SD; and notes below the data.

Each column has its letter, its name without the unit, a `suggested_name` that R takes as written (lower case,
letters, digits and underscores, unique in the sheet), its unit, its type, the date order for dates written as text
(`day_first`, `month_first` or `ambiguous`), and counts: present values, missing by token, censored values
(`below_limit`, `above_limit`, `not_detected`), values not of the column's type, and distinct texts up to 2,000. The
type is the one at least four in five present cells read as, else `mixed`; a date may be one by its form alone.

Issue codes, each with the columns and a count:

| Kind | Codes |
|---|---|
| Columns | `missing_tokens_mixed`, `censored_values`, `decimal_commas`, `ambiguous_commas`, `thousands_separators`, `percent_signs`, `numbers_as_text`, `date_orders_mixed`, `date_order_ambiguous`, `text_in_numbers`, `error_cells`, `category_spellings`, `duplicate_ids`, `ids_not_all_checked`, `mixed_types`, `blank_name` |
| Sheets | `duplicate_names`, `wide_time_points`, `no_header`, `header_not_first`, `two_header_rows`, `title_rows`, `units_row`, `blank_rows`, `summary_rows`, `notes_below_data`, `units_in_names`, `hidden_sheet`, `formulas_without_values`, `merged_cells` |

`profile_detail.json` is for the people of the project and stays in the package: each sheet's first 30 rows and 30
columns as text, the title and note text, each text column's most frequent values, each number column's lowest and
highest, and a few cells for each issue.

## A preparation

`POST /api/v1/projects/{project}/datasets/{dataset}/preparations` with the mutation intent header and
`{spec, title, previous, request_id}` returns HTTP 202 with `{task_id}`. The API checks the specification as written
first and answers HTTP 400 `dataset_preparation_invalid` with `path` and `reason`: where the specification is wrong,
such as `columns[1].type`, and a fixed word, such as `missing_field`, `unknown_field`, `unknown_choice`, `wrong_type`,
`not_a_name` or `rows_out_of_order`. A place holds only the specification's own field names and indexes: it ends at a
recode map, whatever its key spells (a key is a cell's text and may spell a field name), stops before any other name
the sender wrote, and places an unknown field at its parent. A long layout's text label or a recode's target that is
blank or exactly `NA` is refused (`value_reads_as_missing`): it would be written as a text cell that R, the result
digest and the table reader take for a missing value.
`broker_request_dataset_preparation` then checks, in one transaction with the task it creates:

| Refusal | When |
|---|---|
| `project_not_found` | the caller may not add to the project |
| `person_required` | the caller is a service account |
| `dataset_not_found` | the dataset is not in the project |
| `dataset_not_profiled` | its intake has not completed with a profile, or it has no intake |
| `dataset_preparation_invalid` | not an object of the preparation schema, over 256 KiB, or a title over 200 characters |
| `dataset_previous_invalid` | the preparation it changes is not one of the same dataset |
| `dataset_method_unavailable` | the lab has no single enabled dataset method |

The same `request_id` with the same dataset, specification, title and previous preparation returns the same task;
with another it is `idempotency_conflict`. The specification is recorded in `broker_dataset_requests` as PostgreSQL
stores it, and its identity is the SHA-256 of that stored text.

## The specification

`enrichedlab.dataset-preparation.v1`, with unknown fields refused:

| Field | Meaning |
|---|---|
| `table_name` | the table's file name without `.csv`: lower case, starting with a letter, at most 63 characters |
| `sheet` | the sheet's index, 0 for a text file |
| `header_rows`, `units_row` | where the names and units were read from, for the record |
| `rows` | `{first, last}`: the data rows, as a spreadsheet numbers them |
| `drop_rows` | inclusive `[first, last]` ranges inside `rows` to leave out; blank rows are always left out |
| `missing` | texts read as missing in every column; an empty cell is always missing |
| `decimal`, `thousands` | how numbers are written: `point` or `comma`; `none`, `comma`, `point` or `space` |
| `date_order` | `day_first` or `month_first`, for dates written as text |
| `trim` | drop leading and trailing spaces from text (default true), each change counted |
| `columns` | each output column: `from` (the letter), `name`, `type` (`text`, `category`, `number`, `integer`, `logical`, `date`, `datetime`, `time`), `unit`, `decimal` and `thousands` for this column, `recode` (text to text), `censored`, `strip_percent`, `true_values` and `false_values`, and `other` (`refuse` or `missing`) |
| `long` | columns that hold one measure at several points: `{columns, name, values, value}` reads them into a point column and a value column, one row per point |
| `key` | output columns whose values together must not repeat |

A censored column (`censored: {rule, limit, upper_limit}`) reads values such as `<0.5`, `>200` and `ND`. The column
itself follows the rule: `missing`, `limit`, `half_limit` or `limit_over_sqrt2`, the last two for values below the
limit. Beside it, `<name>_censored` says `below`, `above` or `not_detected`, and `<name>_limit` gives the limit, so an
analysis can treat censored values properly instead of as numbers.

## Running a preparation

The worker reads the grid from the intake's package, after checking the package's size and digest and the grid's
digest against the dataset's, and checks the specification's stored text against its recorded digest
(`dataset_spec_changed` otherwise). The executor (`dataset_prepare.rs`, `enrichedlab.dataset-prepare` version 2)
applies it. No value changes except by a rule the specification names, and every change is counted. A cell no rule
reads stops the preparation (`dataset_values_unread`) unless its column says `other: missing`; then it is written as
missing and listed. A text or category value that would be written as exactly `NA` is a cell no rule reads
(`text_reads_as_missing`), since R reads `NA` as missing: the person lists it as a missing token or recodes it. An
integer column keeps integers: its censored limits must be whole numbers (`not_an_integer` at
`columns[i].censored.limit` when the specification is checked), and a value written for a censored cell is checked as
the column's type like any other.

| Refusal | When |
|---|---|
| `dataset_sheet_missing` | the grid has no such sheet |
| `dataset_spec_outside_sheet` | a row or column the specification names is not in the sheet, `rows.last` included |
| `dataset_values_unread` | cells no rule reads; the receipt names the first 20 with row, column, reason and text |
| `dataset_key_repeated` | the key repeats; the receipt names each repeat's row and its first row |
| `dataset_no_rows` | no row is left |
| `dataset_table_too_large` | over 1,000,000 rows, 2,000,000 cells, 1,000 columns or 16 MiB |
| `dataset_grid_unreadable`, `dataset_grid_too_large` | the kept grid cannot be read; a sheet ends before or runs past the rows its listing names, a row is wider than its sheet's columns, or a listed sheet is missing |

A prepared table keeps a package:

- `results/lab_data/<table_name>.csv`: UTF-8, one header row, `NA` for missing values, fields quoted only when they
  need it. R reads it with `read.csv`.
- `exports/<table_name>.spreadsheet.csv`: the same table for a spreadsheet program, with a byte-order mark and `'`
  before any text that starts with `=`, `+`, `-`, `@`, a tab or a carriage return.
- `preparation.json`: the specification as stored; its SHA-256 is the one the request recorded.
- `receipt.json`: the executor's receipt.
- `spec.json`, `README.md` and `SHA256SUMS`.

The receipt (`enrichedlab.dataset-preparation-receipt.v1`) counts rows read, blank, dropped, kept and written, and
accounts for every filled cell of the sheet: `cells.filled` is the sum of `outside_rows` (titles, header and units
rows, notes), `in_dropped_rows`, `in_unread_columns` and `read`. For each column it counts its values, missing values
by token, trims (for every type, when spaces around a value were read past), recodes (the first 200 pairs listed, the
rest in `recoded_unlisted`), decimal commas, thousands separators, percent signs, leading plus signs and censored values
with the rule used; and it gives the table's and the spreadsheet copy's digest and size. The broker stores it as the
receipt's summary within 1 MiB, leaving out the recode lists when it would not fit (`recodes_left_out`); their counts
move into `recoded_unlisted`, so each column's total of recoded cells stays. A refused preparation keeps its receipt too, so the person
sees which cells to decide about. A failure that may pass on another attempt, such as a lost lease, is `failed`.

## What the model sees

Row values are not sent to the model. A prepared table is a result table like any other, but the result
digest lists a table under `results/lab_data/` with its columns, rows and missing counts and
`cells_omitted: lab_data`, never its cells. The research lead is told it is the lab's own data and to run a method on
it. A person may run a template analysis or draw a figure from it; the R render service reads the table, not the
model. A template analysis of such a table (or of one under `results/lab_derived/`) writes its results table to
`results/lab_derived/results.csv`, since its rows may be one per animal, and the digest lists it the same way
(`cells_omitted: lab_derived`). Its headline statistics stay in `results/statistics.csv`, which the lead reads: they
are the method's results, which the model may discuss. A figure's points stay in its PNG and SVG, which the
lead is not shown.

The cells are also withheld when a digest is read, so a digest stored before lab tables were classified, or one of a
table whose path names no lab, never brings them to a model. When a reader that is not a person reads a digest (the
research lead's results and `broker_get_artifact_digest`), a table under either lab path lists its columns, rows and
missing counts with `cells_omitted` (`lab_data` for a table under `results/lab_data/`, else `lab_derived`) whatever task
wrote it, and for a task that holds the lab's data so does every other table but `results/statistics.csv`; a person
reads the digest as stored. A task holds the lab's data when it, or any task it was made from, is a dataset intake or
preparation, an analysis or figure of a table under either lab path, a task whose digest lists a table under one, or a
task whose attempt was given a lab object or an earlier run's output (input classes `object_data` and `prior_output`;
an output volume names its run, not the task that wrote it, so it counts as the lab's whatever it holds). A task is
made from the source task of its analysis or figure request and from its project parent, and the walk keeps each task
once, so it ends on any chain or cycle and has no depth limit: an analysis of an analysis's `results/statistics.csv`
holds the data too, however many steps later. A lab database's query result is not the lab's data in this sense:
the research lead may read it within the reader login's column grants. Only the project's people read the
result files of a task that holds the lab's data (`broker_get_artifact`). The coordinator also shows no cells for a
table under a lab path, whatever its digest holds.

## Records and readers

`broker_datasets`, `broker_dataset_requests` and `broker_dataset_receipts` have row-level security and are immutable.
One receipt is recorded per attempt and lease, while the lease holds, including while a cancellation the worker is
answering waits for it; a repeat must match. Its status is `profiled`, `prepared`, `refused` or `failed`.

- `GET /api/v1/projects/{project}/datasets`: the project's datasets, newest first, each with the guard's decision, its
  intake's status and newest receipt (with the number of sheets profiled), and its preparations with their newest
  receipts.
- `GET /api/v1/projects/{project}/datasets/{dataset}`: one dataset with every intake receipt, the profile included,
  and each preparation with its specification, digest and receipts.

Both need the right to read the project (`project_not_found` otherwise). The full dataset holds the lab's cells
(profile names, recodes, unread cells), so it is for the project's people: a service account that may read the project
gets `person_required`. For the same reason the files of a dataset task, and of an analysis or figure of a table under
`results/lab_data/` or `results/lab_derived/`, are read by people only (`broker_task_holds_lab_data`); a service
account reads such a task's result digest, which lists the table's columns and counts without cells. The project's
contents name each dataset
task's operation, dataset and the preparation it changes (`dataset` beside `figure` and `template_analysis`). Dataset
tasks ready the lab's data for analyses and are not analyses themselves: like figure versions, they do not wait in the
inbox for review and do not count in the project list's analysis counts (migration 059 redefines `broker_inbox` and
`broker_list_projects` for this). The analyses made from a prepared table are reviewed as usual.

## Lineage

A dataset task's lineage has a person's request as its basis, tool class `vetted_service` and output class
`vetted_tool_output`. An intake read `object_data` (the person's file as the guard rebuilt it) and a preparation
`prior_output` (the intake's kept grid), recorded by `dataset_request`. The `dataset` record gives the dataset, the
operation, the file's name and SHA-256, the guard's intake and policy, the grid's digest, the specification's digest,
the preparation it changes, the request's digest, the executor and version, and the prepared table's digest. A template
analysis of the prepared table names the table's path and digest, so a result traces back to the file a person sent.

## Qualification

`sh scripts/qualify_remote_broker.sh lab-datasets` migrates twice and runs `tests/broker_lab_datasets.rs` and the API
test in `src/broker/api/datasets.rs` against an isolated PostgreSQL, a stand-in guard and a stand-in render service,
with a synthetic glucose tolerance sheet: readiness by role, the method offered only through a request, the guard's
decision recorded for every outcome, the upload route's refusals before the guard sees a file, the intake and its
package, a copy that does not match its digest, a preparation and its package, a refused preparation that names its
rows, the lease fence, readers, lineage, the project's contents, the result digest without the table's cells, a
template analysis request on the prepared table, the inbox and project counts without dataset tasks, and
immutability. 18 mutations of the migration, the worker, the API and the digest each fail it or a unit test.

## Release

A release needs migration 059 and `grant_broker_runtime.psql`, the guard's `lab-dataset` use case with the worker's
token named as a fetcher for the API's caller, a network path from the worker to the guard, both configuration files
and their tokens, and the dataset profile with the broker image by digest, granted to each lab that uses it.
