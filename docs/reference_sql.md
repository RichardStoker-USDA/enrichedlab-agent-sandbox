# Guarded database queries

A source's method lets the research lead answer part of a question from a public reference database by writing one
PostgreSQL SELECT. No code runs. The broker's guard admits the query, the worker runs it in a read-only transaction
against a pinned snapshot, and the table PostgreSQL returns becomes that analysis's result, packaged like an R
analysis's.

## A source

A source is a row in `broker_sql_sources` (migration 038). The operator registers it once, as the migration owner,
after the snapshot is loaded and its reader login exists:

```sql
SELECT enrichedlab_agent.broker_register_sql_source('<tenant>', '<source JSON>'::jsonb);
```

| Field | Meaning |
|---|---|
| `source` | key, `^[a-z][a-z0-9-]{2,63}$` |
| `profile` | the method's profile key; the profile must already be enrolled, and it has at most one source |
| `title`, `description`, `release` | what the lead and the scientist read about the data |
| `schema` | the schema that holds the relations |
| `relations` | 1 to 128 objects (64 before migration 062), each with `name` and `columns`; the lead reads them as the source's shape, with relation and column descriptions screened before either the whole or partial presentation, and a large source within its share (`research_coordinator.md`) |
| `functions` | 1 to 128 function names a query may call |
| `row_cap` | 1 to 100,000; the most rows a query may return |
| `statement_timeout_ms` | 1,000 to 300,000 |
| `connection_ref` | the name of the worker's connection file for this source |
| `snapshot` | an object with `manifest_sha256` and whatever a scientist needs to rebuild the snapshot (release files and checksums) |

Registering the same source again returns the stored row. The same key with a different profile, relation list,
connection or snapshot is refused (`idempotency_conflict`). Rows are immutable, so a new snapshot or relation list needs
a new key and profile.

A source also states what kind of data it holds, `data_class` (migration 039): `public_reference`, or since migration
053 `lab_database` for a live lab database (below). The table refuses any other class.

A public reference source's database must hold a one-row `snapshot_manifest` table in the source's schema whose
`manifest_sha256` equals the source's `snapshot.manifest_sha256`. The reader login needs SELECT on the listed relations
and on `snapshot_manifest`, and nothing else. That login and the read-only transaction are the primary controls. The
guard is a second one that also tells the lead what to fix. A lab source has no `snapshot_manifest` (below).

## A live lab source

A lab database keeps receiving imports, so it has no pinned snapshot to confirm. A lab source states
`"data_class": "lab_database"` and a live snapshot:

```json
"snapshot": {"kind": "live", "version_relation": "vw_database_version", "declaration_sha256": "<digest of the declaration>"}
```

`version_relation` names a relation in the source's schema that holds exactly one row: the database's own statement of
its version, such as its newest schema migration and newest completed import. It is not one of the listed relations,
so no query reads it. `declaration_sha256` is the digest of the source's declaration; schema cards are keyed by it as
a public source's are keyed by its manifest (migration 047). A live snapshot names no manifest, so a worker older than
migration 053, which needs one, refuses a lab source's query before it connects (`snapshot_not_recorded`) and can never
publish a lab result. The table's check holds the pairing: a live snapshot only with `lab_database` and without a
manifest, and a `lab_database` source only with a live snapshot naming a version relation and a declaration digest.

The worker reads the version row first, in the same read-only repeatable-read transaction as the result, refuses the
query when the relation is unreadable or holds other than one row (`source_version_unreadable`) or the row is larger
than 2 KiB (`source_version_too_large`; the database withholds such a row, so it never reaches the worker), and records
the row with the receipt (`source_version`, migration 053), in the task's lineage and in the package's `receipt.json`.
A completed receipt of a lab source must carry the version and one of a public source must not
(`query_receipt_version_invalid`), and a retried receipt must equal the recorded one in every field. The package's README says how to rerun: the same query against the lab database returns the same table
while no import since that version changed the rows it reads. The query detail shows the source's class and each
receipt's version, and lineage names the class `lab_database`.

The reader login, the read-only transaction and the guard are the same three controls as for a public source; the
login should also have column grants for any column the lab keeps from agents, so a query naming it fails in the
database. Which lab database the agents may read, and who reviews what they find, is decided per lab, not here.

## The method

The source's profile uses route `server-query`. Like any method it needs a grant, a catalog entry and a starting-point
entry. The worker reads the source's connection from `<dir>/<connection_ref>.dsn`, where `<dir>` is
`--query-connection-dir` (`AGENT_SANDBOX_BROKER_QUERY_CONNECTION_DIR`). The file is a PostgreSQL connection string
read like any other broker secret; `sslmode=disable` is accepted only for a loopback qualification database. A lab's
own database has a certificate its own authority signed: put that authority's PEM certificates in
`<connection_ref>.ca.crt` beside the `.dsn`. That file is opened once and read with a 64 KiB bound; it must hold PEM
`CERTIFICATE` blocks and whitespace only, so a key, a revocation list or any other text refuses it, and a link whose
target is missing is an error, never an absent file. With it, the connection trusts that authority alone, never the
process's own, refuses a server name the certificate does not hold, and requires TLS: `sslmode=disable` is refused, and
`prefer` (also the default when the string names no mode) becomes `require`, so a server that answers "no TLS" ends the
connection. Write `sslmode=require`: the pinned driver parses only `disable`, `prefer` and `require`, and the TLS
library verifies the chain and the host name itself. `source-card --connection-file` reads the `.ca.crt` beside its
file the same way. `scripts/qualify_query_tls.sh` checks these rules in the broker image's toolchain stage, with OpenSSL
as the worker, against PostgreSQL with TLS and in plain text, with the process's own trust and with the process
trusting the lab's server. A worker started with that folder refuses to run without migration 038 and its grants
(`broker_query_schema_not_ready`).

## The guard

`src/broker/sql_guard.rs` (`guarded-sql/v1`) parses the query with libpg_query, the parser PostgreSQL itself uses, and
walks the whole parse tree. It refuses with a fixed code, and for a relation, function or node kind it also names what
it refused; for a query it cannot parse it names the parser's message, such as `syntax error at or near "UNION"`, cut
to 200 characters (the autonomy suite's range question was refused twice with the code alone). Names go to the next
lead call only, never a log:

| Code | Refused |
|---|---|
| `sql_refused_size` | empty, or over 8 KiB |
| `sql_refused_parse` | not valid PostgreSQL |
| `sql_refused_statements` | more than one statement |
| `sql_refused_not_select` | anything but a SELECT |
| `sql_refused_node` | a parse node outside the read-only list, such as a data-modifying CTE |
| `sql_refused_select_into` | `SELECT … INTO` |
| `sql_refused_locking` | `FOR UPDATE` and the other locking clauses |
| `sql_refused_values` | a `VALUES` list |
| `sql_refused_no_source` | a SELECT, or one branch of a set operation, without `FROM`, or a query that reads no listed relation |
| `sql_refused_relation` | a relation the source does not list, anywhere in the tree; a CTE reference is not a relation |
| `sql_refused_function` | a function the source does not list, anywhere in the tree, or a function in `FROM`; in the worker, a name after a qualifier that PostgreSQL would read as a call to one |
| `sql_refused_constants_only` | an output with no column, function call or subquery, so every value would be typed in |
| `sql_refused_unordered` | no `ORDER BY`, unless the top-level SELECT has no set operation or `GROUP BY`, contains at least one non-window aggregate, and builds every target only from aggregates, constants, casts, `+`, `-`, `*`, `/`, `%`, `COALESCE`, `GREATEST`, `LEAST`, `NULLIF`, or a supported scalar call |
| `sql_refused_limit` | a `LIMIT` that is not a constant within `row_cap`, or `FETCH … WITH TIES` |

The read-only list includes `x IS [NOT] JSON [VALUE | ARRAY | OBJECT | SCALAR] [WITH | WITHOUT UNIQUE KEYS]`, so a
query can test stored text before casting it, as in `CASE WHEN c IS JSON OBJECT THEN c::jsonb END` (a cast of text
that is not JSON fails the whole query). The SQL/JSON query functions and constructors (`JSON_QUERY`, `JSON_VALUE`,
`JSON_EXISTS`, `JSON_TABLE`, `JSON_OBJECT` and the rest) parse to other nodes and stay refused.

Relations and functions are collected by the guard's own walk of every node (`src/broker/sql_names.rs`), not by
pg_query's `tables()` and `functions()`, which descend only some node kinds. Before that walk, a subquery or call
inside `COLLATE`, a subscript, a window definition, an aggregate's `FILTER`, `ORDER BY` or `WITHIN GROUP`,
`DISTINCT ON`, `OFFSET`, `GROUPING SETS` or `ROLLUP`, a `CASE` operand or the left side of `IN` went unchecked, a CTE
name exempted a relation of the same name anywhere in the query, and names were compared in lowercase. Now a CTE name
stands for the CTE only where PostgreSQL resolves it so: unqualified, inside the query that defines it, and without
`RECURSIVE` only after its own definition. Names are compared exactly, since the parser has already folded unquoted
names. `src/broker/sql_guard_bypass_tests.rs` puts an undeclared relation and function in each of those positions.

PostgreSQL also reads a selection `(expr).name` as the call `name(expr)` when `expr` has no field `name`, for any
function one argument reaches and any expression: `(1::int).pg_cancel_backend` calls `pg_cancel_backend(1)`. The guard
refuses a selection unless `name` is a function the source lists (none of the 1,489 stored queries or 75 registered
templates used one). A qualified column `q.name` is the call `name(q)` when `q` has no such column, and the guard
cannot see columns, so it hands the worker every such name the source does not list as a function. In the query's own read-only transaction, before anything is read, the worker
refuses the query with `sql_refused_function` if one of them names a plain, aggregate or window function in
`pg_catalog` or the source's schema whose first parameter takes a row and whose other parameters have defaults. On
PostgreSQL 18 that is 45 `pg_catalog` names, `row_to_json`, `to_jsonb`, `pg_typeof`, `count` and `quote_literal`
among them; no source schema defines one. The `reference-sql` PostgreSQL mode sends `SELECT f.description,
f.row_to_json FROM food f` through the worker and expects that refusal.
The guard keeps the version `guarded-sql/v1`: the database checks that constant on every stored query, and each
receipt names the worker image that admitted the query.

The supported scalar calls are `round`, `trunc`, `abs`, `ceil`, `ceiling`, `floor`, `sign`, `sqrt`, `power`, `ln`,
`log` and `exp`; a column outside an aggregate, window call, other function (including a set-returning function),
subquery, `CASE`, parameter, constructor or unknown node does not qualify.

A query without a `LIMIT` gets `LIMIT <row_cap>` on a new line; the admitted text records that. Nothing else in the
lead's text changes in the guard.

Before the guard, the coordinator casts the first argument of every two-argument `round` or `trunc` to numeric
(`sql_guard::cast_places`): `round(avg(amount), 2)` becomes `round((avg(amount))::numeric, 2)`.
PostgreSQL defines those forms only for numeric, so a double precision value, such as every FoodData Central amount,
failed as `query_function_unknown` when the query ran: 38 of 39 failed queries in one series of suite runs. An
integer or numeric argument is converted the same way PostgreSQL would convert it, so the cast changes no query that
ran before. The calls come from the parse tree and the argument's extent from PostgreSQL's own scanner, so a comma or
parenthesis in a string or comment is never mistaken for one; an argument already cast to numeric is left alone, so the
cast text needs no second cast. The coordinator stores the cast text as the lead's query, the worker admits that text
again with the guard alone, and the turn record counts the casts as `query_places_cast`; the lead's own reply stays kept
by its hash.

## The decision

The coordinator reads the sources its run may query and adds each one to its method as `query`: title, release,
description, relations, functions and row cap, never the connection or schema. A proposed step for that method carries
`sql` and no `code` or `settings`; a step for any other method must not (`proposal_sql_missing`,
`proposal_sql_unexpected`). The coordinator admits the query and sends a refusal back to the next lead call as
`prior_attempt`, with the refused names. The turn commit stores the lead's text (with any cast above), the admitted text and its SHA-256, the
guard version, the relations and the row limit in `broker_task_queries`, and refuses a query whose digest, relations or
limit do not match the source (`query_invalid`). The task's request digest binds the admitted text. The context version
is 9. The lead may also see the admitted query of an earlier analysis its lab's knowledge search
found, to adapt it (migration 042, `research_coordinator.md`); an adapted query goes through the same guard. Each of
the project's own query analyses shows the query it ran, its status and row count (migration 043), and a proposed query
that repeats a finished one exactly is refused (`proposal_query_repeated`); a query that finished before the cast is
compared in its cast form too, so proposing it again is still a repeat.

## The run

The worker's query route (`src/broker/query_run.rs`) runs in the worker, not in a sandbox:

1. Read the admitted query for the current lease. The stored digest must match the text.
2. Admit the stored proposed text (the lead's, with any cast) again with the source's lists. The result must equal the stored text and limit
   (`guard_mismatch`).
3. Connect with the source's reader login and open a `REPEATABLE READ, READ ONLY` transaction with the source's
   statement timeout and the schema as the search path.
4. Check `snapshot_manifest` (`snapshot_not_found`, `snapshot_mismatch`).
5. Let PostgreSQL write the table with `COPY (…) TO STDOUT WITH (FORMAT csv, HEADER true, NULL 'NA')`, adding
   `FORCE_QUOTE *` for a one-column result, so the table's bytes are the server's own output. A NULL is `NA`, as R
   writes a missing value; PostgreSQL quotes a text value equal to `NA`; and with one column every other value is
   quoted, so an empty text value is `""`, never a blank line that a CSV reader skips. The receipt's row count is the
   rows of that output. A table over 64 MiB fails (`query_result_too_large`). The worker refuses a query admitted without
   `ORDER BY` under the single-row exemption if its result has more than one row (`query_not_single_row`), keeping no
   result artifact or digest.
6. Retain the package at `tasks/<task>/attempts/<attempt>-<generation>/exports/payload` with `payload.sha256`, register
   both, and record the result digest.
7. Record one receipt for the attempt and finish the task: `query_completed`, or `query_not_completed` with the code.

A failure the lead can act on finishes the task with a fixed code the next decision reads, for example
`query_column_unknown`, `query_type_mismatch` or `query_timeout`. The receipt's status is `refused` when the guard or
the source's records stop the run and `failed` otherwise.

The package is a deterministic gzip tar:

| Member | Holds |
|---|---|
| `README.md` | what the package is, how to check it, how to rerun the query and how to read the table in R |
| `method/query.sql` | the admitted query |
| `receipt.json` | source, release, snapshot, guard, query SHA-256, the proposed text, row limit and count, column names and types, table SHA-256, server version |
| `results/query_result.csv` | the table |
| `run_parameters.json` | method, source, release, row limit and guard |
| `SHA256SUMS` | every other member's SHA-256 |

In the CSV an empty unquoted field is NULL and an empty text value is written as two quotes. R's `read.csv` does not
tell those apart; the README says so.

## Reading it

`GET /api/v1/tasks/{task}/query` returns `{"query": …}` for anyone who may read the task: both texts, the digest,
guard, relations and row limit, the source's title, description, release and snapshot, and every receipt. It is `null`
for a task that ran no query. Lineage classes the task `guarded_query` and its outputs `query_output`, adds a `query`
field with the source's title, release, snapshot digest and class (migration 039), names the source's class in
`inputs.classes` with `classes_recorded_by` set to `query_source`, and keeps the review state `unvalidated`. The workspace
shows "The database query" on the analysis page: the checks it passed, the outcome and the query text, shown only after
its SHA-256 matches. Its "Where this came from" Data row names the source, its kind of data and the pinned snapshot.

## Schema card

A card (migration 047) states facts about a source's snapshot that a lead otherwise spends queries
on, measured from the data so none is typed in. The operator declares what the source has; the broker measures it:

```sh
agent-sandbox-broker source-card --connection-file <reader DSN file> < input.json > card.json
```

`input.json` is one object: `source` as `broker_register_sql_source` took it and `spec`, with `source` (its key),
`joins` (`{"from": "relation.column", "to": "relation.column"}`, where `to` must identify one row), `values` (columns
with at most 64 values) and `lookups` (`relation`, `key`, `label`, up to four `detail` columns and an optional
`count_by` column whose rows are counted per key). The command opens one read-only repeatable-read transaction as the
reader, confirms `snapshot_manifest`, and writes: each listed relation's rows, its unique keys and how many rows lack
each column's value; each join's condition, with `::text` on the side that is not text when the two columns' types
differ (an integer category id against a text one), and how many rows find a match; each value column's values with
their rows; each lookup's entries with how many `count_by` rows match each under that join's own condition, so a
numeric key `1.0` counts the rows whose integer is 1. A key comes from a valid unique index without a predicate or
expression, over its key columns only (not `INCLUDE` columns), when every one is listed and never missing. A spec that
names a column the source does not list, a join target that is not unique, more than 64 values or 2,000 entries, or a
text over 300 characters is refused with a fixed code. The card is at most 256 KiB.

Before it counts anything, the command prices each listed relation by the planner's estimate of reading its listed
columns (`EXPLAIN (FORMAT JSON) SELECT <listed columns>`; PostgreSQL checks the privilege of every column a statement
names, `EXPLAIN` included, so a card never needs a column the source does not list) and counts exactly the cheapest first, while one costs at most a quarter, and all of
them together twice, of what the source's statement timeout counts at 100,000 cost units a second (`Budget`; on the
site's lab database host 12.3 million rows over ten columns, cost 288,000, took 1.7 s). Any other table, partitioned
table or materialized view is measured from a seeded block sample (`TABLESAMPLE SYSTEM (p) REPEATABLE (0)`) scaled to the
whole: it aims at a million rows, reads at most half of the relation and costs at most half of one relation's exact
budget, and the samples together cost at most half of the exact total. A relation no sample may read within that, and
any view not counted exactly, is not read; its rows are the planner's estimate. Such a relation carries `estimate`, `{"method": "block_sample", "percent",
"sampled_rows"}` or `{"method": "planner", "cost"}`. A sampled relation keeps only its primary key, since a sample cannot
show that no row lacks a key value, and a relation of a million rows or more is marked `large` unless the planner
estimated it. A join, value list or lookup may name only relations counted exactly; any other is refused as
`card_spec_relation_too_costly:<relation>`. A statement that fails names its relation (`card_plan_failed:` or
`card_measure_failed:`). The planner can price a view far below its cost (on one lab database a view priced within
the exact budget ran past the 60-second timeout), so a relation no join, value list or lookup names is read
under half the statement timeout, inside a savepoint, and only until four statement timeouts have passed since counting
began (`Budget::seconds_total`). A read PostgreSQL cancels for time is rolled back to the savepoint and the relation
takes the planner's estimate; once the time is spent the remaining such relations are not read. A relation a declaration
names is always counted under the full timeout, and a count that fails ends the card. Whether a costly relation is
counted can therefore differ between two runs on the same source; every estimate is marked. Cards measured this way say
`"generator": "source-card/2"`; registration and older cards are unchanged.

The operator registers it as the migration owner:

```sql
SELECT enrichedlab_agent.broker_register_sql_source_card('<tenant>', '<card JSON>'::jsonb);
```

The card must name a registered source, the snapshot the source records and only relations it lists, and have the
shape the coordinator reads: each count a whole number of at most 18 digits, no join matching more rows than it has,
and every lookup entry as wide as its columns. The function computes the card's digest from the stored form and
returns it; the same card again returns the same row, and rows never change. `broker_research_run_sql_sources` returns
the newest card registered for the source's snapshot as `card` with its `card_sha256`, so a new snapshot without a card
shows none.

`agent-sandbox-broker source-card-view` reads `{"card", "card_sha256", "title", "question", "context", "room"}` and,
for a source shown in part, `relations`, the relations the call shows (`context` holds the constraints and instructions;
both may be left out; at most 512 KiB, more is refused) and prints what a lead call with that question and room would
be shown, with its record, or `{"shown": null}` for a card the coordinator would leave out. It opens no connection, so an operator can check a card against a set of questions before registering
it.

The coordinator shows the card as `data_cards` (`research_coordinator.md`): keys, joins, values and missing values
first, then the lookups. A card with estimates also shows, after the joins, `large_relations` (each large relation's
rows, marked `estimated` when sampled) and `costly_relations` (the relations it did not read), and a sampled relation's missing
values as `missing_values_estimated`; query facts cite a relation's rows only when the card counted them exactly. Before ranking or sizing, the fixed text screen removes a lookup entry if any text cell
matches, and removes each matching listed value. Structured cells are withheld as `non_text`. The stored card is
unchanged: query checks and measured counts still use every entry. The card's `values_others` counts withheld values
by column; lookup `others` includes withheld entries, and a whole screened lookup says `shown: "screened"` instead of
claiming to show all entries. `context_inputs.data_cards[].withheld` records the source, relation, column, field,
reason, field SHA-256 and screen version without the text, even when no card fits. The methods specialist inherits
this screened card from the lead's context.

The complete card view then passes the recursive source-text boundary before ranking, sizing or recording it:
every string value and object key passes `screen::screen`, including joins, lookup columns, missing-count keys,
`values` and `values_others` keys and custody records. Failed values become `{"withheld":true}` (`"withheld"` in
scalar arrays); failed keys become `withheld_<n>` within their object, skipping existing clean keys. Boundary records
contain only a screened source key, safe key/index location, key/value kind, reason, SHA-256 and screen version.
Missing counts must be numbers; join text and lookup columns must be strings. Invalid metadata is dropped and
recorded (`non_number` or `non_text`), with a dropped lookup column's cells removed from the view to retain alignment.
Neither this projection nor screening changes the stored card used for computation.

Each lookup is shown whole when it takes at most a quarter of the card's room and fits what it is
given, otherwise as its entries that match the question, strongest first, with `more` counting matches left out and
`others` the entries that did not match. A word of an entry matches when it is not a stop word or one of the source's
one-word relation and column names (`food`, `amount`; a compound name such as `food_protein_conversion_factor` stops no
word), has two characters or more, is not only digits, and belongs to at most two measures (entries sharing the label
before its first comma or parenthesis are one measure); two adjacent words count when the entry has the same pair, one
of them is such a word and the other is a single letter or not a stop word, so "vitamin C" and "vitamin A" match
although "vitamin" alone is common, and "per 100 g" matches nothing. The question's own words decide first; the
constraints and instructions only order the entries the question does not reach. The other forms and units of a matched
measure follow it, so energy in kcal brings energy in kJ. Lookups the question names come first, strongest first, then
the rest, shorter lists first. The room is shared in three passes, each choosing a lookup's form by the bytes it takes,
so no lookup holds room it does not use: first each lookup with matches, in that order, takes its smallest form (its
strongest entry, or the whole list when that is smaller) while any room is left; then the rest is shared out, the
lookups with matches before the others and the smallest remaining need first, each at most an equal part of what
remains; last, in the same order, each lookup takes the most it can of what is left. A short list takes only what it
needs, and a list that shares only a few of the question's words cannot crowd out the measure it asks about.
Measured on the FoodData Central card and the suite's six goals, every answer-key nutrient and food group is shown down
to 3,000 bytes of room. A query can still read any entry; the card only saves the lookup query and the shortened name
that returns no rows.

### A card follows its data (migration 082)

A card names what it was measured from: `spec`, the spec it was measured with, and, for a live lab source,
`measured_at`, the version row the source's database stated in the card's own transaction. Registration keeps both in
columns and refuses a live source's card without them (`sql_source_card_version_required`,
`sql_source_card_spec_required`); a pinned source's card carries no `measured_at`. The lead's view shows neither.

The run's sources carry the newest card's version and the version the source's newest completed query read. When both
are known and differ, the lead sees `card_behind_data` beside the card: a fixed note that counts may be low and newer
values missing. The version rows are the lab database's own text and are never shown; the turn's record marks
`card_behind_data: true` and its `shown_sha256` covers the note. No count is changed.

After a completed live query, the worker asks `broker_begin_sql_source_card_refresh` whether the card is due. The
database starts one refresh per source and version, only for a version a completed receipt of that source read, only
when the newest card has a spec and another version, and never while another refresh of the source runs (a running
refresh older than 15 minutes is closed as `abandoned`). The worker measures the card as the source's own reader with
the newest card's spec, and `broker_finish_sql_source_card_refresh` registers it only when it passes the registration's
checks, states the version the refresh started for and carries the same spec; otherwise the refresh ends `refused`
(`version_moved`, `card_spec_changed`, `card_invalid`, `declaration_drift`) or `failed`, and the previous card stays in
use. A relation, column, function or grant the reader can no longer use is `declaration_drift`: the declaration changes
only through a reviewed `lab-source` step. Every refresh is a row of `broker_sql_source_card_refreshes`, and the worker
logs `event=card_refresh` with the source key, state and code only.

## Checks

- `cargo test --locked source_card` covers the card's view, its matching, join casts and name checks; the coordinator's
  tests cover the `data_cards` section, its share of the budget and its record.
- `cargo test --locked sql_guard` covers the guard's refusals, the added limit and the cast to numeric.
- `agent-sandbox-broker check-queries` runs the guard on queries read from standard input, one JSON object per line
  with `id`, a `source` shaped as the coordinator reads it (profile, source, title, description, release, schema,
  relations, functions, row cap) and `sql`. It casts each query as the coordinator does, and for each line writes the
  admitted text with its SHA-256, relations, row limit and the number of calls cast, or the refusal code and the names
  it refused. It uses the coordinator's own per-source check and opens no
  database or network connection, so an operator can run it in a running pod to show what that image's guard admits.
- `scripts/qualify_remote_broker.sh reference-sql` runs the PostgreSQL path: migration 038 twice, readiness with and
  without the grants, registration, the run's sources, admitted queries only, read-only execution against the named
  snapshot, the package, digest and receipt, the failure codes, readers and immutability, the source's class and the
  query's lineage without a class gap; it also measures a schema card as the reader, refuses another snapshot, a
  non-unique join target and an unlisted column, measures the same source exactly within its timeout's budget and,
  beyond a tighter one, a table from a seeded sample (primary key only) and a view by the planner's estimate, and refuses
  a declaration over a relation not counted exactly, registers the card idempotently, refuses a card for another snapshot or
  relation, keeps cards immutable and shows the run's source with its card and digest.
- The earlier-query read (migration 042) returns a finished query analysis of another lab-visible project to a later
  run of the same person in the same lab, and nothing for a private project, a failed query, a method the run may not
  use, an unknown task, another worker's claim, more than twelve tasks, the run's own project, or an analysis a person
  rejected.
- The own-query read (migration 043) returns the queries of the run's own project among the tasks asked, in the order
  asked, each with its newest receipt's status, error code and row count, and nothing for another project's task or an
  unknown task; another worker's claim and more than 64 tasks are refused.
- The API, coordinator and a worker with a query folder refuse to start without migration 038 and their grants; the
  coordinator also needs migrations 042 and 043 and their grants.

## Limits

This slice reads public reference data only. A query reads one source; there is no join across sources and no
parameter binding, so a code the database holds is found with one query and used in a later decision's query, unless
the source's schema card shows it. The
guard does not judge whether the query answers the question; the methods specialist reads it, and the results stay
unvalidated until a person reviews them.

Reviewed questions with typed parameters use [reviewed query tools](reviewed_queries.md). They share
this guard and worker; packages also keep the immutable template and requested and normalized parameters.
