# Result tables and byte ranges

A person reads a result table in the browser without downloading the package that holds it. The broker pages and
summarizes a CSV or TSV table from its verified stored bytes and serves byte ranges of a package member or a stored
file. Values always leave as the stored strings; the broker never rounds, converts or reformats one.

## Routes

| Method | Path | Answer |
|---|---|---|
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/pages?path=&offset=&limit=&sort=&order=` | one page of rows |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/summary?path=` | every column's summary with a receipt |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/contents/file?path=` with `Range` | part of one package member |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/download` with `Range` | part of the stored file |

`path` names a CSV or TSV member of a retained result package, as the `contents` listing names it. It is left out only
when the artifact is itself a CSV or TSV file. A reader is anyone who may read the task; anyone else gets 404.

A page is `{"path", "sha256", "columns", "types", "rows", "total", "offset", "limit", "sort"}`. Each row is
`{"index", "line", "values"}`: `index` is the row's place in the file (0 is the first data row) and `line` is the
1-based line where its record starts, so a quoted value with line breaks keeps the next row's line exact. `offset`
starts at 0 and `limit` is 1 to 1000, 100 by default. `sort` names a column and `order` is `asc` (the default) or
`desc`; the answer's `sort` is `{"column", "order"}` or `null`.

A summary is `{"path", "sha256", "columns", "receipt"}`. Each column is `{"name", "type", "type_source", "count",
"missing", "min", "max", "distinct"}`, and the receipt is `{"routine": "enrichedlab.table-summary", "version": 1,
"input_sha256", "output_sha256"}`.

## Rules, version 1

- The file is UTF-8. A leading byte-order mark is dropped, the first record is the header, blank lines are skipped,
  and every row has the header's field count. Both CSV and TSV accept RFC 4180 quoting, which R writes by default.
- A value is missing when it is empty or exactly `NA`, quoted or not. The broker's own tables write a missing value as
  `NA` (the query route's NULL, a template analysis's missing value), as R's `write.csv` does, so no row is a blank
  line; R reads these tables the same way, except that it reads an empty value as empty text in a text column.
- Until a method declares column types, every column's type is inferred from all of its present values, and
  `type_source` says `inferred`. A column is `integer` when every present value is an optional sign and digits;
  `number` when every present value is an integer, a decimal with an optional exponent, `Inf`, `+Inf`, `-Inf` or
  `NaN`; `logical` when every present value is `TRUE` or `FALSE`; otherwise, or with no present value, `string`.
- Sorting uses the type: integers exactly at any length, numbers as doubles with `NaN` after every number and `-0`
  equal to `0`, logicals with `FALSE` first, strings by their bytes. Missing values come last in both orders, and
  equal values keep file order.
- A summary counts present values (`count`) and missing ones, and counts different stored strings (`distinct`, so `1`
  and `1.0` count twice). `min` and `max` are the stored strings that sort first and last, the earliest in the file
  when several are equal.
- `output_sha256` is the SHA-256 of `columns` in RFC 8785 canonical JSON: keys sorted, no whitespace. A client
  recomputes it from the answer, for example in Python with
  `json.dumps(columns, sort_keys=True, separators=(",", ":"), ensure_ascii=False)` encoded as UTF-8.

## Verification and limits

Before a table is parsed, the package's bytes must match the artifact's recorded size and SHA-256, and the member's
bytes must match the digest in the verified listing. A bare table is read once and checked against its record. A
parsed table is kept in memory by digest and delimiter, oldest first out, up to 64 MiB in all by an estimate made at
parse time that counts its column names, values, cell offsets, both orders of every column and its summary. A table
whose estimate alone is over 64 MiB is not kept, so each read of it parses it again. A page or
summary answered from memory comes from bytes that matched their digest when they were read. A stored file changed or
replaced since is found on its next read from disk: a download, a range, or a parse once the table has left memory or
the service has restarted. At most two listings, parses, first sorts or first summaries run at once, each off the
request threads. Page and summary bodies are written off the request threads, four at a time, straight from the
parsed text, and a body past 8 MiB is refused before it is written whole.

A table may hold up to 16 MiB, 1,000,000 rows, 1,000 columns and 2,000,000 cells. A larger one stays downloadable. A
record with more than 1,000 fields is refused when the reader's room for fields fills, before any field is stored.

A `Range` request names one range: `bytes=a-b`, `bytes=a-` or `bytes=-n`, with digits only. The answer is 206 with
`Content-Range`, `Accept-Ranges: bytes` and `x-artifact-sha256`, the digest of the whole member or file, which a client
checks once it has every part. The broker checks the whole package or file before it sends any part and reads the
part from the same open file, so a file replaced after the check is never the one read. A whole member is checked as it
streams, and a member that differs from its digest ends the stream with an error. A start past the end is 416 with
`Content-Range: bytes */size`. A malformed range (another character, a sign, or an end before its start) is ignored
before its start is compared with the size, and several ranges, several `Range` fields or another unit get the whole
content with 200, as RFC 9110 allows.

## Refusals

| Status | Code | When |
|---|---|---|
| 400 | `table_member_required` | a package artifact without `path` |
| 404 | `package_member_not_found` | no such member, or a path that leaves the package |
| 415 | `artifact_not_a_package` | `path` on an artifact that is not a package |
| 415 | `not_a_table` | a member or file that is not CSV or TSV |
| 422 | `table_unreadable` | not UTF-8, no header, or a row whose field count differs; the message names the line |
| 413 | `table_too_large` | past a limit above |
| 413 | `page_too_large` | a page body past 8 MiB; ask for fewer rows |
| 413 | `summary_too_large` | a summary body past 8 MiB, when the stored extremes are long |
| 400 | `sort_column_unknown` | `sort` names no column |
| 400 | `page_order_invalid` | `order` other than `asc` or `desc`, or `order` without `sort` |
| 400 | `page_offset_invalid`, `page_limit_invalid` | out of range |
| 409 | `artifact_integrity_failed` | stored bytes differ from their record |

`src/broker/tables.rs` holds the rules and their unit tests. `scripts/qualify_remote_broker.sh table-reads` runs the
routes against PostgreSQL with synthetic packages, including changed stored bytes and a reader who may not see the task.
