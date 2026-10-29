# Figure versions

A person who may add to a project can draw a figure from one result table of a finished analysis in it, with the
reviewed R render service, and then change the chart, the words, the legend, the size or the resolution and draw
again. Each drawing is its own task, a figure version, with the request, the service's receipt and a package that
reruns it. Nothing is overwritten and no model writes code for it.

## The method

The figure method is a profile on route `server-render`. A lab needs exactly one enabled one granted to it, like any
method (`figure_method_unavailable` otherwise). It runs only from a figure request:

- The profile list (`broker_get_profile_choices`, migration 040) marks it `offered: false`, so the workspace names
  tasks that ran under it but never offers it to run or to give the research agent.
- A person's direct submission of it is refused (`profile_forbidden`), in a project or not.
- A research run that lists it is refused at start, and a running one stops being authorized.

The worker reaches the render service at `--render-url` (`AGENT_SANDBOX_BROKER_RENDER_URL`), an `http` or `https`
base URL without credentials, query or fragment. A worker started with it refuses to run without migration 040 and its
grants (`broker_figure_schema_not_ready`), reads the service's catalog at start and again with each figure version,
and serves the route. Without it, a figure version finishes as `render_service_unconfigured`. The worker is the only
broker component that reaches the service.

## The catalog

The worker reads `GET /features` from the render service and keeps its figure features, with their settings, in
`broker_render_catalogs`. The catalog's identity is the SHA-256 of the stored features, so reading the same catalog
again only moves its last-seen time. A catalog without a `service_version` is older than the route needs
(`render_service_outdated`) and is not recorded; one without figure features is `render_catalog_unreadable`.
A setting's type is one the broker checks: `column`, `columns`, `enum`, `number`, `numbers`, `string` and, since
migration 057, `colors`, which must name a `column` setting of the same chart in `of` and list one to 64 `#RRGGBB`
colors in `choices`. A catalog that declares any other type, or a malformed `colors`, is refused whole and the newest
recorded catalog stays in use, so a render service release that adds a setting type follows the broker release that
checks it. `GET /api/v1/figures/catalog` returns the newest catalog, with `groups`: the reader's
labs, by membership or a project grant, whose figure method is enabled. The workspace offers figures only in a project
of one of those labs.

## A request

`POST /api/v1/projects/{project}/figures` with the mutation intent header and
`{source_task, table_path, feature, controls, title, previous, request_id}` returns HTTP 202 with `{task_id}`.
`broker_request_figure` checks, in one transaction with the task it creates:

| Refusal | When |
|---|---|
| `project_not_found` | the caller may not add to the project |
| `task_not_found` | the source is not an analysis of the project |
| `figure_source_not_ready` | the source has not completed |
| `figure_table_unknown` | the source's newest summarized result digest does not list the table |
| `figure_table_unsuitable` | the table has no rows, over 50,000 rows, over 64 columns, or columns the digest left out |
| `figure_method_unavailable` | the lab has no single enabled figure method |
| `figure_catalog_unavailable` | the worker has not read a catalog |
| `figure_feature_unknown` | the chart is not in the newest catalog |
| `figure_settings_invalid` | a setting the chart does not declare, a value of the wrong kind, a column the table does not have, a required column left out, a string past the chart's `maxlen` or 200 characters, colors that are not one to twelve groups each mapped to a color the chart lists, over 40 settings or 8 KiB, or a title over 200 characters |
| `figure_previous_invalid` | the version it edits is not a version of the same table of the same analysis |

The broker checks each setting's name and kind; the service checks values when it draws, clamps numbers to their
range and says so. The same `request_id` with the same request returns the same task. The request is recorded
immutably in `broker_figure_requests` with the source package's digest, the table's digest, rows and columns, the
catalog's digest and the request's SHA-256. The task's objective is the title, or the chart's name and the table's.

A figure version is not a step of the plan, so a research run's parallel limit does not count it, the lab index's
analysis counts leave it out, and it does not wait for review in the inbox. The project read names each version's
figure: its title, chart, source analysis, table and the version it edits.

## The run

The worker's render route (`src/broker/figure_run.rs`) runs in the worker, not in a sandbox:

1. Read the request of the current lease (`figure_not_recorded` when there is none).
2. Open the source package the request names. Its SHA-256 must match the request, and the table member's SHA-256 must
   match the digest (`figure_source_changed`).
3. Read the table: every cell as the text the file holds, at most 50,000 rows and 64 columns. The service writes the
   cells into the `data.csv` its template reads with R's `read.csv`, so the template reads what `read.csv` reads from
   the table itself.
4. `POST /render` with the chart, the settings, the columns, the rows and the title.
5. Check what came back: a PNG and an SVG, a PDF or TIFF when one was asked for, each by its signature; the template's
   code, the shared `_common.R`, the settings the template read (`spec`) and the service's version. A service that
   returns no `spec` or `common_code` is older than the route needs (`render_service_outdated`). Only those parts are
   held: the table the service echoes back and its reproduction page are read past, and an answer whose kept parts
   would take over 128 MiB to hold is refused as it is read (`render_response_too_large`).
6. Retain the package at `tasks/<task>/attempts/<attempt>-<generation>/exports/payload` with `payload.sha256`, register
   both, and record the result digest.
7. Record the attempt's execution with the service's image digest when the service reports one, one receipt, and
   finish the task. The receipt is recorded under the attempt's unexpired lease, also while a cancellation waits for
   the worker (migration 058 replaced migration 040's fence, which refused it and left a cancelled version waiting for
   its lease to run out). The same receipt again is accepted; one that differs in any field is `idempotency_conflict`.
   The service's notes are kept in order, at most 32 of 300 characters, stopping before the one that would take them
   past the record's 8,192 bytes as PostgreSQL stores them.

The service's HTTP 400 is a refusal, and so is a template's error that carries a refusal code (render 0.6.0,
README "Refusals"): the receipt's status is `refused` and its code `figure_refused`. An R error without a refusal is
`failed` with `figure_render_failed`. The receipt's message is the broker's own words for the refusal code, filled
only with columns of the table and whole counts, so the person can change a setting; a refusal the broker does not
know, or whose columns are not the table's, gets a general message. The service's own words are never passed on: they
may quote what the request sent or a value a template read, and the message reaches the person, the version's record
and the research lead. Any other failure has a fixed code, such as `render_service_unavailable` or
`figure_table_unreadable`.

The package is a deterministic gzip tar:

| Member | Holds |
|---|---|
| `figures/figure.png`, `figures/figure.svg` | the figure, always |
| `figures/figure.pdf`, `figures/figure.tiff` | when asked for |
| `code/<chart>.R` | the reviewed plot template that drew it |
| `code/_common.R` | the shared R code every template reads |
| `code/params.json` | the exact settings the template read |
| `spec.json` | the request, the settings as asked for and as used (with `font_face`, the typeface R drew with, when the service names it), the service's notes, the source table and the service's version |
| `README.md` | how to check the files and draw the figure again with R |
| `SHA256SUMS` | every other member's SHA-256 |

The table is not copied. The package names it by path and SHA-256; it stays in the source analysis's package.

## Previews

`POST /api/v1/projects/{project}/figures/previews` with the mutation intent header and
`{source_task, table_path, feature, controls, title}` answers with the drawing a version with those settings would
have, as SVG, and makes no version, task, package or project history entry. The request passes the same checks as a
version request (migration 057 moves them into `broker_check_figure_request`, which both use).

Only worker Pods reach the render service, so the API records the request and waits up to 35 seconds
while the worker that reaches the service claims it, reads the table from the source package as it would for a
version, draws it and records what it came to. The API hands the result over once.

| Answer | When |
|---|---|
| 200 `{"preview": {"media_type": "image/svg+xml", "sha256", "size_bytes", "svg_base64"}, "refused": null, "notes", "settings_used", "service_version", "image_digest", "catalog_sha256", "table_sha256"}` | drawn; `sha256` is computed by the database over the bytes; `settings_used` includes `font_face` when the service names the typeface it drew with |
| 200 with `"preview": null`, `"refused"` in the broker's words and `"refusal"` (`{"code", "columns", "counts"}`, or null) | the service refused the request, or could not draw the table with these settings |
| 404 `project_not_found` | the person lost the project while the preview waited or was drawn |
| 409 `figure_source_changed`, `figure_source_unreadable`, `figure_table_*` | the worker could not read the table as the analysis's digest names it |
| 413 `figure_preview_too_large` | the SVG is larger than 2 MiB; keep a version to see it |
| 429 `capacity_reached` | the person has a preview still waiting, or asked for 30 in the last minute |
| 503 `figure_service_unavailable` | no worker drew it within the wait, the database did not answer within it, or the service failed |

The answer comes within 40 seconds of the request once its body has arrived, whatever it waits on: the person's
sign-in, the database and the drawing share the first 35, and the last 5 are kept for abandoning a preview that was not
drawn. Reading the request body comes before that deadline and is bounded by the server's own body limit and timeouts. A preview the database
has not abandoned by then is abandoned after the answer goes out. A request that times out while the database records
it may still make a preview, which then waits until it expires. Every broker database call waits at most 15 seconds for a pooled
connection, 10 to open one and 5 to check one before reuse, then fails as `database_unavailable`.

Access is checked again when a worker claims a preview and when its drawing is handed over
(`broker_figure_preview_permitted`): the requester must still be able to contribute to the project, and the source
analysis must still be one of its tasks. A preview whose requester was removed from the project, or disabled, while it
waited ends without being drawn; one whose requester lost access while it was drawn ends with its drawing erased.
Both read `figure_preview_not_permitted`.

Each preview is a row in `broker_figure_previews` while it waits and is drawn. The drawing is erased when it is handed
over. A preview whose request ends first is abandoned at once (`broker_abandon_figure_preview`): the wait ran out, or the person closed the editor and the server dropped the request when the connection closed. It then no longer
counts as their preview still waiting, a worker drawing it cannot record it, and a drawing not yet handed over is
erased; it still counts toward the 30 a minute. A preview nobody draws stops waiting after a minute, and the worker
deletes every preview two minutes after it was made. The drawing passes through the broker database, so its write-ahead log backups hold it for their retention
period, as they hold every broker record.

## The version in the manuscript

A figure is named by its first version; every later version names the one it edits. A person who may steer the project
marks one completed version of a figure as the one in the manuscript:

- `PUT /api/v1/projects/{project}/figures/{figure}/manuscript` with `{"version"}` answers `{"figure", "version",
  "marked_by", "marked_by_name", "marked_at"}`. Marking the version already marked records nothing new.
- `DELETE` on the same path clears the mark and answers 204, also when nothing was marked.
- Refusals: `figure_not_found` (404, not the first version of a figure in the project), `version_not_in_figure` (409),
  `version_not_drawn` (409, only a completed version that drew its figure), `project_not_found` (404, the caller may
  not steer the project).

Marks and clearings are immutable events in `broker_figure_manuscript_events`, each with a project audit entry
(`figure_manuscript_marked`, `figure_manuscript_cleared`). A change moves the project's update time, so open pages see
it through the project's event stream.

## Reading versions

| Route | Returns |
|---|---|
| `GET /api/v1/tasks/{task}/figure` | `{"figure": …}`: a version's request, its source analysis's title, every receipt, `figure` (its first version) and `manuscript` (that figure's mark or `null`), or `null` for any other task |
| `GET /api/v1/tasks/{task}/figures` | `{"versions": […]}`: the versions made from an analysis that the reader may open, newest first, each with `figure` and `manuscript` |

A version's lineage (`GET /api/v1/tasks/{task}/lineage`) has `render`, the request with the service's version and
image digest. Its method class is `vetted_service`, its input class `prior_output` recorded by the render request, and
it has no plan step or workspace inputs of its own. An image gap is named only when a drawn version's receipt has no
image digest, and a missing result only when a drawn version has no files. A version the service refused, or whose R
code failed, drew nothing by design and names neither (migration 041; migration 040 named both).

## The workspace

A finished analysis's result tables offer **Make a figure** in a project the reader may add to, when its lab's figure
method is enabled. The editor offers the charts the table has enough columns for and starts a new figure on the first
one the table suits: a measure for each measured role and a column that is not a number for each category. A measure
is a numeric column whose name does not mark it as an identifier or code (`fdc_id`, `Patient ID`, `sampleId`). It
groups the service's settings into columns, titles, legend, size and resolution, and more settings, each in plain
words. It names a value it cannot send before anything is requested. Drawing waits for the version, shows it on white
paper, lists what the service changed or left out, and keeps the next drawing as a new version of the same figure. A
version's page shows its settings, its outcome and the other versions of the figure, and opens the editor on its
settings; a version that failed reads "Not drawn" there and in its lineage. The source analysis lists the figures made
from it, each by its newest drawn version, and the project lists each figure once.

## Checks

`scripts/qualify_remote_broker.sh figure-versions` runs `tests/broker_figure_versions.rs` against a throwaway
PostgreSQL with a stand-in render service: the catalog, the request and each refusal, the figure method's exclusions,
the worker's route, the package, receipts, a refused setting, readers, lineage, the project read, counts and inbox, the
run's parallel limit and immutability, then previews (claim, drawing, hand-over, refusal, limits, expiry and sweep) and
the manuscript mark with each refusal. `figure-previews` runs the preview and mark routes over HTTP with the worker's
preview loop and a stand-in service, including colors, string limits, a stale table, the size limit, a preview no
worker draws in time, each kind of service error (whose words, holding a made-up cell value, must not appear in the
answer), and access lost before a claim, after a drawing and during the API's wait. The workspace's browser suite draws, refuses, edits and finds a figure against
the loopback preview's stand-in, which draws a simple SVG and says in every package it writes that it is not R.
