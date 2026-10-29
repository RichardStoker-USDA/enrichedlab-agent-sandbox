# Result kinds

A result kind says what a stored file holds, so a reader can choose a view without trusting the file name. The broker
carries one registry, checks each researcher-visible artifact's bytes against it, and records the outcome with a
receipt. A file that fails its kind's check, or that no accepted kind claims, is `file.opaque`: it is kept, listed and
downloadable, and a new kind is never a reason to refuse a result.

## The registry

Each kind is one file, `kinds/<kind id>.yaml`, built into the broker binary. The broker refuses to start when the
registry does not load. A kind names:

| Field | Meaning |
|---|---|
| `id`, `version` | dotted lower-case words, and a version that starts at 1 |
| `status` | `candidate`, `accepted` or `retired`; only accepted kinds are recognized |
| `owner`, `summary` | who maintains it, and one line on what it holds |
| `recognition` | `media-type` (the default) or `declared`; see "Declared kinds" |
| `checked_as` | a declared kind only: the kinds that check its files, exactly one for each of its media types |
| `media_types` | the media types it claims; two accepted kinds recognized by media type never claim the same one |
| `validator` | the reviewed check that reads the bytes; `delegated` for a declared kind |
| `access` | `whole`, `range`, `paged` or `live`: how a reader fetches it |
| `limits` | `bytes`, plus the validator's own limits (`rows`, `columns`, `cells`, `line_bytes`) |
| `views` | each view's name, its GPU preference ending in `still` and its fallback; optionally the media types it draws (`media_types`, a subset of the kind's) and the channels a declaration must map to columns before it can draw (`encoding`, at most eight, such as `x`, `y`, `z`) |
| `fixtures` | small synthetic files and whether each must pass or fail; a declared kind has none, since the kinds it is checked as hold them |

The registry digest is `sha256:` and the SHA-256 of the kinds' public fields in RFC 8785 canonical JSON. A change to
any kind changes the digest. Fixtures are checked by the unit tests, in one-byte and larger chunks.

Kinds in version 1 of the registry:

| Kind | Media types | Check |
|---|---|---|
| `table.delimited` | `text/csv`, `text/tab-separated-values` | the table rules of [result tables](result_tables.md) and the kind's row, column and cell limits |
| `image.raster` | PNG, JPEG, GIF, WebP | the format's signature and its ending (PNG `IEND`, JPEG end-of-image, GIF trailer, WebP length) |
| `image.svg` | `image/svg+xml` | UTF-8, and the first element is `svg` |
| `document.pdf` | `application/pdf` | a PDF header and an end-of-file marker in the last 2,048 bytes |
| `record.json` | `application/json` | one JSON document |
| `record.ndjson` | `application/x-ndjson` | one JSON document on each non-blank line |
| `text.plain` | `text/plain`, `text/markdown` | UTF-8 without NUL bytes |
| `package.result` | `application/gzip` | one gzip member that decodes to its checksum and length, and a package listing the broker can check |
| `file.opaque` | none | none; every file can be this |
| `figure.r` (declared) | SVG, PDF, PNG | the kind that owns the media type: `image.svg`, `document.pdf` or `image.raster` |

A kind is recognized from the artifact's recorded media type and accepted only when its check passes. SVG is an
untrusted document: a view draws it in a sandboxed frame with no scripts and no network.

`table.delimited` offers two views: `table`, and `surface` (WebGPU, then WebGL 2, then a still image) with the
encoding `x`, `y`, `z`. A view with an encoding is offered only for a file whose method's output binding maps those
channels to columns.

## Declared kinds

A declared kind names what a file is when its media type cannot say it, such as an R figure, which is an SVG, PDF or
PNG like any other. No media type, file name or file content makes a file a declared kind. Only the code that wrote
the file declares it, bound to the file's path and SHA-256: broker code, or a reviewed method's output binding. The
file is still checked by the kind that owns its media type, and `validation.checked_as` names that kind. A file takes
the declared kind only when that check passed, the declared kind lists that checking kind in `checked_as`, and it
takes that media type; otherwise it keeps the kind its check gave it.

`figure.r` is the first declared kind. The broker's figure path packages each drawing it gets from the
reviewed R render service and records a receipt that lists every drawing's path and SHA-256 with the package's
SHA-256. When a person lists that package, each member that a completed receipt for that package names, with the same
digest, takes `figure.r`. A copy of the drawing in any other package, a drawing whose digest differs from the receipt,
and a file the receipt does not list keep their checked kinds. A TIFF export has no kind with a validator and stays
`file.opaque`. Its views are those of the checking kinds, each limited to its media type.

## Recording

The tenant's leader worker checks researcher-visible artifacts that the registry has not checked, newest first, 16 at a
time, every 15 seconds when idle. It reads each stored file once, computes its SHA-256 while it checks it, and records:

- the kind the bytes hold (`file.opaque` when the check failed) and the kind it checked them against;
- `passed` or `failed`, with the reason when failed;
- the receipt: `{"routine": "enrichedlab.kind-check", "version": 1, "registry", "checked_as": {"id", "version"},
  "media_type", "input_sha256", "size", "checks", "result", "reason"}` in RFC 8785 canonical JSON, and its SHA-256.

A record is bound to the artifact's bytes, its recorded media type and the registry digest, and it never changes. The
media type chooses the kind and how the bytes are read (the same bytes as CSV and as TSV can differ), so a
registration that changes it leaves the artifact `not_checked` until the worker checks it as the new type. The
database refuses a record for an artifact that no longer holds the bytes and media type the worker checked, a record
whose receipt disagrees with its columns, and a second, different record for the same bytes, media type and registry.

JSON and NDJSON checks read the whole document as UTF-8 before they parse it, since a parser that skips values does
not check its strings. Nesting depth has no limit beyond the kind's byte limit: the parser walks a document without
recursion.

A file that cannot be read, or whose bytes differ from the artifact's record, is a custody fault, not a kind: nothing
is recorded, the worker logs `artifact_kind_unchecked` with the reason, and it tries that artifact again after ten
minutes. Such an artifact stays `not_checked`.

## Reading

| Method | Path | Answer |
|---|---|---|
| GET | `/api/v1/kinds` | the registry: `{"registry": {"digest", "version"}, "kinds": [...]}`, with the digest as the `ETag` |
| GET | `/api/v1/tasks/{task}/artifacts` | each artifact with its kind fields, and `registry`, the digest they refer to |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/kind` | one artifact's kind fields with the full receipt and `recorded_at` |
| GET | `/api/v1/tasks/{task}/artifacts/{artifact}/contents` | each package member with its kind fields, and `kind_check` |

`GET /api/v1/kinds` answers 304 to a matching `If-None-Match` and sends `Cache-Control: private, no-cache`.

An artifact item gains:

```json
{
  "kind": {"id": "table.delimited", "version": 1},
  "validation": {
    "state": "passed",
    "receipt": "sha256:<receipt digest>",
    "checked_as": {"id": "table.delimited", "version": 1},
    "reason": null
  },
  "same_values_group": null,
  "views": []
}
```

`state` is `passed`, `failed` or `not_checked`. A failed check has kind `file.opaque`, `checked_as` naming the kind
it failed, and the reason. Before the current registry has checked an artifact, `kind`, `receipt`, `checked_as` and
`reason` are null. A client checks a receipt by computing the SHA-256 of the `/kind` answer's `receipt` in RFC 8785
canonical JSON. The broker serves a receipt only when its stored text is already in that form; any other answers 500
`artifact_kind_receipt_invalid`. The artifact list and the kind records are separate reads, so a record is attached
to an item only when it was made for the item's bytes and media type.

An image build runs `agent-sandbox-broker check-kinds`, which loads the registry built into the binary and prints its
digest, so a registry the broker would refuse at start fails the build.

`same_values_group` and `views` are what a method declares in its output binding: which files hold the same numbers,
and which columns a view draws. No method declares them yet, so they are null and empty, and a reader offers the
kind's own views from the registry.

A package member in the `contents` listing gains `result_kind`, `same_values_group`, `views` and `validation`
(`state`, `checked_as`, `reason`); `result_kind` is a declared kind where its writer declared one. The member's own `sha256` is the checked input, and the listing's `kind_check`
names the routine, its version and the registry once for all members. Members are checked in the same pass that
hashes them, as `inside a package`: a package inside a package is not read and stays a download.

A reader is anyone who may read the task; anyone else gets 404.

## Checks

`src/broker/kinds.rs` holds the registry rules, the checks and their unit tests. `src/broker/kind_record.rs` is the
worker's recorder and `migrations/postgres/055_result_kinds.sql` the record. `scripts/qualify_remote_broker.sh
result-kinds` runs the recorder and the routes against PostgreSQL with synthetic files, including a custody fault,
refused conflicting and edited records, member kinds, and a person who may not read the task. `figure-kinds` runs a
figure version's declaration through the `contents` route: the drawings it lists take `figure.r`, and another digest,
another package, a refused attempt and an unlisted file do not.
