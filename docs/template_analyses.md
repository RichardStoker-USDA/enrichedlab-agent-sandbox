# Template analyses

A person who may add to a project can run one of the render service's analysis templates on one result table of a
finished analysis in it: a two-group comparison, a correlation, a linear model, an ANOVA with estimated marginal means,
a PERMANOVA, a batch screen of many features, a power calculation and the rest the service declares. Each template is
reviewed R code that no model wrote. Each run is its own task, a template analysis, with the request, the service's
receipt and a package that reruns it. Nothing is overwritten. The research lead can choose one as a step of a research
run (migration 060, "The lead's choice" below). This page is the maintainer's reference.

A template analysis is an analysis, not a drawing: it counts in the project's totals, waits for a person's review like
any analysis, and its results table can be the source of a figure version or of another template analysis.

## The method

The template-analysis method is a profile on route `server-analysis`. A lab needs exactly one enabled one granted to it
(`analysis_method_unavailable` otherwise). It runs only from a request a person or the research lead makes:

- The profile list (`broker_get_profile_choices`, migration 058) marks it `offered: false`.
- A person's direct submission of it is refused (`profile_forbidden`), in a project or not.
- Since migration 060 a research run may list it; the figure and lab-dataset methods stay refused at a run's start
  (`profile_forbidden`).

The worker that has `--render-url` serves the route, and refuses to start without migration 058 and its grants
(`broker_schema_not_ready`). It reads the service's catalog at start and again with each template analysis.

## The catalog

The worker keeps the service's analysis features (`kind: analysis` in `GET /features`), with their settings, in
`broker_analysis_catalogs`, apart from the figure catalog: a figure request cannot name an analysis, nor an analysis
request a figure. The catalog is checked as the figure catalog is (migration 057's setting types, `colors` and `maxlen`
included) and refused whole when any setting is one the broker cannot check. `GET /api/v1/template-analyses/catalog`
returns the newest one with `groups`: the reader's labs whose template-analysis method is enabled. A catalog is named by
the SHA-256 of its features with the service version and image that declared them, so a new release with the same
features is a new catalog and the reader sees its version.

## A request

`POST /api/v1/projects/{project}/template-analyses` with the mutation intent header and
`{source_task, table_path, feature, settings, title, previous, request_id}` returns HTTP 202 with `{task_id}`.
`broker_request_analysis` checks, in one transaction with the task it creates:

| Refusal | When |
|---|---|
| `project_not_found` | the caller may not add to the project |
| `task_not_found` | the source is not an analysis of the project |
| `analysis_source_not_ready` | the source has not completed |
| `analysis_table_unknown` | the source's newest summarized result digest does not list the table |
| `analysis_table_unsuitable` | the table has no rows, over 50,000 rows, over 64 columns, or columns the digest left out |
| `analysis_method_unavailable` | the lab has no single enabled template-analysis method |
| `analysis_catalog_unavailable` | the worker has not read a catalog |
| `analysis_feature_unknown` | the template is not in the newest analysis catalog |
| `analysis_settings_invalid` | a setting the template does not declare, a value of the wrong kind, a column the table does not have, a required column left out, an enum value it does not list, over 40 settings or 8 KiB, or a title over 200 characters |
| `analysis_previous_invalid` | the analysis it changes is not a template analysis of the same table of the same analysis |

The same `request_id` with the same request returns the same task; with another request it is `idempotency_conflict`.
A repeat is judged, under the project's lock and the caller's current right to add to it, on what the caller sent
(source, table, feature, settings, title and the analysis it changes), not on the catalog: a retry after the worker
read a newer catalog, even one without the template, returns the task with the catalog and source it pinned. Only a
new request is checked against the newest catalog. The request is recorded immutably in `broker_analysis_requests`
with the source package's digest and the table's digest, rows and columns.

## The lead's choice

Migration 060 lets the research lead of a run that lists the method choose a template analysis as one of its steps.

- **What the lead sees.** Each turn the coordinator reads the run's method and the newest catalog under the run's lease
  (`broker_research_run_analysis_catalog`). The method is null unless the lab has exactly one enabled template-analysis
  method and the run lists it; the catalog is null until a worker has read one. The lead sees an analysis's whole input
  contract under `analyses`: its id, label, first sentence and settings, required ones first, each with its type,
  choices or range, default, and `about`, the catalog's description of what the setting holds (its unit, or the measure
  it names). The contracts (`src/broker/analysis_view.rs`) come in the order the run needs them: an
  analysis the previous reply named without having seen its contract first, then by how strongly the question names
  each analysis, then the constraints and the plan's step titles (the word weights the query sources use for their
  relations), then the catalog's order. Contracts take at least two thirds of the view; every other analysis is listed
  by id and label under `other_analyses` with a note, and only those that do not fit even there are counted in
  `analyses_omitted`. The view stays within 8 KiB (the model configuration's `sections.analyses_bytes` can raise it),
  or a quarter of the context's budget when that is smaller. Only an analysis shown with its contract may be chosen:
  one listed by id alone is refused with `proposal_analysis_contract_not_shown`, and the next call shows its contract
  first. An unknown setting's refusal names the analysis's settings (`available settings: ...`). The method's
  `analyses_note` says how to read `default` and `about`, and `other_analyses_note` how to use the listing; neither is
  in the lead's system text, which every run's call carries (an earlier change put them there and the autonomy suite refused it; they
  moved into the view, and a test pins the system text). Before that the view
  showed contracts in catalog order without the settings' descriptions; a test run paused after an
  unknown setting and table refusals; measured on the render service's registry, 13 of its 36 analyses fitted that view, and 9 would with the settings' descriptions. A number in the shown analyses' words may name
  a setting in a step's title or rationale (and in the methods check's reason and details), but not in the summary, a
  finding, a lesson or a part: there it would read as a result value written without its binding. The offline scorer
  reads these numbers from the kept context, so it allows the same ones.
- **What a step names.** A step for the method carries `analysis`: `source`, the step key of a completed analysis in
  the lead's context (completed in the plan and as a task: a step awaiting review is not); `table`, a table its digest
  lists in a result the lead was shown, with 1 to 50,000 rows, 1 to 64 columns and no columns left out;
  `feature`, a shown analysis; and `settings`, checked by type against the catalog (columns of that table, choices,
  ranges; at most 40 of them, within 8,192 bytes as PostgreSQL writes them as jsonb text, the measure the commit
  applies). It has no code, settings field of its own or query. The coordinator refuses any other reply before the
  commit with one of `proposal_analysis_unexpected`, `_missing`, `_unavailable`, `_feature_unknown`, `_source_unknown`,
  `_source_not_finished`, `_source_rejected`, `_table_unknown`, `_table_unsuitable`, `_settings_invalid`,
  `_setting_unknown`, `_column_unknown`, `_setting_invalid` or `_setting_missing`, and after two refusals in a row
  narrows the next reply to one step, except for `proposal_analysis_unavailable`, which no reply can repair.
- **What the commit checks.** The step reaches the turn commit as `template_analysis`: `{source_task, artifact,
  table_path, feature, settings, title, catalog_sha256}`, the title being the step's, `artifact` the result the lead
  was shown the table in and `catalog_sha256` the catalog it chose from. The commit refuses any other shape, or the
  field on another method's step (`template_analysis_invalid`), then runs the checks of "A request" above in the
  database, as the run's actor, on that result's table (`analysis_table_unknown` when it lists none) and only while
  that catalog is the newest (`analysis_catalog_changed` otherwise, and the turn is tried again with the new one). The task's parent is the source analysis, its `request_sha256` covers the analysis, and the request in
  `broker_analysis_requests` records where it came from as `origin` (`run`, `turn`, `action_key`). From there the worker
  and the readers treat it as any template analysis.
- **Places.** A lab dataset's intake or preparation does not count against a run's places, as a figure version never
  has: they are not analyses. A template analysis counts.

The coordinator is not ready for results without migration 060 and EXECUTE on the catalog read
(`grant_broker_coordinator.psql`). The offline scorer (`check-lead-replies`) takes the catalog a call read as
`analysis_catalog`, so a replay checks a step as the coordinator did.

## The run

The worker's route reads the table from the source's retained package and checks the package's size and digest and the
table's digest, as the figure route does, then sends the table's columns and rows with the template and settings to
`POST /render`. Each cell goes as the text the file holds, so the `data.csv` the service writes for the template reads
in R as the table itself does (`NA` is missing; an empty cell is missing in a numeric column and empty text in a text
column). A template can run for minutes, so the worker renews its lease while the service works, every third of the
lease, and checks then whether the task was cancelled. A cancelled run stops waiting, records its receipt (`failed`,
`cancelled`) under the cancelling lease and ends `cancelled`; the service may still finish the call it was given. It
keeps:

| Member | What |
|---|---|
| `results/results.csv` | every row of the results at full precision, as R's `write.csv` writes a table: a missing value is `NA`, unquoted; text is quoted when it is empty, is `NA`, or holds a comma, quote or line break; logicals are `TRUE` and `FALSE`. No row is a blank line, so every reader counts every row. An analysis of a lab's own table (under `results/lab_data/` or `results/lab_derived/`) writes it as `results/lab_derived/results.csv` instead, which the result digest lists without its cells ([lab datasets](lab_datasets.md)). |
| `results/statistics.csv` | the headline statistics as `statistic,value` rows, when the template stated any |
| `results/record.json` | the same results with the method and notes as the template wrote them |
| `figures/figure.png`, `figures/figure.svg` | the figure, when the template drew one |
| `code/<template>.R`, `code/_common.R`, `code/params.json` | the template, the shared R code and the exact settings it read |
| `spec.json`, `README.md`, `SHA256SUMS` | the request, settings as asked and as used, source table, service version and image; how to check and rerun it; every file's SHA-256 |

The service returns two copies of the results: one rounded for display and `results_exact`, the record. The package
keeps the record; a service release older than 0.6.0 returns no record and the run finishes as
`render_service_outdated`. The service writes each number with at most 15 significant digits; the worker reads it as
the nearest double and writes the shortest text that reads back as that double, which keeps each of those digits. A
number sent with more digits than a double holds is kept as the nearest double.

The worker reads only the parts of the answer it keeps (the record, settings, figure, code, version, image, notes and
any refusal); the table the service echoes back and its reproduction page are read past without being held. What it
holds is charged as it is read, and an answer that would take more than 128 MiB to hold finishes as
`analysis_result_too_large` before more is read. The source table is not copied; the package names it by digest. The results tables are
listed in the package's digest, so the workspace pages them like any result table and a figure or another analysis can
take them as its source. The figures are declared as R figures (`figure.r`) in the package listing.

One receipt is recorded per attempt in `broker_analysis_receipts` under the attempt's unexpired lease, also while a
cancellation waits for the worker: `status` (`completed`, `refused`, `failed`), `error_code`, `message`, `refusal`,
`service_version`, `image_digest`, `method`, `files` (each with `path`, `sha256`, `size_bytes`, `media_type` and `role`:
`table`, `statistics`, `record` or `figure`), `notes` (the template's, then the service's) and `package_sha256`. Method
and notes lose control and direction characters and are cut at 2,000 and 300 characters. At most 32 notes are kept, and
the worker stops before the note that would take them past 16,384 bytes as PostgreSQL stores them, the record's own
limit. The same receipt again is accepted; one that differs in any field is `idempotency_conflict`.

## Refusals

The service's error text is never passed on: it can quote a value of the table. A refusal whose code the broker knows,
whose columns (at most eight) are columns of the table, whose counts (at most eight) are whole numbers up to 10^9, and
which fits the receipt's 2,048 bytes finishes the analysis as `refused` with `analysis_refused`, the broker's message for
the code filled from those columns and counts, and the refusal (`{code, columns, counts}`) so the workspace can point at
the setting to change. Otherwise the outcome depends on how the service answered: its HTTP 400, a request it refused,
finishes as `refused` with `analysis_refused` and the general words for refused settings; any other error finishes as
`failed` with `analysis_failed` and a general message. The codes and their messages are in `analysis_run.rs`
(`REFUSALS`); the render service's README lists what each template sends. They include the codes of the analyses of
animals in render 0.6.0 (tolerance-test areas, per-animal window summaries, the body-size ANCOVA and the repeated-measures
mixed model), which refuse a table that would take readings, hours or visits for animals, such as
`repeated_rows_per_animal` and `factor_varies_within_animal`.

| Code | Why the run stopped |
|---|---|
| `analysis_source_unreadable`, `analysis_source_changed` | the source package or its table could not be read, or no longer matches its digest |
| `analysis_table_missing`, `analysis_table_too_large`, `analysis_table_unreadable`, `analysis_table_unsuitable` | the table is not in the package, is over 64 MiB, is not one header and rows of equal width, or has no rows or over 50,000 |
| `analysis_result_unreadable`, `analysis_result_too_large` | the record is not columns and rows of them, or holds over 200,000 rows or 256 columns; the kept answer would take over 128 MiB to hold; or its CSV would pass 64 MiB, its statistics 1 MiB or its `record.json` 96 MiB, each refused before the line or value that would pass the limit is written |
| `cancelled` | the task was cancelled while the service worked |
| `render_service_unconfigured`, `render_service_unavailable`, `render_service_outdated`, `render_response_unreadable`, `render_response_too_large` | as for figure versions |
| `analysis_package_failed`, `analysis_package_not_registered`, `analysis_digest_failed` | the package could not be written or registered |
| `analysis_not_recorded` | the task has no request (a task on the route made another way) |

## Readers

- `GET /api/v1/tasks/{task}/template-analysis`: `{"template_analysis": …}`, the request, its source analysis and every
  receipt, or `null` for any other task. Readers of the task only.
- `GET /api/v1/tasks/{task}/template-analyses`: `{"template_analyses": […]}`, the template analyses made from an
  analysis that the reader may open, newest first.
- The project read's task items carry `template_analysis` (`title`, `feature`, `source_task`, `table_path`,
  `previous`) for a template analysis and `null` otherwise.
- Lineage classes the task `vetted_service` and its outputs `vetted_tool_output`, records its input as `prior_output`
  by `analysis_request`, adds an `analysis` record (source, table, feature, settings, title, previous, request digest,
  service version, image and method), and names no plan step or workspace for it. Only a completed analysis without
  files, or one whose service named no image, is a gap. The attempt names the image, the template and the settings'
  digest.

## Checks

`sh scripts/qualify_remote_broker.sh template-analyses` runs migration 058 twice and then, against an isolated
PostgreSQL and a stand-in render service: readiness by role, the catalogs kept apart, every refusal before a task, the
method offered only through a request, a retry after newer catalogs, the lease fence, the worker's route, the
package's members and digits, receipts, their limits and replays, refusals in the broker's words, an outdated service,
a service that names no image, a figure version and an analysis cancelled under their leases (the analysis while the
service works), readers, lineage, the project's contents and totals, a figure and a second analysis of the results, and
immutability. The route's unit tests cover the CSV form and its round trip through the service's reader and the table
view, the digits of numbers, every limit at its boundary, the refusal words for every code the templates send and the
stop names. `budgeted_json`'s tests cover the held-bytes budget.

`sh scripts/qualify_remote_broker.sh lead-analyses` runs migration 060 twice and then: the coordinator ready only with
the catalog read, which methods a run may list, the method and catalog under the run's lease, every refused shape,
the step the coordinator builds from a reply committed and recorded with its parent and origin, the worker's run of it,
and a lab dataset's tasks holding none of a run's places. `lead_analysis`'s unit tests and
`the_lead_sees_the_analyses_and_its_choice_is_checked_as_the_coordinator_checks_it` cover the lead's view through the
coordinator's own context, every refusal, the request digest, the refusal words and redirects, numbers the shown
analyses' words hold, and the offline scorer with and without the catalog.
