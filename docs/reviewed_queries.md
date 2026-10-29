# Reviewed query tools

A reviewed query gives the lead a named question over a registered source, with typed values
instead of SQL. It uses the existing [query guard and worker](reference_sql.md). Registration does not grant a source
or widen its allowed relations or functions. Labs receive tools through their existing source method grants; a run
must include that method. Lead-written guarded SELECTs remain available.

Apply migration 071 before starting the API, coordinator or query worker. Their start checks require it. Existing
runtime function signatures keep their existing grants. Registry tables have RLS and no runtime table grants;
registration has no PUBLIC or runtime EXECUTE grant. Run the owner command with the migration owner's DSN file:

```sh
agent-sandbox-broker reviewed-query register --tenant research-site \
  --migration-dsn-file /run/private/migration.dsn \
  --file profiles/reviewed_queries/fdc-foundation/fdc-find-foods/tool.json \
  --template profiles/reviewed_queries/fdc-foundation/fdc-find-foods/template.sql
```

The command reads a bounded JSON definition and template, hashes the exact template bytes, lowers with every
parameter's default or a typed sample, admits through the unchanged guard and then calls the owner-only storage
function. It prints only tool key, version and template digest. SQL checks the schema, source, cap and template digest.
Registration is idempotent on tenant/key/version; a different body is `idempotency_conflict`. Rows are immutable
except an owner's transition of `definition.state` from `enabled` to `retired`. Retirement prevents new proposals and
commits; it does not rewrite or disable an already committed query. Use a new integer version for a changed tool.
Only the highest enabled version of a key is shown and chosen at proposal time and its exact version is kept with the step.

A definition has `key` (`^[a-z][a-z0-9-]{2,63}$`), positive integer `version`, `source`, `title` (120 characters),
`purpose` (1,000), `parameters` (zero to sixteen), `output_columns` (one to 64 name/type/description objects), `row_cap`
(no greater than the source's) and `state: enabled`. Output descriptions and parameter descriptions have at most 300
characters. Parameter names are lowercase SQL-style names; their order binds `$1` through `$n` and every parameter
must be used. Each parameter has `name`, `description`, `type`, and either `required: true` or a `default`.

| Type | Schema and value rules | SQL literal type |
|---|---|---|
| `text` | `max_length` from 1 through 256; strings only, no control characters, normalized to NFC before the character limit | `pg_catalog.text` |
| `integer` | finite inclusive `minimum` and `maximum` in the signed 64-bit range; JSON integers only | `pg_catalog.int8` |
| `number` | finite inclusive `minimum` and `maximum`; finite JSON numbers only | `pg_catalog.numeric` |
| `enum` | 1 through 64 distinct closed NFC strings, each at most 256 characters and control-free; exact membership | `pg_catalog.text` |

Values are never identifiers. The lowerer parses exactly one SELECT, replaces each protobuf `ParamRef` with an
`A_Const` inside a `TypeCast`, deparses and reparses, and requires equal trees after removing spelling offsets.
Unknown parameters, missing values, wrong types and bounds are refused with fixed codes and registered parameter
names; supplied values and unknown names are never echoed in faults. The request JSON is capped at 8 KiB. A typed
parameter in LIMIT is refused by the existing guard; use a fixed LIMIT within the tool cap, or an ordered
`row_number()` predicate for a smaller typed result limit, as the supplied profiles do.

The methods context lists each tool under its source's `query.reviewed_queries`: key, version, title, purpose,
parameter schema, output columns and cap. It contains no SQL template. Instructions travel only with methods offering these tools, leaving the shared system
prompt unchanged. A lead action can carry
`"reviewed_query": {"tool": "example-tool", "parameters": {"name": "synthetic"}}` instead of `sql`, with no code,
settings or template analysis. Proposal checks always lower and admit again, even if the owner inserted a row without
using the registration command. Faults reach the next lead call as `proposal_reviewed_query_invalid` with the step,
registered parameter and fixed cause, or `proposal_reviewed_query_unknown`. A refused proposal creates no worker task.
Commit binds the template, version, schema and cap to the enabled registry row. The worker checks the parameter
record against the stored lowered SQL and re-admits it before connecting to the source reader.

Execution remains repeatable-read and read-only under the source's reader and timeout, with the tool's row cap and
the existing result byte bound. Lineage uses method class `reviewed_query` and output class `query_output`; results
remain unvalidated until a person accepts them. The package retains all existing query files plus
`method/template.sql` and `method/parameters.json`. The latter keeps tool/version, template and schema digests, the
parameter schema, requested and normalized parameters, and cap. `method/query.sql` is the lowered admitted SELECT.
SHA256SUMS covers these files. The schema digest is SHA-256 of the broker's compact serialized parameter list;
requested parameter omission and default use remain distinguishable. Live database version receipts describe what
was read; they do not restore rows changed by later imports.

A lab's own tools live with the lab's database source, not in this repository, because they name its schema and its
questions. `agent-sandbox-broker reviewed-query render --file tool.json --template template.sql --source-file
source.json` runs the registration check offline against a declaration file (`schema_name`, `relations`, `functions`,
`row_cap`) and prints the body `register` would store, so a lab repository can check its folders without a database.
Registration does not make a tool scientifically accepted. `scripts/qualify_remote_broker.sh reference-sql`
adds owner registration, immutability and grants, a bad directly inserted template, named parameter refusal, a lead
proposal, worker execution, lineage and the three method files. Run that PostgreSQL mode and a paired source replay
before rollout.

The FoodData Central Foundation tools (`profiles/reviewed_queries/fdc-foundation/`) exist for every enrolled Foundation
release. `fdc-find-foods`, `fdc-food-nutrients` and `fdc-rank-foods-by-nutrient` are the reviewed April 2026 tools;
`<tool>-<yyyy>-<mm>` are their copies for the other releases, written by `scripts/generate_fdc_release_tools.py` from
the reviewed templates and `tests/fixtures/reviewed_queries/fdc_foundation_releases.json` (each release's declared
relations, functions and cap, and the stored type of `food.publication_date`). Keys are unique per tenant, so each copy
names its release; the lead sees a tool under its own release's source. A release without `food_category` (April and
October 2023) returns that column as NULL text and drops the join; nothing else in a template changes. Each copy's
description states its live check (counts only, `fdc_foundation_release_checks.json`). After a reviewed template
changes, run the script, then `--check` and `every_fdc_foundation_request_admits_with_its_release_declaration`.

The same three questions exist for the other FoodData Central data types labs are granted, in
`profiles/reviewed_queries/fdc-sr-legacy/` (SR Legacy, April 2018), `fdc-fndds/` (the five FNDDS releases) and
`fdc-branded/` (Branded Foods, April 2026, source `fdc-branded-2026-04-r2`), written by
`scripts/generate_fdc_data_type_tools.py` from `tests/fixtures/reviewed_queries/fdc_data_type_sources.json`. Each
follows its Foundation counterpart with the data type's own table in place of `foundation_food`: `sr_legacy_food`
(adding the NDB number), `survey_fndds_food` (the food code and WWEIA category, whose column names changed in October
2020) or `branded_food` (brand owner, brand name, GTIN or UPC and branded category; find also matches the brand
fields). Each of those tables is unique on `fdc_id`, so no join repeats a food. Descriptions state each tool's live
check (`fdc_data_type_checks.json`); `every_fdc_data_type_request_admits_with_its_source_declaration` checks all 21.
