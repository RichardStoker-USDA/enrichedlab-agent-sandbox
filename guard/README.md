# Upload guard

The upload guard checks a file before a research service accepts it. It confirms the file is the type it claims and
that its structure is sound, scans it for malware, and passes on a rebuilt copy with active content removed: a PDF or
image rebuilt in its own format, or a lab's table (CSV, other delimited text, or an Excel workbook) rebuilt as a grid
of the cell values it holds. A file
that needs a person's judgment is held with a plain-words explanation; malware is refused. Every decision is recorded
per person with retention dates. The guard adds evidence and friction around the controls that bound damage
(untrusted content treated as data, isolation, denied egress, human approval of material actions); it is never the
only barrier.

## Layers

| Layer | What it checks | How |
|---|---|---|
| L0 type | the bytes' real type against the declared media type; a second format hidden in the file (data before a PDF header, an archive a reader could open); where a use case allows tables, text that is no other type and a ZIP package that holds a workbook | magic numbers, owned code |
| L0 structure | PDF encryption, repair and readability; active content (JavaScript, launch actions, embedded files, open and additional actions, XFA, rich media, form submission, remote go-to); data streams no page uses; content operators and operands no viewer draws with; bytes after the end marker; PNG chunk checksums and JPEG segments | qpdf, lopdf, owned content lexer, owned code |
| L0 hidden text | PDF glyphs drawn invisibly, in white, under 1 pt or outside the page, including inside forms; layers hidden by default; Unicode tag characters, bidirectional controls and zero-width characters in the text and metadata | owned content-stream interpreter, pdftotext |
| L1 malware | known malware signatures; owned rules for programs, scripts inside images, launch commands, obfuscated document JavaScript, download and reverse-shell commands, office macros and tag-character smuggling, run on the file's bytes, every decoded PDF stream and inline image, the PDF's strings and its action text; archives anywhere a copy would keep them, found by their structure | ClamAV `clamd` (separate Deployment), YARA-X, owned archive recognizer |
| L0 tables | a delimited text's encoding and records; a workbook's parts measured by decompressing them, and its macros, controls, embedded objects, links to other files and data connections, XML declarations, hidden sheets, formulas saved without values, and invisible characters in its cells; every bound a table may reach | owned record reader, `encoding_rs`, `zip`, `calamine` |
| L2 disarm | a rebuilt copy: PDFs decrypted, normalized, stripped of active content, document metadata and every stream no page draws, their content streams written again in canonical form, rewritten and checked, and every page drawn at low resolution within the helper limits; PNG and JPEG decoded within pixel limits and re-encoded without metadata; a table written as its cell grid, whose
text the rules scan before the grid is compressed. The rebuilt copy goes through L1 again, a PDF's streams are scanned again, and its extracted text must equal the original's | qpdf, lopdf, owned content lexer, pdftoppm, `image` |

The PDF stream scan sorts every stream by the use the document makes of it. It reads each page's content with the
guard's own lexer and follows each resource an operation draws with, by its category (an image or form for `Do`, a
font for `Tf`, a graphics state, shading, color space, pattern or property list) and in the resource dictionary of the
content that names it; it reads every form, tiling pattern, Type 3 glyph and annotation appearance it reaches the same
way, through a work queue, until nothing new is reached. A name inside a string, or a name another category uses, does
not make a resource drawn. A walk that passes its budgets (200,000 content streams, 256 MiB of decoded content, 20
million operations) holds the file, so a deep chain of forms can never leave drawn content unread; so does an object
nested deeper than the walks follow (128 levels, above the 100 lopdf parses), a page tree whose inherited attributes
sit more than 128 parents up, a payload entry nested past 16 levels, or a form-field or appearance tree past 128. A stream an
active-content entry reaches (a script, XFA form logic, an attached file, sound, movie or 3D data) is a payload
whatever else names it, and a stream reached by two different drawing uses, or by none, is unknown; the rebuild
removes all of these and the document metadata, then writes every kept content stream again in canonical form: one
operation per line with exactly the operands its operator takes, so no comment, unused operand or stray byte survives.
Each stream and inline image is decoded (Flate, LZW, ASCII85, ASCIIHex, run-length and Brotli; an image codec's input
is scanned as it is, and a filter after a codec makes the stream unscannable) within 256 MiB per stream and 1 GiB per
document, and scanned with the rules, as are the document's strings and names, the strings and names in every content
operand (shown text, marked-content properties, inline image dictionaries), and each action's script and launch text. Commands and macros match in every stream but page text and document metadata, and not in strings, which hold
words a person reads, so a paper that quotes a command in its text, abstract or an annotation is not held for it.
Archives are found by structure, at any offset, in the file and in every stream, inline image and string a copy would
keep: a ZIP end record (classic or ZIP64, however far its record sits in front of its locator) whose directory and
local headers are where it says, a ZIP local entry a streaming reader reads to the end of its data whatever its
version, method or name (the end known from its sizes, its ZIP64 extra field, or a data descriptor whose size reaches
back to the data), or a 7-Zip, xz or RAR header of any size whose own CRC matches; every candidate is checked, within
4,096 candidates and 64 MiB of CRC and descriptor search per input, past which the input counts as an archive. A name,
number, flag or short array of them given by reference (`/S 7 0 R` where object 7 is `/Launch`, or an image's
`/Columns`) is read as the value it points to, through a chain of references of any length, before any of this; any
other chain is shortened to one reference, and one to a missing object reads as null. A chain that returns to itself,
or references that would add more than 64 MiB that way, make the file unreadable. Every image a viewer would decode, inline images included, is bounded by its dictionary and by
what its codec declares, counting the bytes its decoder allocates: JPEG frames (two bytes per sample above 8 bits, two
more per coefficient for a progressive frame; a frame whose height a DNL marker sets, a hierarchical or differential
frame, or a second frame is refused), JPEG 2000 (four bytes per sample, the image header and codestream both read),
CCITT (the larger of the image size and the decode parameters' columns and rows) and JBIG2 (every page, region and
pattern dictionary bitmap its segments and global segments declare; an unknown segment length, a striped page with no
end, or any symbol dictionary, whose symbol sizes only its decoder knows, is refused). Any kept stream that declares
itself an image is bounded the same way, whatever use reached it. At most 30,000 pixels a side, 512 MiB per image
and 4 GiB for all of a document's images, since the corpus decodes them later. The rebuilt copy is then drawn by
pdftoppm under the helper limits, every page into a 16-pixel square written to the helper's bounded output and
discarded, never to disk; each image a page draws is still decoded at its full size, so a bomb, a codec variant whose
size its headers hide, or a decoder fault ends the helper and holds the file (`render_failed`). A stream the scan cannot decode, an inline image without an end, an image header
it cannot read, an image past the bounds, and an archive are findings the library policy holds.

## Lab datasets

The use case `lab-dataset` takes the tables a lab keeps: comma-, tab-, semicolon- or pipe-separated text in UTF-8,
UTF-16 (after its mark) or Windows-1252, and Excel workbooks in the Office Open XML format (.xlsx and .xlsm). Text
that is no other type is a table; any control character other than a tab or a line break makes it unreadable. A ZIP
package is read as a workbook, and one with no spreadsheet in it is refused as an archive. An Excel 97-2003 file or a
password-protected workbook is an OLE file, blocked with its type named (`type_not_allowed`) so the person can save it
again as .xlsx or CSV. A table may arrive under any table's name: a text table sent as a workbook is read as text, since spreadsheet
programs and instruments save one under the other's name.

The worker (`dataset-grid`) reads the table and writes the rebuilt copy, `enrichedlab.cell-grid.v1`: gzip-compressed
JSON lines, first the source (type, SHA-256, size, encoding, delimiter) and each sheet's name, size, hidden flag,
formula counts and merged ranges, then each sheet's rows in order, each row where a spreadsheet program shows it. A
cell is null, text, a number, a boolean, or a tagged date, date and time, time, duration, error or out-of-range
serial; the module comment in `src/dataset.rs` is the format's definition. A text table's fields are kept exactly as
written, so `<0.5`, `ND` or `1,5` reach the lab's own cleaning step unchanged. A workbook's cells are the values it
held when it was saved: no formula runs, and a formula saved without a value is empty. The copy carries nothing that
runs, embeds or reaches out, so macros, controls and embedded objects are disarmed and links to other files are
annotated; a workbook that declares XML entities is held. The worker measures every part of a workbook by
decompressing it, within 256 MiB a part and 1 GiB in all, and refuses a part that holds more than its directory
claims. A table is refused whole (`table_too_large`) past 64 sheets, Excel's 1,048,576 rows or 16,384 columns, 4
million filled cells, 32,767 characters in a cell, 256 MiB of grid text, or a compressed grid larger than the guard
keeps for its upload; the file limit is 32 MiB. A reader never sizes its room from a count the file declares before
the worker checks it: the ZIP end record the reader takes first, and any ZIP64 record it points to, must name at most
10,000 parts (the reader's own checks hold another record to 65,535 entries, or one for each 46 bytes before a ZIP64
record), and every number in the shared-texts element, whose `uniqueCount` the workbook reader reserves room for, must
be within the 4 million cell limit; the element is found with the workbook reader's own XML reader and settings. A
row's texts count against the grid's room while the row is built, the grid is written only as far as its room, and its
first line counts toward the 256 MiB. The rules scan the cell text as a file's own bytes (`guard_type`
`delimited` or `xlsx`), and invisible Unicode in cells and sheet names is counted as in a PDF's text.

Policy is a versioned JSON record of use cases (`policy/policy.json` holds the research library's papers and the
labs' datasets): allowed types, size limit, daily quota per person, retention, a threshold for hidden-text counts, and an action per finding:
`allow`, `annotate`, `disarm`, `quarantine` or `block`. A finding the policy does not name takes the default action,
which must quarantine or block. A malware signature always blocks, and a file the guard could not check, scan or
rebuild is always held.

An operator may record an exception for a held file: its SHA-256, the use case, who approved it, why, and an expiry
within 90 days. The exception releases only a hold found in the original (L0 and L1). The file is still rebuilt and
the rebuilt copy still checked; a failed rebuild, or a rebuild finding that holds or blocks, keeps the file held, and
what passes is always the rebuilt copy, never the original. A blocked file cannot be excepted.

## Processes

The service never parses an uploaded file itself. qpdf and pdftotext read PDFs, and the guard's own binary, started
as `enrichedlab-guard worker TASK`, reads PDF objects (`pdf-inspect`, `pdf-text`, `pdf-strip`), rebuilds images
(`raster`) and reads and rebuilds tables (`dataset-grid`); the service itself only decodes text to recognize a
table. Each runs as a helper process with an address-space limit, a CPU limit, a wall-clock timeout, a file-size
limit (four times the upload, at least 64 MiB), a cap on its output, its own process group and no environment, so a
hostile file can exhaust only its helper. The service reads a helper's rebuilt copy only when it is within the size the
guard keeps. A worker that does
not finish holds the file (`analysis_failed`), except the hidden-text count, which then reports itself partial.

## Interface

Callers, fetchers and operators authenticate with a bearer token; the guard stores only SHA-256 digests of tokens,
and no two principals may share one. A caller sends files for its use cases. A fetcher sends nothing: it reads the
records and rebuilt copies of the files named callers sent for named use cases, so a service's worker can take a copy
its API received without holding the API's token.

- `POST /v1/intake`: the file is the body. Headers: `content-type` (the declared type), `x-guard-use-case`,
  `x-guard-tenant`, `x-guard-lab`, optional `x-guard-project`, `x-guard-person` (an opaque identifier, stored as a
  digest) and optional `x-guard-file-name` (percent-encoded). The guard reads the body only after the caller is
  authenticated and a permit is free, and stops reading at the use case's size limit, so at most `max_concurrent`
  bodies are in memory. The reply gives the intake ID, the outcome (`allowed`, `quarantined` or `blocked`), each
  finding with its explanation, and for an allowed file the rebuilt copy's digest and expiry. The same bytes from the
  same person within an hour return the earlier decision, unless an exception has since released a held file.
- Refusals before evaluation are logged and counted but not recorded: `413 too_large`, `429 busy`, `429
  quota_exceeded`, `507 store_full`. A refusal after evaluation started, `503 scanner_unavailable` (the scanner is
  unreachable or its signatures are older than the configured age; the file is not accepted unscanned) or `500
  evaluation_failed`, is recorded and counts against the person's quota.
- `GET /v1/intake/{id}/disarmed`: the rebuilt copy, to the caller that sent the file or a fetcher for it, until it
  expires. Its media type is the file's own, or `application/vnd.enrichedlab.cell-grid+gzip` for a table, and
  `x-guard-sha256` carries its digest.
- `GET /v1/intake/{id}`: the decision record, to the same readers and to operators.
- `POST /v1/exceptions`: operators only.
- A second listener serves `/healthz`, `/readyz` (scanner reachable, signatures fresh, store writable) and `/metrics`.

`enrichedlab-guard check --policy FILE --use-case NAME --rules DIR [--scanner HOST:PORT] FILE` evaluates one local
file and prints the decision; without `--scanner` it skips ClamAV and says so.

## Storage

Under the configured root, each person's scope keeps the originals and an index; records, rebuilt copies and
exceptions sit beside them, every file owner-only. The store counts every byte it writes against its cap. Before an
intake is evaluated it reserves, under one lock, the person's quota (their index and their other intakes in flight)
and the store's room for the original, the largest rebuilt copy it will keep (twice the original, or the original
and 64 KiB), the record and the work directory (the original and three helper files at their limit), so concurrent
intakes cannot all pass the same check. The work directory removes itself when the intake ends, a cancelled request
included. A write outside a reservation (a retry's record, an exception) is checked against the cap under the same
lock and refused with `507 store_full` when it does not fit. A record keeps its 16 most recent
retries and a count of all of them. An hourly sweep deletes what is past its date, drops index lines older than two
days, removes expired exceptions and empty scopes. Log lines carry metadata only: codes, counts, sizes, engine and
policy versions, timings; never file content or names.

## Build and test

`images/guard.Containerfile` runs the tests on the build machine's own architecture with qpdf, pdftotext and pdftoppm
present, then builds the release binary for the target from the same locked sources and assembles a Wolfi runtime with
those programs and ClamAV. `images/guard-runtime-amd64.lock` names every package in the runtime by name, version and
APK checksum; the build installs exactly those and fails if the installed set differs in any package, so a later
rebuild of the same commit gets the same packages or no image. The `runtime-lock` target regenerates the lock when a
package or the base changes.

The `runtime-check` target is the release image's layers plus two: the same tests compiled for the target, with the
files they read, and the script that runs them, with the image's own binary as the service tests' worker. A release
checks that its layers start with the release image's layers, runs it under gVisor on a node of the target's
architecture (the helpers' address-space limit cannot be tested under an instruction emulator), and relays the image
for promotion only after that run passed. The release transaction scans the
image by digest and attaches the signed result to it; the registry refuses any release image with a HIGH or CRITICAL
finding. Debian 13 had unfixed ones in the libraries these programs need, which is why the runtime is Wolfi.

Locally, `cargo test` skips the tests that need qpdf unless `GUARD_REQUIRE_TOOLS=1` is set. The dataset tests
(`tests/datasets.rs`) need no PDF helper and always run.
