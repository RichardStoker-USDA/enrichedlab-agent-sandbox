# Answer documents

A person who may add to a project can take a research run's answer away as a Word document, an Excel workbook or
PowerPoint slides. Code writes each one from the stored decision the run ended on and the result tables that decision
cites; no model is called and no value is retyped. The broker's own writer makes the file, with no template file and
no other program. Each document is its own task with the request, the writer's receipt and a package.

## The method

The document method is a profile on route `server-document`. A lab needs exactly one enabled one granted to it
(`document_method_unavailable` otherwise). Like the figure method it runs only from a request: the profile list marks
it `offered: false`, a person's direct submission of it is refused (`profile_forbidden`), and a research run may not
list it (migration 072 adds the route to `broker_request_only_route` and `broker_run_excluded_route`).

The worker writes documents itself; it needs no render service for them. A worker refuses to run without migration
072 and its grants, the completing function's among them (`broker_document_schema_not_ready`).

## A request

`POST /api/v1/projects/{project}/documents` with `run_id`, `kind` (`xlsx`, `docx` or `pptx`) and `request_id` asks for
one. `broker_request_document` checks, in order:

1. The person may add to the project (`project_not_found` otherwise) and the run is the project's
   (`document_run_unknown`).
2. The run stopped on its answer: its status is `review_required` or `paused`, and its newest turn's outcome is the
   same with a `proposal` (`document_run_not_ended`).
3. Every result a finding binds to and every answer table names is a result package of one of this project's tasks,
   with a summarized digest that lists the table (`document_source_unknown`); at most 32 tables
   (`document_sources_too_many`).
4. The lab has the document method.

The request pins the decision's turn and the SHA-256 of its stored record, and for each cited table its artifact,
package digest, table digest, rows and columns. It also pins up to eight PNG figures that the cited packages hold,
with their digests. A request whose pinned content matches one already asked for returns that task unless it failed
or was cancelled, so asking twice writes one document (a transaction lock makes two requests at once agree). A
request id names one task for good: `broker_document_request_keys` binds every id to the task it returned, including
an id the dedupe answered with an earlier task, so a retry returns that task whatever the run holds by then, and the
same id for another run, kind or person is an idempotency conflict. The task's objective names the kind and the turn,
and its origin is the person.

A document is not an analysis: the lab's project list and its review standing, the inbox and a run's parallel slots
leave it out, as they leave out figure versions and lab datasets (072 replaces `broker_list_projects`, `broker_inbox`
and `broker_commit_research_turn` with that one change each).

## The worker's route

`document_run.rs` reads the request under its lease (`broker_task_document`), which re-checks the record's digest and
returns each source with its package's place. A source whose artifact no longer holds the pinned bytes drops out, and
the route refuses on the count (`document_source_changed`). It reads every pinned table and figure, hashing and reading
each package once for all the members it holds, checks each member's digest and row count, and parses each table with
the workspace's reader (`tables.rs`). The tables one document reads hold at most 48 MiB, 2 million rows and 4 million
cells in all, which keeps their parsed form near 100 MB of the worker's memory whatever their shape; the packages it
hashes hold at most 1 GiB as stored, and it decompresses at most 1 GiB of their streams to reach its members, every tar
header, extension header, padding and skipped body counted as it is read (`document_sources_too_large` for any of
these). A package may expand to 2 GiB and a request may pin 32, so without the last two one document could hold the
worker's one execution slot for many minutes.

The reading and the writing run off the async runtime while the route renews the lease every third of its length and
checks for a cancellation every two seconds. When the lease is lost or the task cancelled, the route sets a flag the
reader checks on every read, so the hashing or expanding stops at its next read, and waits for the work to end before it
gives the task up, so a cancelled document never leaves work running beside the next one.

`document_compose.rs` then writes the request body:

- **Question**: the goal of the checkpoint the decision was made on.
- **Answer**: the agent's reading, its summary (the turn commit already refused a summary whose numbers no shown cell
  holds) and the sentence that no person has accepted the answer yet.
- **Findings**: each sentence with every placeholder replaced by the cell it binds, as the digest showed it (control
  characters as spaces, trimmed), and a table naming each value's file, row and column.
- **Parts of the question**, then **each answer table** whole from its retained file with the columns the agent
  chose, then **each figure**, then **Sources** with every digest.
- **Provenance**: the project, run, turn, both digests, and what was shortened to fit.

The record must have the shape the coordinator admits: at most four answer tables of at most six distinct columns
(`document_record_invalid` otherwise). A table repeated is shown once, and a column named twice in a table is shown
once, each with a note. Text these files cannot hold (C0 and C1 controls other than tab and line feed, U+FFFE, U+FFFF)
is sent as spaces or U+FFFD, with a note; `spec.json` and the CSV keep the stored text. A workbook cell is a number only
when the parsed double prints back as the same text, so a spelling past the double's range stays text. A figure that is
not a still PNG the writer takes (every pixel defined and shown as stored, decoded as the writer decodes it) is left out
with a note, and so is a figure past the 12 MiB or 100 million pixels the figures placed may hold in all; each distinct
figure is decoded once.

The writer's limits decide the shape. A deck puts one kind of content on each slide and packs it by the writer's own
estimate of what a slide shows (below): a table's rows join a slide while the table fits the slide's body, at most 15
to a slide and three slides to an answer table; a row too tall for a slide alone has its longest text cells cut until
it fits, and a column name past 48 characters is cut; paragraphs join a slide while they fit its paragraph box, and a
paragraph too tall for one slide continues on the next; a title, heading or caption too long for its box is shortened.
Numbers are never cut. A Word document holds 20,000 table cells in all; a workbook types a cell as a number only when
its text is the number's own plain spelling (at most 15 significant digits, no exponent, no leading or trailing
zeros), so the cell keeps the same digits. Anything shortened is said in the document's provenance, in the receipt's
notes and in `spec.json`. Notes are gathered by kind, one note for each kind naming every finding, table or figure it
covers (`document_compose_notes.rs`), so there are at most 20 and the receipt and the provenance keep every one
whole. The same request and bytes always give the same body.

Writing, the package and its digest run off the async runtime. The route checks again that its lease holds and no
one cancelled, writes the package and its checksum under the attempt's folder, and then one transaction,
`broker_complete_document`, registers both files, records the package digest and the completed receipt and finishes
the task, under the whole fence: the lease owner, attempt and generation, a running task whose desired status is
still `run`, and an unexpired lease. A cancellation or a lost lease before it leaves nothing registered and no
receipt, and the route removes the package that transaction refused (any other error leaves it, since the
transaction may have committed); one after it finds a finished task. A completed receipt can be recorded no other way
(`broker_record_document_receipt` takes refusals and failures only).

## The writer

`document_write.rs` reads the body (unknown fields refused), checks it against the same limits compose writes within,
and writes the file. Its parts are its own small XML, written by `document_write_docx.rs`, `document_write_xlsx.rs` and
`document_write_pptx.rs` (with the theme, master and blank layout in `document_write_pptx_parts.rs`); nothing in a file
comes from a template file or another program, so nothing reaches it that the body did not hold. There are no macros,
fields, formulas, hyperlinks or external relationships.

- **Word**: US Letter, one-inch margins. The title in the Title style with a short Aggie Gold rule under it, each
  section's heading (Heading 1) and its paragraphs, table or figure, each table and figure followed by its caption and
  reference, then a Provenance heading and one `label: value` line per entry. Tables use a Result Table style: an
  Aggie Blue header row of white bold text that repeats on each page, light rules, banded rows, and fixed column widths
  in proportion to each column's longest text (8 to 30 characters). Rows may break across pages, so Word never cuts
  off a row taller than a page.
- **Excel**: one sheet per section, then a Provenance sheet. A table's sheet starts with its header row (frozen) and
  its rows from A1, so the data can be sorted and filtered as it stands; numbers and truth values are typed cells;
  text is an inline string, never a formula, with a literal `_xHHHH_` escaped as Excel requires. Sheet names follow
  Excel's rules (no `[]:*?/\`, 31 UTF-16 units, unique without regard to case).
- **PowerPoint**: 16:9 slides. A title slide, then one slide per section with its heading as the title over the gold
  rule and its table, figure or paragraphs, then the provenance in two columns on as many slides as it needs; every
  slide but the first carries the document's title (shortened to the footer's one line) and its number in a grey
  footer. Each table row is as tall as the estimate of its tallest cell.

PowerPoint lays text out when it shows a slide and a file cannot ask it how much room a text takes, so
`document_write_fit.rs` holds every box a slide has with its type size and estimates from above what fits: it wraps a
paragraph at spaces and tabs as PowerPoint does (spaces and tabs that start a line take room and wrap, spaces at a break
hang past it, tabs come along to the next line), breaks a word wider than the line between characters, takes text
holding a character or tab wider than the whole box as not fitting at all, and takes each character at least as wide as
Calibri draws its kind in any face (measured from the fonts Office ships; the test pins those widths), one em or more
for other scripts, four for the blocks with wider glyphs, and a tab as a whole inch. A line is 1.25 em tall (Calibri's
is 1.2207), or 2 em when another font draws one of its characters. The writer refuses a deck body whose title, heading,
paragraphs, caption or provenance entry the estimate does not fit (`slide_content_invalid`), or whose table it does not
fit (`table_too_large_for_slide`). A font PowerPoint substitutes with taller lines or wider glyphs than those can still
take more room than the estimate gives it.

Colours are UC Davis Aggie Blue (#022851) for headings and header rows, Aggie Gold (#FFBF00) for the title rule only,
near-black body text and grey captions; the font is Calibri. Each figure is the cited PNG byte for byte, after
`document_write_png.rs` checks that it is one well-formed still image: the signature, `IHDR` first, every CRC, a palette
where the colour type needs one and none where it forbids one, one run of `IDAT` chunks, `IEND` last with nothing after
it, and only ancillary chunks that leave every pixel's colour as stored (`tRNS`, `bKGD`, `pHYs` and `sRGB` checked for
their length, place and values, `tIME` for its length only, and the text chunks `tEXt`, `zTXt` and `iTXt` with their
bodies unread), so animation, colour management (`gAMA`, `cHRM`, `iCCP`, `sBIT`) and unknown chunks are refused; at
most 25 million pixels; and its pixel data decoded as a stream to exactly the image's
rows, each with a valid filter byte, the zlib stream and its checksum ending with the data. An indexed image's rows are
reconstructed from their filters and every pixel's index must name a palette entry. The R service's figures (ragg: RGB
or RGBA with `pHYs` and `bKGD`) pass. Identical figures share one media part and one check, and a document's distinct
figures hold at most 100 million pixels. The ZIP is deflated with no extra fields, comments or data descriptors, its
parts in a fixed order, and every entry dated the body's `generated_at`, so the same body always gives the same bytes.

The receipt's version is `writer.<n>`; a release build compiles in the source revision it builds
(`ENRICHEDLAB_SOURCE_REVISION` in `images/broker.Containerfile`), giving `writer.<n>+<revision>`, which lineage takes
as the document's pin. `agent-sandbox-broker write-document < request.json > document.<kind>` runs the same writer on
a package's `request.json`.

## The package

A completed document keeps one gzip-compressed tar: `document.<kind>`, `request.json` (the exact body the writer read),
`receipt.json` (the kind, the body's and the file's SHA-256, the size, the counts and the writer's version),
`tables/answer-table-<n>.csv` (each answer table with every row and the stored text), `spec.json` (each finding whole,
each finding value with its cell, each source and figure by digest, the counts and notes), a README and `SHA256SUMS`. In
the CSV copies a cell that begins with `=`, `+`, `-`, `@` or a tab and is not a number, or begins with `sep=`, starts
with an apostrophe, so a spreadsheet opens it as text (Excel takes a first line beginning `sep=` as its separator, which
would make those apostrophes separators); a table whose copy would pass 24 MiB (48 MiB for all) is left to its result
package, with a note. The cited tables and figures are named by digest, not copied. The same broker release writes the
same file from `request.json`.

## Readers and lineage

`GET /api/v1/tasks/{task}/document` returns a document's request and every receipt. `GET
/api/v1/research-runs/{run}/documents` lists the newest 50 documents made from a run that the reader may open. The
project read marks a document task with `document` (kind, run, turn), and the workspace keeps those apart from the
analyses. Lineage names the document's run, turn, record and sources; its tool class is `vetted_service`, its outputs
are `document_output`, its input class is `prior_output`, and its review state is `unvalidated` until a person accepts
it. A completed document whose receipt does not name the writer's source revision shows the `image_not_digest_pinned`
gap.

## Tests

`src/broker/document_write_tests.rs` checks each kind's file the way a reader resolves it (every part well-formed, every
content type and relationship target present, no external link, field or formula), the same bytes for the same body, the
ZIP's form and dates, escaping, typed workbook cells and the `_xHHHH_` escape, figures byte for byte and shared, limits
that refuse rather than cut, slide content past the estimate, row heights, provenance slides, sheet names and column
widths. `document_write_png_tests.rs` covers each colour type, depth and interlace, chunk faults, the ancillary chunks
allowed and refused, palette indexes read after each filter, and pixel data that does not decode to the image's rows;
`document_write_fit_tests.rs` pins Calibri's measured widths under the estimate's and covers wrapping, the reviews'
probes and each box. `src/broker/document_compose_tests.rs` covers splitting and cutting text, number typing, filling
findings, the section order and counts, deck slides, figures and the one left out, text the files cannot hold, the notes
(every kind at its longest fits the receipt), the CSV marking (with the `sep=` probe) and bounds, and the admitted table
shape; `document_compose_slides_tests.rs` covers a paragraph continued on the next slide, shortened headings, captions
and column names, a deck of tall text the writer takes, and the figures' pixel budget. `figure_run.rs` tests the
reader's budget (charged by the decompressed stream, extension headers included) and stop flag, and `document_run.rs`
the removal of a refused package. `tests/broker_answer_documents.rs` (`scripts/qualify_remote_broker.sh
answer-documents`) runs requests against PostgreSQL from refusals to the package for each kind: request-id binding and
conflicts, a cancellation while the worker writes (nothing registered, no completed receipt without its package),
readers, lineage, and the counts and inbox that leave a document out. Office's own reading was checked by hand: Word,
Excel and PowerPoint open each kind without a repair prompt; a deck at the edge of what the slide estimate accepts
(`tests/fixtures/documents/slide_limits.py`) shows every text whole in PowerPoint, Latin, CJK, Cyrillic, symbols and
dingbats alike; and Excel opens a CSV copy whose first column is named `sep='` as text, where the same file without the
apostrophes evaluates its formula. `tests/fixtures/documents/README.md` says how to repeat these.