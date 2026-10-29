# Research workspace

A Svelte 5 browser client for the authenticated Agent Sandbox broker. Scientists open research projects, read the
actual R figures and tables their analyses retained, see how each result was made, follow the project's history,
start or pause the research agent, share access, and download a verified evidence package. Administrators can
inspect a project for a stated reason. Broker lifecycle stays separate from research evidence.

## Run the synthetic preview

Node.js 24 or newer and npm are sufficient. The preview has no model, container runtime, broker credential, or
external endpoint.

```sh
npm ci --ignore-scripts
npm run preview:fixtures
```

The loopback preview is labelled as simulated data. Its fixture API (`fixtures/`) holds seven projects in two labs, four
research runs (one with rejected model replies), an administrator directory, 240 recorded tool steps, one retained analysis package
from a fixed method and one from model-written code. `fixtures/long_run.ts` adds a long run: thirty planned steps,
27 analyses read ten at a time through the analyses cursor and 271 retained files, with one step that did not finish,
one to check and one that ran twice, all from a seeded generator; `POST /api/v1/fixture/long-run-advances` lets its
model finish and its next step wait for a compute slot while a page follows it, and a second call gives that step its
slot. `POST /api/v1/fixture/query` turns the model-written analysis
into a database query with its own simulated package (`?on=false` turns it back). `POST
/api/v1/fixture/sensor-run-stops` stops the sensor project's work session for review while a page follows it. The
preview serves the change stream by reading every watched record through its own routes after each change and on a short
timer (`fixtures/changes.ts`), answers reads with weak ETags, and offers the nutrition lab's three starting points as one
set or several (`fixtures/starts.ts`); `POST /api/v1/fixture/changes?on=false`, `/etags?on=false` and
`/combine?on=false` play a broker without each, `/changes-renew` and `/changes-drop` end the open streams, and
`GET /api/v1/fixture/reads` counts full and 304 answers. Every value is simulated. `POST /api/v1/fixture/reset` restores the starting state; it exists only in the
preview, which never runs an agent. The production build has no fixture adapter or model client.

To inspect the step-by-step lineage fixture, open
`#/projects/50000000-0000-4000-8000-000000000001/analyses/10000000-0000-4000-8000-000000000005`
and choose **How it was made**. Its synthetic record follows a figure through a reviewed analysis and a guarded
FoodData Central query to the recorded release file, and reports one unreadable upstream task without exposing it.

The fixed method's figures and tables are real R output from simulated inputs. `fixtures/r/simulate_inputs.R` writes the
inputs and `fixtures/r/analysis.R` writes the box plot, volcano plot, two-page model checks and tables in base R, the
same way reviewed analysis tools do. To regenerate them, run both scripts with an R that has Cairo SVG support, for
example `pixi exec --spec "r-base>=4.5" -- Rscript simulate_inputs.R input` and then
`... Rscript analysis.R input results`, and copy `input/` and `results/` over the files in `fixtures/r/`. The
model-written analysis in `fixtures/r/generated/` is the output of a reviewed R template on the same
simulated measurements, run in the pinned R image with no network; its README says how to regenerate it.
The long-run fixture's season figure is the 3D companion example: its retained R PDF and SVG share an explicitly
mapped three-numeric-column table. The fixture mapping names the axes, labels and units; the browser does not choose
columns or make values for it.

## What the interface does

The design direction is "Glass Box", on "Quiet Instrument" and the "Working paper" figure, table and
provenance rules, all recorded in `design_direction.md`. The bar names the reader's lab, with the organization the site
sets (the broker's `--site-organization`) beneath it; a person in several labs changes lab from the lab name, whose menu
lists every lab with its project count and what waits for the reader there, and the choice is kept in the browser. The
lab in view is where the reader is: home, Waiting for you, Working now, their counts, search and where a tab lands show
that lab's items only, another lab's appear only as counts in the lab menu and a mark on the lab name, and the index's
lab filter widens the list to other labs on purpose. A notice for an agent that stopped in another
lab says so and opens that lab. The bar also holds New question, search
(Command K: places, actions, and projects by title and question), the Simple and Detailed switch, Display and the
account. An inset sidebar holds the places: Your research, Waiting for you and Working now with their counts, Results,
and Library with the commons and shared notes under it; below 993 pixels it opens as a drawer from the bar, with New
question at its top. Pages inside a place carry crumbs at the top of the page column.

Where the workspace opens is one rule (`src/landing.ts`): a tab's first visit to the bare address opens what needs the
reader, then their project that finished since they last looked, then their project that is working, and says why;
otherwise home. The broker's last look decides "since they last looked" when it reports one, else this browser's own
record. Home asks first: the question field with the lab it will run in and what the agent can work from there; Continue
opens a setup sheet (title drawn from the question, lab, data and methods, who can open it, and under More options the
methods and limits), and Start creates the project, saves the question and starts the agent, each request with its own
idempotency key. Where the broker combines a lab's data and method sets (`combine` on its starting points), the sheet
offers one, several or every set, for a question across all the lab holds, says what the chosen sets come to, and offers
the methods of each; the project then names every set it started from. An older broker takes one set, as before. A
project saved without its question gets the same choice on its page. Below the field sit Needs you (each item with its action), Working now (with Pause), Recently finished
and All projects: who started them is a segmented control, a search field narrows them, and a Filter button opens the
lab and standing filters (a sheet on phones). On phones a compact ask bar docks at the bottom once the field scrolls
away. Working now lists every project an agent works on. Results is a table of analyses filtered by where each stands
and sorted by status, request time or duration. The account menu names the reader, the address once and the role in
the current lab, with one quiet line, "runs on enrichedlab", and administrators reach Inspection from it. A Display
control sets the theme (System, which follows the device, Light or Dark) and four text sizes, and on desktop browsers
turns on a notice for when an agent waits for the reader.

A project is one page with no tabs, read from the top:

1. The head: lab, who started it, sharing, the title and the question as the abstract, clamped to three lines with
   "Full question and limits" under it.
2. The live status: the standing, one sentence, who has seen a stop, what the system does in the current step and for
   how long (from the step's own records, with no percentages), the steps against the plan (named when there are six
   or fewer, else one bar with counts) and the one action that fits. A wait for a compute slot says where the analysis
   stands as the broker records it: how many analyses submitted earlier are ahead of it, or that it cannot start as
   things stand, and whether the lab's or the person's slots are all in use (a broker that does not record the queue
   gets "Its place in line is not recorded yet."). While work runs, the line gives how long the last five analyses
   waited for a compute slot and ran, at the median, against the five before them, from each analysis's recorded start
   and end; it names no cause and no finish time. Once it scrolls under the bar, a compact line keeps the standing and
   that action in view.
   Watch it work, a quiet action beside Pause and Tell the agent, opens the record as it is written, newest first:
   what the system does now, each decision with its methods check, each analysis requested and ended with its files
   and their checksum state, the recorded steps of the analyses that run and of the one that ended last, and people's
   instructions and access changes. It starts closed, reads nothing while closed, reads on from where it left off on
   each change while open, announces a new line once, and points to the full record for every entry.
3. Since you last looked: what the records hold after the reader's last look (a new answer, steps that finished or
   stopped, new files, checks, instructions), each row taking the reader to its part of the page. The last look is the
   broker's when it reports one, read once when the page opens.
4. What it found: the agent's answer, labelled as its reading until a person accepts the analyses it rests on, its
   numbered findings, the parts of the question still open, the lead figure and the newest others. Every value in a
   finding is a button: it names its file, row and column, the exact text in the file, the step and method that made it
   and the file's checksum state. A whole results table the agent gave as part of its answer shows the
   columns it named, every row read from the stored digest, long decimals to four significant digits with the exact
   text in each cell's title; a table larger than a digest holds, or one of a lab's own data, shows its row count and a
   link to the verified file instead, never part of its rows. The agent's notes, and the notes and papers it read, sit
   one level down.
5. Your next step: Accept the result (a sheet names each finished analysis no person has accepted, with its checks,
   any earlier review and "Read it first"; accepting records the reader's own review on each, so a stop for review whose
   analyses are all accepted reads as finished), Ask for changes (the instruction box opens in place), Download
   everything and Rerun on your computer, then Edit the question, Continue with different methods and Run one method
   yourself, and the instructions given to the agent.
6. How it got here: each step on one line with its method, files, checks and review; a step opens in place with its
   saved history, its checks and its figures as plates drawn from the checksum-verified files (R PDFs with PDF.js, SVG
   copies as images). Older steps fold when the plan is long. Analyses outside the plan follow, then "Every step and
   file, in the detailed view".
7. Sharing on one line with Manage, which opens the people and their access; then, closed, the full record
   (every entry in order, each naming its source, the research agent's marked as its reading) and technical details.

From 1,360 pixels a margin beside the page holds "About this answer" (data, methods, checks, reviews, who can open it,
how to rerun it) and, when the reader points at a value or figure, where it came from; narrower, that is a section of
the page and a sheet. The old section addresses (`/notebook`, `/results`, `/checks`, `/downloads`, `/people` and the
rest) open the matching part of the page.

Every view stays current through one hub (`src/live.ts`). A view names what it shows by key (`inbox`, `projects`,
`tasks`, `commons`, `project:<id>`, `task:<id>`, `dataset:<id>`, `lab:<key>`), and a change to a key reads again only
the views that show it: at once while the page is visible, when it is next shown otherwise. Changes come from three
places. Each request the reader sends names what it changes (`keysFor`), so a review, a mark or a start shows on every
page at once, and a broadcast channel carries it to the reader's other tabs. The broker's change stream
(`GET /api/v1/changes/stream`) names the keys the agent's and colleagues' changes touch, and for
each project it names whether work still runs there, so a hidden tab's title stops saying "Working" without a read. The
workspace asks for it only when `/api/v1/me` says `change_stream: true`, connects it right after the sign-in, reads its
first lists once it is ready, resumes it with `?after=` after a renewal or a dropped connection, and reads everything
again after a `reset`. Changes from the stream reach a view at most once every one or two seconds, the last
always arriving. Without the stream (a broker that does not offer it, or after five failed connections) the lists read
again every minute and when the tab is shown, and a page opening on a list older than five seconds reads it first. One copy of the
waiting list (`src/waiting.svelte.ts`) and of the project list serve the sidebar, home, Waiting for you and the tab
title, and a read while one runs waits for it and reads once more, so an older answer never replaces a newer one. A
quiet read keeps the page in place; one that fails keeps what the page shows. Items that leave a list fade where they
stood while the rest close the gap, items that arrive rise into place and are announced once, and reduced motion
removes both.

Reads are conditional: the last answer to each address stays in the tab's memory with the broker's ETag (8 MiB in all,
none over 1 MiB, nothing on disk), a read sends `If-None-Match`, and a 304 reuses the kept answer. A broker that sends
no ETag keeps nothing.

A project page, and the open project in the Detailed view, read the project again when its key changes. While the
change stream is connected they open no stream of their own; without it they follow the broker's project stream, whose
events carry a fingerprint and whether work runs, never research content, and news of the agent's progress there also
reads the waiting list and the projects again. While work runs the page still reads once a minute, and without either
stream it reads every five seconds and says live updates are unavailable. The sidebar says "Reconnecting for live
updates" only when a connected change stream stays lost for five seconds. The tab title leads with the number of unseen items waiting for the
reader and says "Working" while the agent works on the open project. The optional notice appears only while the tab is
in the background, only for a stop for review or a step to check, and names no project, because a lock screen shows it
to anyone nearby.

"Tell the agent" works while the agent works. The instruction is saved with the question
and the agent reads it at its next decision; a decision already under way starts again from the new version, and
analyses already running are not stopped. When no work session is active, sending it starts the agent again with the
same methods.

An analysis page always names its lab, its project and who can open it, whichever route opened it; an analysis no
listed project contains says so. Its tabs are Results (first the result check when the agent's step said what its
result must show: checked, check failed or not fully checked, with code's reasons, as the agent read it in its newest
decision; then figures numbered, tables in publication style, each figure's note
naming the code and inputs in the same package, with "Show how this was made", which opens the next tab at those
files), How it was made (where it came from, Model-written code when the analysis
ran it (the block exactly as the broker admitted it, shown only after its checksum matches, the broker's and the
template's admission checks, the sandbox isolation probe and the outcome; its figures carry the unvalidated note), The
database query when the analysis read a database (the source and release, the checks the query passed, the outcome and
the query exactly as it ran, shown only after its checksum matches), and how this was done: method notes, parameters,
code, inputs, environment and logs from the retained package). Each recorded input starts closed and opens one step at
a time through the reviewed method and its own inputs to a terminal source snapshot; unreadable steps, missing records,
cycles and bounded reads are named without filling them in. Files shows all files and the recorded steps, followed by Review and
Activity. If the files of a finished analysis cannot be read, the page says so, keeps the status,
and offers Try again. Results lists the lab's work with each analysis's project. Inspection lists every project
for administrators; opening a private project needs a reason, gives read-only access for 30 minutes, and is recorded
where the owner sees it. The broker's project view reports an active inspection, so the notice survives a reload.

The Detailed view (`#/workbench`, or `#/workbench/projects/<id>` for one project) opens the same records laid out like
a code editor. An activity bar switches the side pane between the explorer, search and what needs attention, and opens
the timeline and the evidence package; each tool's name shows on hover and focus. The explorer shows one project,
chosen from a picker at its top, in plain groups: the question and the answer; the steps in plan order (a planned step
with no analysis yet is its own row), each with its files by kind and one row per figure however many formats R saved;
analyses outside the plan; and the record (data used, all files, work sessions, people and sharing, the evidence
package). Names wrap rather than being cut short, a step that ended since the reader's last look is marked New, and a
record that arrives while the page is open is marked for a moment and announced politely. A project opens on its
Overview tab, which stays open: where it stands in the project page's words, the agent's answer with each value read
from the table it names (a value opens that table), the limits, the plan and the agent's notes. Other records open in
a preview tab that the next one replaces until it is kept, with breadcrumbs to their step. The inspector at the right
shows the open record's details and its thread back through the code and inputs that made it, the analysis, the
decision that proposed it and the plan step to the question, from recorded links only; it starts closed on a window
narrower than 1,280 pixels. The panel under the editor starts as a bar and opens on the timeline (every saved version,
decision, analysis and access change, newest first), a step's saved output, or service activity. All files lists every
retained file by step and draws only the rows in view, so a run with thousands of files scrolls as one with ten; it
filters by kind and by what is new and finds a file by name, and each row says its place in the whole list. Needs
attention groups what needs a person by kind with counts; a stop for review is where the project stands, not a
problem. On a phone the view shows one pane at a time (explorer, open record, thread, timeline). Ctrl or Command K
finds a record by name. Opening a project from another lab moves the letterhead to that lab and closes the other lab's
records, as choosing a lab at the top does. A project with more analyses than one read returns is read page by page
through the broker's analyses cursor. It reads through the same broker routes, and its address carries only the
project identifier. Its art direction is in `design_direction.md`.

Every file opens in one result frame. The broker's registry of result kinds (`GET /api/v1/kinds`, or
the built-in entries until a broker serves one) gives each file its kind from the kind its method recorded or else its
media type; a declared kind, such as the broker's R figure, comes only from the code that wrote the file. The kind gives
its views, each limited to the media types the registry names for it: values for tables, the plate for R figures and images, text for code and
records, and name, size, checksum and download for anything else. Files that hold the same numbers (the method's
same-values group) are one result whose views are one switch apart: 3D, R figure, values. Values come a page of 100
rows at a time from the stored file, sorted by the broker by each column's type with missing values last, and each row
keeps its place in the file; the column summaries come from the broker's routine and show only when their receipt
names the file's bytes and digests to the summaries shown. A broker without pages leaves the reader as it was: the
verified file read whole. The 3D view draws the columns the method mapped to every channel the registry names, one vertex per stored row, with x across
and y going back as R's `persp()` draws them, a gap where the table has no rows, viridis colors with the stored lowest
and highest values, and its vertical scale printed;
it loads three.js only when it opens, draws with WebGPU or WebGL 2 only when something moves, and shows R's figure of
the same numbers when neither works or two 3D views are already open. A row picked in one view stays picked in the
others and the frame names its values, row and line. The file record under the views gives the file's kind and, when
the broker sends it, how the file passed its check against that kind; a file that fails opens as a plain file. In an
analysis's Files tab, Space on a file opens it larger in Quick Look (a sheet on a phone); the left and right arrows move
to the next file and Escape returns to the list.

An R figure whose render request, or reviewed method output binding, names its retained table and column mapping offers
Explore data in the figure toolbar. The companion reads that one checksum-verified CSV, tab-separated or JSON member
and draws only its stored rows: a 2D scatter or line for mapped numeric x and y, or a 3D view when z is also mapped.
For a requested surface on a regular x-by-y grid, each row is one separate column from the z minimum to its stored
value; a missing cell stays empty floor and no cells are joined or interpolated. Palette-coloured tops and uniformly
darkened sides keep adjacent columns distinct. A non-grid surface request says why it fell back to stored-row points.
It never fits, smooths, aggregates, bins, normalizes or adds a column. Stored cell text is kept for row readouts and
selection tables, including its written precision; categories keep first-appearance order. A mapped decimal must
round-trip through the browser's placement number to the same normalized decimal, and every mapped span and layout
intermediate must remain finite, or the whole table is refused.
Group legend controls hide and restore rows, a brush selects rows, and hover or keyboard focus names the row and mapped values. Above every view
it says that the R figure is the publication master. At desktop width the companion sits beside that figure; on a
phone the full-width R figure stays first and the open companion follows below it. The 3D scene lazy-loads three.js, uses WebGPU with WebGL 2 fallback,
renders only after an input, resize, selection or camera change, has no automatic turn, and is disposed when closed.
Its points share one palette-coloured point buffer and its columns share one instanced mesh, independent of group count;
the WebGL 2 point path keeps the round device-pixel-aware disc. Both modes use the render request's vertical palette,
or viridis when the request names none. Numeric axes and the colour bar choose at most six spread-out stored values,
including both ends, and label them with the retained cell strings. A faint floor grid and aspect-aware 35°/25° camera frame the projected
box to about 85% of either canvas dimension. Its controls, marks and row table have keyboard
paths, visible focus and a screen-reader count of drawn, hidden and selected rows. Facets, groups and selected rows are
windowed. Three.js raycasts the one column mesh or point object for depth-correct hover and click picking; camera
changes require no hit-index rebuild.

A result table opens in the figure editor from its table or from its 3D view, which sends its angle as
R's `persp()` takes it. R draws a preview as the settings change, and no version is made until the reader keeps one;
each version records the table, the settings and the render service's release. A compute worker draws each preview,
so the editor sends one at a time and only the newest settings go when a drawing comes back; it says so when a
preview takes a while, and names why one did not come. The editor offers the
settings the render service's catalog declares, among them a colour per group from Okabe and Ito's set and widths in
millimetres. A colour vision simulation and a print-size view change how the drawing is shown, not the figure. A
figure's record marks the version in the manuscript and sets two versions side by side with their differences marked.

A running step's output opens from its page ("Watch the log") and in the Detailed view's Output panel. It arrives as
numbered pieces from the broker's stream (`GET /api/v1/tasks/{task}/log/stream`), and the browser resumes after the
last piece it received. The page filters each piece again, so only colour and weight codes, line feeds, carriage returns
and tabs reach it; a carriage return rewrites its line as a progress count does. The log follows new lines until the
reader scrolls up, then offers a jump back with the count; it filters the step's lifecycle apart, finds, shows times,
wraps and copies plain text. The text is marked as not yet checked until the step ends; then the page checks the
pieces, size and SHA-256 of the last attempt's text against the broker's record and names the saved output files,
which keep the exact bytes. A step that ended without a record says its text was not checked.

A reviewed R analysis is an analysis like any other: it is named by its title, counts in the
project's totals and waits for review, and its record names the template that ran and the template's own statement of
the method. A lab dataset's intake and preparation (v10) are not analyses, so they stay out of the analysis lists and
counts; a table under `results/lab_data/` or `results/lab_derived/` says that the research lead sees its columns and
counts, not its rows. The start dialog offers each method a work session may list, including the reviewed R analyses
the research lead may choose.

A person runs one too. In a lab whose catalog (`GET /api/v1/template-analyses/catalog`) lists it, a finished table and a
prepared lab table offer Analyze beside Make a figure. The dialog lists the templates by the question they answer
(`src/template_analysis.ts`), the ones the table fits first with what the others need, and starts each with columns
picked by name and type; a prepared table in the long layout starts on the tolerance test. The request goes to
`POST /api/v1/projects/{project}/template-analyses` and the analysis page leads with what ran: the outcome, the
template's statement of the method, the headline statistics read from the checked `results/statistics.csv` (rounded
for reading, a small value in scientific form and never 0, every digit on request) and the template's notes, then the
results table and R's figure. A tolerance test offers the comparison its table is made for, one row per animal, with
the animal column as the experimental unit. A refusal shows the broker's words, a button for each setting it names
(the dialog opens with it marked) and, for repeated rows of one animal, the per-animal summary it asks for. The record
lists the settings, who asked (a person, or the research lead in a turn of its run) and every attempt's receipt. The
preview's stand-in for the render service (`fixtures/analyses.ts`, `fixtures/r_stand_in.ts`) follows the templates'
rules for summary statistics, the group comparison and the tolerance test, says where it differs from R, and gives
the other templates counts and means only; its drawings say they are not R's.

A project's Lab data section sends a table the lab keeps (CSV, tab-separated text or .xlsx, chosen or dropped) to
`POST /api/v1/projects/{project}/datasets` with one request id per chosen file, and lists each file with where it
stands. It offers Add lab data only where the reader's identity marks the project's lab with `lab_data: true`; a
missing field reads as false. The Detailed view lists the lab's files and prepared tables apart from the analyses. A file's page (`#/projects/{project}/data/{dataset}`) shows the upload guard's decision, then the sheet from the
profile's detail (`profile_detail.json` in the reading's checked package) in its own row numbers and column letters with
the broker's reading drawn on it, each choice the profile asks for with a starting answer (`startingSpec` in
`src/lab_data.ts`), and the columns with their guessed names, units and types. Approving sends the specification as
the broker names it, with the earlier preparation it revises; a 400 names the place in the page's words.
Each preparation keeps its receipt: rows and every filled cell accounted for, what changed in each column, and for a
refusal each cell no rule read with a link to the control that decides it. The prepared table opens in the result
frame and in the figure editor. The preview's stand-ins for the upload guard, the profile and the preparation code
(`fixtures/datasets.ts`) follow the broker's rules for text tables only, and its sample sheet
(`fixtures/lab_data/gtt_week4_simulated.csv`) is simulated.

## Integrity and limits

Every file shown or saved is checked against its recorded size and SHA-256 before use. A failed check shows or saves
nothing. Package members are read one at a time through the broker's verified member route and checked again in the
browser. Single files up to 32 MiB and project evidence packages up to 128 MiB are verified in the browser; larger
ones are refused here and go through the operator's verified command-line download. Byte integrity is not scientific
validation.

Tables keep every value as written, including precision, quoted fields and blank cells. Server pages refuse a page
whose checksum is not the file's; without them the inline reader takes up to 8 MiB and shows up to 1,000 rows. The 3D
view draws up to 250,000 points and refuses a table that is not one surface (a repeated place, an axis that is not
numbers) rather than guess. Text and code readers take up to 2 MiB. Inline PDFs draw at their printed size and
the enlarged view zooms to 600% with a bounded canvas. Downloads are always the complete original.
The figure companion is a separate all-or-nothing reader: at most 8 MiB, 50,000 rows and 200 columns. Past a bound,
with an unsupported file, a missing mapping or a mapped nonnumeric axis, it says why and draws no rows. It never
samples a larger input.

Lists are bounded by the broker: 200 projects, 200 tasks, 200 lifecycle events per page, and 100 access-record
entries per project page. Run decisions, deferrals and checkpoint history are paged until complete.

## Run the checks

```sh
npm run check
npm test
npm run build
```

`npm run test:browser` is an external-runner contract. It needs the loopback synthetic preview and three environment
variables: `WORKSPACE_BROWSER_URL` for that `http://127.0.0.1` or `http://[::1]` preview, `PLAYWRIGHT_NODE_PATH` for
an installed Playwright module, and `WORKSPACE_BROWSER_EXECUTABLE` for a Chromium executable with the new headless mode,
such as Playwright's Chrome for Testing build. The older headless shell reports notifications as denied and ignores a
granted permission, so the notification walk cannot pass there. It refuses
any target that is not the synthetic preview before it clicks anything. At desktop and phone widths in light and dark
it asks a question from home (a create whose answer is lost is sent again and makes one project), checks where a fresh
tab lands, searches with Command K, lists Working now, walks the Detailed view (the Overview and a value opening its
table, explorer keys, a figure's thread to its agent decision, search by name, what needs attention, the timeline and
status contrast), follows a long run (the analyses cursor, thirty steps with what is new, a few hundred files drawn a
window at a time with kind, new and name filters, records marked as they arrive, a wait for a compute slot with the
recent pace, Watch it work following the record as a step gets its slot, and the project page's counts), opens a
result in its frame (3D, R figure and values with a pick carried across them, server pages sorted by type, column
summaries with their receipt, keys on the 3D view and the switch, the 3D angle sent to the figure editor) and in Quick
Look from the file list, watches a running step's output (following, the jump back, a progress count on one line,
find, times, wrap, the lifecycle filter and copy), a stream that ends checked, one that does not match and one that ended without a record, and the same
log in the Detailed view's Output panel, draws a
figure with R previews (a value it cannot send named, colours per group, colour vision
and print size), keeps two versions, marks one for the manuscript, compares them and keeps a third that R refuses,
reads a project on one page (what changed since the last
look, where each value came from in the margin or a sheet, steps opened in place, the compact status line, the full
record) and accepts its result, reads results and traces a figure to its code, reads a model-written analysis with its
checks and downloads its block, reads a database query analysis and downloads its query, edits the question, starts,
pauses and resumes research, invites and removes a colleague, downloads and checks the evidence package, reads files
and recorded steps (late-step navigation, filters, selected-state contrast), sets up a new project, changes display
settings, runs an administrator inspection through reload, end and refusal, and follows a project while it is
instructed mid-run and its work session stops with the tab hidden (no polling while live, the tab title, the notice,
the read on return), and sends lab data (an .xls the guard blocks, a sheet read in its own rows and columns, a row
left out and taken back, a rule for values against a limit, a missing word left unread so the preparation stops at its
cell and the refusal leads back to the choice, the prepared table's receipt that adds up, and its first figure), and
runs reviewed R analyses (the tolerance test filled in from the prepared table, its statistics and notes, the
comparison of animals it leads to with every digit on request, a refusal of repeated rows with its setting marked and
the per-animal summary it asks for, no lab data where the lab does not take it, and the lab's files in the Detailed
view).
`WORKSPACE_BROWSER_CAPTURE_DIR` saves full-page captures.

## Connect a site

Serve `dist/` as static files at the same authenticated HTTPS origin as the broker. Route `/api/v1/*` to the broker
and other paths to the static client. The browser uses only same-origin paths. It has no broker-host setting,
bearer-token field, provider key, direct Kubernetes route, or external font, image, telemetry, or model endpoint.

Cloudflare Access supplies the browser session and verified origin assertion. The broker still validates issuer,
audience, and principal for every request. Use a same-origin CSP with `frame-ancestors 'none'`, `object-src 'none'`,
`base-uri 'none'`, `Referrer-Policy: no-referrer`, and `X-Content-Type-Options: nosniff`. Do not expose the Vite
development server or the fixture API as a shared service.

The API contract is in `src/contracts.ts` and `src/projects.ts` and follows `../../src/broker/model.rs` and
`../../docs/api.md`. Mutations carry the broker mutation-intent header. A retried submission keeps its request key, so it
cannot start twice. URLs carry only identifiers; drafts stay in memory; theme, text size and the notice
choice are stored in the browser.
