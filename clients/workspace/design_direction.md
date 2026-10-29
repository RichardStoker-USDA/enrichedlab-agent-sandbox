# Working paper

Replaces "Evidence bench", which still looked generated: an
invented square logo in the header, section labels drawn as small capitals behind a gold dash, rounded cards around
every result, a colored attention box, filter pills and generic bar charts in the preview. Earlier directions
("Question, work, evidence", "Research folio") are in this file's Git history.

## Glass Box

A prototype page showed it at desktop and phone widths in light and dark before it was built. It replaces the portal shell below (Portal navigation) with the reference application's shell and adds what
long, unattended runs and results of every kind need. The working paper's figure, table, provenance, wording and
refusal rules in the sections below stay. Where the workspace opens may change, so that is one rule in one
place (`src/landing.ts`) with its own tests.

### The idea

A glass box is an instrument whose case lets you see the mechanism working. The front shows the reading: the answer,
how sure the record is, and what needs you. Through the glass you can watch the work move as it happens. Opening the
case lays every part on the bench.

- The simple view is the front of the box: ask, watch it work in plain words, read the answer with its evidence, act.
- The detailed view is the open case: every step, file, query, log, check and review, laid out the way a code editor
  lays out many related files.

"Glass" names what the scientist can see. It is not a material rule: content surfaces stay solid, and translucency is
limited to floating chrome, as Apple uses it.

### Reader, proof object, visit

- Reader: a nutrition, food or agricultural scientist with ten minutes, not technical, used to an HPC portal and to
  iPhone and Mac apps, skeptical of results an agent made.
- Proof object: the answer with its evidence. Each finding, its numbers read from result files, the R figure that
  shows it and the steps that made it.
- Visit: ask, check on it, read the answer, decide whether to trust it, then accept, redirect, download or rerun.

### Seven rules

1. **Ask first, and land where the work is.** Opening the workspace lands on the reader's own work first: a project
   that needs them opens on that need, then one that finished since they last looked opens on its answer, then one
   that is running opens on its live screen, the most recently opened first. With none of these, home opens on the
   question field. A link to a project always opens that project. New question is in the bar on every page and in
   Command K's search (browsers keep Command N for a new window), and the phone keeps its Ask bar. The title, data,
   methods and sharing are a setup sheet with defaults after the question, and nothing runs until the person presses
   Start. The landing may change later, so it is one rule in one place with its own tests.
2. **Always say what is happening.** Every project has one live status line: where it stands, the step in words
   (step 2 of 4), what the system is doing in that step, and how long that step has run. The same line appears on the
   home row, the project head and the detailed view's status bar. It never shows a percentage or a finish time the
   records do not hold.
3. **The answer leads.** A project opens on "What it found": the agent's answer marked as its reading, the findings
   with values read from result files, and the lead R figure at reading size. With no answer yet, that place says so
   in one sentence. The agent's plan is never shown as a summary.
4. **Every number shows where it came from.** Pointing at a value or a figure fills the evidence margin with its file,
   row and column, the exact text, the code and data that made it, and the checks it passed. On a phone it is a sheet.
5. **One step to everything.** One Simple and Detailed switch, always in the bar. Detailed opens on the same project
   and the same item.
6. **Plain words where a scientist reads.** Recorded facts come first in deterministic sentences. The agent's own
   prose is labeled and sits one level down. Machine words live under Technical details.
7. **Apple-native, UC Davis, one design language.** One design language across the site's interfaces and the reference application's semantic roles, light and dark with
   the system setting as the default, sheets, popovers, segmented controls, inset grouped lists, full keyboard reach,
   44 px targets.

### Shell

- The the reference application shell replaces the portal menu bar: the bar plane (Aggie Blue with the gold rule in light, `#191919`
  in dark) carries the lab letterhead and its lab menu on the left, and search (Command K), the Simple and Detailed
  segmented control, Display and the account on the right. An inset sidebar holds five places: Your research, Waiting
  for you, Working now, Results, Library. Below 992 px the sidebar is the drawer, as in the reference application.
- Open OnDemand footing stays where scientists use it: home is a dashboard of their running work with live status (the
  "My Interactive Sessions" habit), Results is the jobs table, and status words stay short and familiar.
- Counts mean one thing everywhere: Waiting for you counts items that need the reader, in the sidebar, on home and on
  its page.
- Display offers System, Light and Dark; System is the default.

### Simple view

**Your research (home).** When the landing rule opens no project, the ask field leads: a large field, the lab it will
run in, and one plain line of what the agent can use there (its data sources and methods by name, its papers). Below
it, inset grouped lists: Needs you (each row with its action), Working now (live rows), Recently finished, then All
projects. On a phone a compact Ask bar docks at the bottom once the field scrolls away, so asking is reachable from
every scroll position. A lab with no data source says what the agent can and cannot do there.

**A project.** One page, no tabs:

1. Head: lab, who started it, who can open it; the title; the question in the serif, three lines at most, with "Full
   question and limits".
2. Live status, sticky under the bar while the page scrolls: the standing word, one sentence, the step line and the
   one action that fits (Review the answer, Pause, Resume, Try again).
3. What it found: the answer sentence and the agent's-reading label, numbered findings with bound values, the parts of
   the question still open, the lead figure at column width, then the other figures and tables as a strip.
4. Your next step: Accept, Ask for changes (the composer opens in place), Download everything, Rerun on your computer.
5. How it got here: each step on one line with its data, method, checks and outcome; a step opens in place; "Every
   step and file" opens the detailed view there.
6. Sharing, on one line with Manage.

The evidence margin sits at the right from 1360 px: "About this answer" by default (data used, methods, checks, who
reviewed it, how to rerun), and the provenance of whatever the reader points at.

As built: the whole status block does not stick, because on a phone it would cover a third of the screen; once it
scrolls under the bar a compact line with the standing and its one action takes its place. "Since you last looked" sits
between the status and "What it found". Accept records the reader's own review on each finished analysis that no
person has accepted, the ones the answer reads first, so a stop for review reads as Finished on the page and in the
index from the same review counts. Below 1360 px "About this answer" is a section of the page and the provenance of a
pointed value opens as a sheet. The full record and technical details close the page, and the old section addresses
open the matching part.

**Results** is the jobs table with rows that differ: the question, the project, the status, when and how long, and the
lab's work grouped by project. **Waiting for you** is one list, each item with its action.

### Detailed view

A code editor's layout with this product's words:

- Activity bar: Explorer, Search, Timeline, Needs attention, Evidence package, with labels on hover and focus.
- Explorer: The question and the answer, then Steps (each step with its figures, tables, code and data used), then
  The record: Data used, All files, People and sharing, Evidence package. Names wrap instead of cutting off; the width
  is remembered. One figure saved as PDF and SVG is one row with both formats inside.
- Editor: tabs with an italic preview tab, breadcrumbs, and viewers for figures (R PDF and SVG on white paper with
  zoom), tables (sortable, tabular figures, the source of each column), code (R and SQL, line numbers, read-only, the
  diff against its template for model-written code), logs (searchable) and notes, plus every viewer under Results of
  every kind (3D views, the figure editor). It opens on an Overview tab holding the same answer readout, never an
  empty field.
- Inspector at the right: the open item's details and The thread, from the item back through the code and data to the
  step and the question; each hop opens its record and lights its explorer row.
- Panel at the bottom, closed until asked: Timeline (every event, filtered by You, the agent and the system), Output
  (the live log of the selected running step) and Activity (service records, labeled as not scientific evidence).
- Needs attention lists failures, refusals and unreadable files, grouped with counts. A normal wait is not a problem.
- Status bar: lab, sharing, the reader's access, the live status with the waveform while work runs, and the checksum
  state of the open file.
- Command K finds any record by name and runs plain commands.

As built: the explorer adds "Also in this project" for analyses outside the plan (a person's own runs, figure
versions, steps taken out of the plan), and a planned step with no analysis yet is its own row. The Overview tab cannot
be closed; its standing sentence is the project page's, and each value of the answer opens the table it was read from.
A step whose files could not be confirmed carries the check mark in the explorer whatever its analysis reported, and
the project page's fold of earlier steps names every state it holds, so a failed or unchecked step never folds into
"finished". The inspector starts closed below 1280 px and leaves the tab order when closed; on a phone it is the Thread
pane, and the side pane's views are a segmented control at its top instead of the activity bar. Output shows a step's
saved logs once it ends; while it runs, it says the output shows here once the system streams it. All files draws
only the rows in view at a fixed row height, so the list's arithmetic stays exact at any length. Opening a project from
another lab moves the letterhead with it and closes the other lab's records, as choosing a lab at the top does. Records
that arrive while the page is open take a brief mark at their row's edge and one polite announcement; reduced motion
keeps the mark still.

### Live behavior

- Facts refresh every 5 s while work runs, as today, and move to a project event stream when the broker offers one.
  Updates are batched to the frame. A stale connection says "Reconnecting" and when the page last heard.
- The system part of the status line maps recorded states to words: waiting for a free compute slot, checking the code
  before it runs, running, checking the result files, drawing figures, writing up what it found.
- A new result slides into "What it found so far" and carries "New" until the page is left. A finished step's mark
  fills. The waveform moves only while work runs.
- The browser tab title carries the state ("Working: Test diet response study", "(1) Waiting for you"). A browser
  notification when work needs the reader is opt-in.
- Screen readers hear step changes through a polite live region.

### Live data: every view current

After a person rejected a result, home and Waiting for you kept the old item until a browser refresh. The workspace
is realtime for a person's changes and the agent's, with caching that never reads what has not changed.

- One rule for freshness: a view names the records it shows, and a change to one reads again only the views that show
  it. The reader's own change shows everywhere at once, in their other tabs too; the agent's and colleagues' arrive
  through the broker's change stream within a second or two. Nothing reads on a timer while the stream is connected.
- A list never blanks to say it is loading again. It stays as it is until the new answer arrives, and a failed quiet
  read keeps it.
- An item that leaves a list (a result the reader just reviewed) fades where it stood in 180 ms while the rows below
  close the gap in 220 ms on the workspace's curve. An item that arrives while the page is open rises 6 pixels into
  place in 220 ms and is announced once ("One new item waits for you."). The first answer of a visit arrives still,
  and a change of view the reader makes (a filter, another lab) replaces the list at once: motion reports what changed
  in the data, never what the reader just asked for. Reduced motion removes all of it. Transform and opacity only.
- Connection state is quiet. A broker without the stream is the ordinary state and is not mentioned; only a stream that
  was connected and stays lost for five seconds says "Reconnecting for live updates" in the sidebar's foot, and the project page keeps its
  own line.

Starting from several sets. A question can start from one, several or every data and method set in a lab ("even
do all of them if there are many"). The setup sheet's list gains a head row, "Every set in" the lab, whose box shows
some, all or none chosen, set apart from the sets by a firmer rule; the legend counts what is chosen, and one line under
the list says what the choice comes to in data sources and methods, without promising what the agent will find. The
project names every set it started from in its facts. Nothing is chosen for the reader when a lab has several sets.

### Long runs

The agents will work for hours or days, across many tools, steps, files and figures, on GPUs several scientists share,
so a run can slow to a crawl. Start and walk away is the main mode; a short task can still be interactive and gets the
same checks. A scientist who comes back must see where it stands, what it did since they last looked, watch it live
only when they want to, steer it, and check every result, including the agent's checks of its own work.

1. **Since you last looked.** From the reader's own last visit: steps finished, new files by kind, checks and their
   outcomes, instructions read, and whether it has answered. The status line counts steps against the plan, which can
   change. Never a percentage or a finish time.
2. **Slow is a state, not a failure.** When shared GPUs or the queue hold it back, the line says so from records: the
   wait for a compute slot and how many analyses are ahead, the time on this step, and today's median step time
   against last week's. It does not guess when it will finish. The waveform moves only while recorded work runs.
3. **Watching is optional.** "Watch it work" opens the record as it is written: each decision, tool call, check and
   file, newest first. It is not a chat, it starts closed, nothing needs it open, and the Timeline keeps all of it.
4. **Steer while it runs.** "Tell the agent" adds an instruction to the record at any time, and the page shows the
   decision at which the agent read it. Pause and Resume stay one click.
5. **Checks on every step.** Query and code checks, file checksums, the methods review and the agent's own checks of
   the values it states show on each step with their outcome. A step with no recorded check says so. The agent's
   self-check is shown as a recorded check, never on its word.
6. **Hundreds of files.** Files group by step, filter by kind, search by name and mark what is new since the last
   look. Figures show as a sheet that loads as you scroll; older steps fold into one row; the Detailed view's lists
   are virtualized.

As built: the pace compares the last five ended analyses with the five before them instead of today with last
week, because a project's own recent analyses are what the reader is waiting on, and its words give the wait for a
compute slot and the run time apart, so a longer queue shows as a longer wait without the page naming a cause. A wait
for a slot says where the analysis stands as the broker records it: how many analyses submitted earlier are ahead of
it, or that it cannot start as things stand, and whether the lab's or the person's slots are all in use. A broker that
does not record the queue gets "Its place in line is not recorded yet." Watch it work is a quiet
link beside Pause and Tell the agent; it opens under "Since you last looked", takes the keyboard to its heading, shows
the newest twelve lines, marks the agent's lines with the dashed rule the full record uses, and reads the recorded
steps of the running analyses and the last one that ended only while it is open. In the Detailed view the status bar
shows the same "now" words in place of the work session's sentence while work runs.

What the harness has to record, since the page can only show what is recorded:

- every decision, tool call, check, file and figure, in order, with its time, inputs by reference, outcome and
  checksums;
- a project event stream, so pages update from it instead of asking every 5 seconds (built);
- the queue: a project's place, the analyses ahead of it and its waits for a slot;
- model calls' start and end, without model names, so "writing its next step" is a recorded state;
- the agent's checks of its own work as typed records with their outcome;
- each person's last look at each project.

Until a fact is recorded, the page says it is not recorded.

### Results of every kind

The design presents any kind of data, result and file; shows 3D results
people can turn and swap with the R figure of the same numbers; lets a scientist customize an R figure for a manuscript
(titles, colors, fonts, legends); and shows a tool's or container's log live. The prototype (version 4 and later)
ran all three: a 3D surface on WebGPU with its R figures and values, the figure editor with R's drawings,
and a live log.

1. **One frame.** Every result opens in one frame: its name, file and checksum, the step and method that made it, then
   its views. Space opens the selected file or result from any list (Quick Look), the arrow keys move to the next one,
   Escape closes it; on a phone the frame is a sheet. The file's media type picks the viewer through one registry, and
   a kind gets its viewer when a reviewed method starts writing it.
2. **Views of one result.** Files that hold the same numbers in different forms show as views one switch apart: 3D, R
   figure, values. A point picked in one view stays picked in the others and names its line in the file.
3. **Numbers stay exact.** Views draw stored numbers. Nothing is fitted, smoothed, interpolated or recomputed in the
   browser. A view that thins points to stay fast says how many it shows; an exaggerated height or a log scale is
   printed on the view. Data colors use perceptually even scales that read with color blindness (viridis, Okabe-Ito);
   UC Davis colors stay on the interface.
4. **R draws what is published.** The R figure is the master. The 3D view can send its angle and vertical scale to R
   for a new figure version. The figure editor gains R previews before
   anything is saved, the settings journals ask for (subtitle, legend title, colors per group, widths in millimeters,
   typeface), a color blindness preview, a print-size view, versions side by side and a mark for the version in the
   manuscript.
5. **3D that is calm on shared machines.** three.js with its WebGPU renderer and WebGL 2 fallback (0.186.1). Its code loads only when a 3D result opens, and it draws only while something moves; R's still drawing
   stands in when neither GPU path works. Drag turns it, scroll or pinch zooms, right-drag or two fingers move it;
   arrows, plus and minus, 0 and Shift with an arrow do the same from the keyboard. Reduced motion turns camera
   flights into cuts.
6. **Logs are text, not a terminal.** A step's log starts closed: Watch the log on a running step, or the Output
   panel. It follows new lines until the reader scrolls up, then offers a jump back with the count of new lines.
   Errors and messages and System filter apart; find, times, wrap and copy. Escape codes other than color and weight
   are dropped, so a log cannot move the cursor, set links or reach the clipboard. A quiet log says how long it has
   been quiet and that the container still runs. The saved file keeps the exact bytes with a checksum. Logs have the
   same access as the step's files; the system's own logs stay with operators.

Kinds and their viewers: tables (sortable, paged from the server, each cell's file, row and column); R figures (paper,
zoom, versions, the editor); 3D (surfaces and point sets from tables now; glTF meshes, LAZ or COPC point clouds, NIfTI
or OME-Zarr volumes and mmCIF molecules when methods write them); logs; images (deep zoom and channels, OME-TIFF tiles
for large ones); maps (GeoJSON, GeoParquet, GeoTIFF); code and records; reports and notebooks in a sealed frame with
no scripts or network; anything else as name, size, checksum, origin and download.

#### Viewer modules

New kinds of results should be easy to add as modules. The result-kind registry makes each kind a registry entry (media types, a content check, a validator, how it is read, its views and a
still fallback) and gives views one interface: `mount(el, ctx)` and `unmount()`, with `ctx.read`, `ctx.page` and
`ctx.pick`. A new kind is then a registration and a view module, and the workspace core does not change. The Glass Box
viewers need eight additions, recorded:

1. Picks go both ways. A view sends with `ctx.pick(location)` and hears picks made in other views (`ctx.onPick`). A
   location names the same-values group, the file, the row index and, for text tables, the line.
2. A method declares each view with its encoding in its output binding (`views: [{ view, encoding }]`, such as which
   columns are x, y and height), so a 3D tab appears only when declared and the view never guesses columns. A
   converter that derives a drawable (a table to glTF) keeps the source row index on each vertex so picks map back.
3. GPU needs are an order of preference: WebGPU, then WebGL 2, then the still fallback.
4. Running output is a fourth access mode, `live`: numbered pieces over the event stream with a resume cursor. Color
   and weight codes, line feeds, carriage returns and tabs pass; every other control code is removed, and the view
   filters again. The stored log ends the stream with its checksum; until then the view marks live text as not yet
   checked.
5. A figure capability: `ctx.figure.preview(spec)` returns an R drawing without making a version, and
   `ctx.figure.save(spec)` makes one through the figure-version route. The figure editor and "send this angle to R" use
   it.
6. Pages return each row's index and, for text tables, its line in the file.
7. The frame keeps each view's state (camera, scale, pin) across unmounts and can save it with the step. It keeps at
   most two live GPU views per window; the others show their still until opened again.
8. Every group includes its values, or the view gives its own text equivalent for screen readers.

In the workspace, three.js ships in the bundle as a lazy-loaded chunk. The prototype page loaded it from a CDN only
because its host required that.

As built: the frame plans its views from the registry and the files of the same-values group, one per view, in
the switch order 3D, R figure, values; the figure view takes the lighter SVG with R's PDF as its original. A table
opens on its values, a figure on its plate. The switch is a tablist on the segmented track, so its arrow keys change
views and leave Quick Look's arrows to the file list. Values are a publication table (heavy rules, no box): a pointer
picks anywhere on a row, the keyboard picks with the row number. The 3D view keeps the table's x across and its y going
back, the right-handed space R's `persp()` draws, so it is never a mirror image of R's figure and its angle maps to
`persp()`'s `theta`, `phi` and `expand` (theta 0 looks from the lowest y, theta 90 from the highest x). A scale change
keeps the reader's angle and steps back so a taller surface still fits; the phone plate is square and steps back for
its width. The pick marker is the page's heading ink, not a data color. In the Detailed view, Space already opens the
selected record in the preview tab, so it keeps that as its Quick Look. Not built here: a method's own encodings beyond
x, y and height, point sets, meshes and volumes, and range reads, which wait for methods that write them.

As built: R draws a preview 650 ms after the settings stop changing. It shows as "Preview, not saved"; a value the
editor cannot send is named in its place, and no version is made until "Keep this version". A broker without previews
keeps the earlier flow, where each drawing is a version. A preview is shown only when its size and SHA-256 match the
answer. The settings come from the render service's catalog: subtitle and legend title, a color per group from Okabe
and Ito's set (read from the grouping column, up to 12 groups), a width of one column (85 mm), 1.5 columns (120 mm),
full width (180 mm) or any value in millimeters, sent to R in inches, and the face R used. The stage can show the
drawing as seen with deuteranopia, protanopia, tritanopia or no color vision (Machado, Oliveira and Fernandes 2009
matrices in an SVG filter), labeled as a simulation, and at print size in CSS millimeters beside a 10 mm bar to hold a
ruler to. Neither view changes the figure. A version's record marks the one version of its figure in the manuscript,
with who marked it and when; the source analysis lists the figure by that version. Two versions sit side by side with
each differing setting marked. The 3D view's "Draw this angle in R" opens the editor on the surface chart with
`persp()`'s theta, phi and expand from the view. The loopback preview's stand-in for the render service draws plain
SVGs and says in each one that R did not draw it.

As built: a compute worker draws each preview and the broker waits up to 40 seconds, with one
waiting preview per person. The editor sends one preview at a time and only the newest settings when a drawing comes
back; after 8 seconds the words say a compute worker is drawing it; it stops waiting at 45 seconds. A preview that did
not come names why: a table the worker cannot read as its record names it (no "Preview again", since asking again
cannot help), a drawing too large to preview (keep a version to see it), a busy moment or a service that did not
answer (both offer another try). A kept version whose chart catalog moved since its preview says it can differ from
the preview. Text settings take the length their chart declares, 200 characters when it declares none.

As built: a preview R refuses shows the broker's own words as sent, never the service's, and the
settings whose column the refusal names are marked; the refusal's code chooses what to mark, never the words. A
preview that comes back after the reader lost access to the project says so, without another try. A kept version that
was not drawn shows its receipt's broker words.

As built: the log is text on the page's surface with a hairline, the system monospace and line numbers. Colour
codes become three tones by meaning (alarm, note, emphasis) in the interface's own colors; 256-color and direct colors
fall back to the ink. The cluster merges a step's messages and errors into one stream, so the filters are All, Messages
and errors, and System (the step's lifecycle); a source that keeps them apart gets one filter each. Find moves only the
log, not the page. Up to 5,000 of the newest lines are drawn, earlier ones on request. The log is checked when the step
ends and stays open after it, so the check can be read; the saved output files are named beside it. In the Detailed view
the log fills the Output panel at whatever height the reader gives it, so the newest lines stay in view, and it stays
after its step ends until another step is opened. A step that ended without a closure record
says its lines were not checked and points to saved output only when there is some, and a stream the broker refuses
(64 open streams in all, 4 per person) is asked for again after 3, 10 and 30 seconds, never in a tight loop.

#### Figure companions

The Glass Box figure plate gains one quiet action, Explore data, when the figure's render request or reviewed method
binding names both its retained input table and its column mapping. It extends this direction; it does not add another
visual language. The retained R drawing stays on white paper and remains the publication master. The companion is an
instrument view beside it, on the existing solid plate surface, with the same type, rules, controls, focus and light
and dark tokens.

- A fixed line above every companion reads, "Interactive view of this figure's input data. The R figure is the
  publication master."
- The client reads one checksum-verified CSV, tab-separated or JSON package member. It accepts at most 8 MiB, 50,000
  rows and 200 columns. A file past a bound, an unsupported shape, a missing mapped column or a mapped axis that is not
  numeric is refused in full. Every present mapped decimal must round-trip to the same normalized sign, digits and
  exponent through the browser's placement number. Every mapped span and every coordinate intermediate used by the
  layout must also be finite; otherwise exact placement is not possible and the table is refused. Nothing is sampled.
- The render request supplies x, y, colour or group, facet, z when present, labels and units. The view does not guess a
  column. It draws one retained row per mark in file order, keeps category order by first appearance and prints the
  stored strings in row details and the selection table. It does no fitting, smoothing, aggregation, binning,
  normalization or derived-column work.
- Two mapped numeric columns make a scatter or the request's line. Three make a 3D view. A requested surface on a
  regular x-by-y grid is a set of separate columns from the z minimum to each row's stored value; absent cells are
  empty floor. A column fills 92% of its grid cell, keeps the palette colour on top and uses one fixed darker factor on
  its sides. An irregular grid falls back to round stored-row points and says so. Neither path joins or interpolates values. Legend
  entries toggle groups; a brush selects rows; hover, plot focus and keyboard row movement name the mapped values and
  retained row number. A polite text summary gives rows drawn, hidden by legend controls and selected.
- At desktop width the figure and companion share the figure column. On a phone the publication-master R plate remains
  full width and the open companion follows below it; opening a companion never hides the figure. Closing Explore
  removes its scene and returns the figure to its earlier layout.
- The 3D module lazy-loads three.js 0.186.1. WebGPURenderer chooses WebGPU and its maintained WebGL 2 fallback. Drag or
  arrow keys turn, wheel, pinch or plus and minus zoom, and Reset or 0 restores the starting camera. There is no
  automatic rotation and no render loop: pointer, key, resize, selection and camera changes request one frame. A
  zero-size host waits for a real size. Reduced motion has no camera flight to remove. The default 35°/25° camera fits
  the projected box to about 85% of both canvas dimensions at the current aspect ratio. Plain secondary-ink tick labels
  sit outside the box, choose at most six spread-out retained values including both ends, use their cell strings exactly,
  and drop on overlap. Categories stay in retained data order and thin by place. A faint token-coloured floor grid keeps
  depth readable; the labelled vertical colour bar repeats the same stored-value ticks.
  All surface columns share one instanced mesh, with per-instance colour and legend visibility. All scatter marks share
  one Points object, with per-point colour and visibility; its WebGL 2 path uses the round device-pixel-aware disc.
  Panel, grid, frame and tick text use the current light or dark semantic tokens.
- Facet and group controls and the selected-row table render fixed-height windows around their scroll view and state
  their full counts. Three.js raycasts the single column mesh or point object for hover and click, using the nearest
  drawn intersection and a CSS-pixel threshold for points; camera changes do not rebuild a second index. A brush
  projects retained mark centres once when it closes. The empty string remains a real group and its legend control
  affects drawing, counts, selection and keyboard traversal like every other group.

The loopback preview's season figure is the deciding fixture. Its retained R PDF and SVG share the block-by-hour table
through the method's recorded same-values group, and that binding names three numeric columns, axis labels and units.
The result frame and figure companion therefore read one mapping and one verified table.

What the backend has to provide, since today a step's output is saved only when the step ends and each figure change
runs as a new version:

- live logs: built on the broker branch: numbered pieces of at most 4 KiB, filtered to the
  codes item 4 keeps, a 4 MiB limit per attempt, a resume cursor and a closing record the view checks; the full log is
  still saved with its checksum. The worker keeps live output off until the containment review accepts reading a
  running sandbox's output, so until then a finished step's stream closes with no record;
- figure previews and the manuscript mark: the broker routes (shapes proposed and accepted), and the new
  catalog settings and surface chart (render service owner);
- 3D and other kinds: the registry, validators, range reads and `same_values_as` links of the result-kind registry, and methods
  that write open formats (the harness and method authors);
- tables: server pages and column summaries past 1,000 rows.

### Lab data: the sheet as the lab wrote it

A lab's own tables arrive the way instruments and spreadsheets write them: titles above the names, a units row, Mean
and SD rows under the animals, a note at the bottom, one measure across several columns, `<0.20` and `>600` where a
value is reported against a limit, NA and a dash for missing, Male, male and MALE for one category. The broker's
profile says where the table is and what needs a decision; the page has three jobs:

1. **The sheet in its own coordinates.** A dataset opens on its sheet with the spreadsheet's row numbers and column
   letters, and the broker's reading is drawn on it: titles and notes set back, the names and units rows labeled,
   summary rows struck through as left out, the cells a choice affects marked (a value against a limit underlined in
   the mark colour, missing words faded, a spelling the table rewrites underlined). Above each letter is the name the
   column gets, or "not read", and one rule runs under the columns that become one measure. A row number leaves its
   row out or takes it back, and a column letter stops or starts reading its column, so the sheet is the control.
   Every place the page names (rows 13 and 14, column F) is one the scientist finds in their own file.
2. **Choices, not warnings.** Each thing the profile found is one sentence that says what the sheet does, with the
   control that decides it and a starting answer already chosen: summary rows left out, one row per time point, values
   against a limit written as missing (which invents no number), the missing words found, one spelling per category
   (the most common one), and stopping if an ID repeats. The page counts the choices filled in, never the problems.
   Names, units and types are labeled as the profile's guesses ("in the header", "from the units row") until a
   preparation is approved.
3. **A receipt that adds up.** A preparation opens on its receipt: rows read, left out and written, one line in which
   every filled cell of the sheet is outside the table, in a row left out, in a column not read, or read, and what
   changed in each column as counts. A refused preparation names each cell no rule read by row, column letter and
   text, with a link to the control that decides it. The prepared table opens in the result frame, with Make a figure,
   and a table in the long layout starts its first figure on its points across, its values up and its first category
   as the colour.

The page says once that the broker's own code applies the choices and no model reads a value, and that the research
lead sees a prepared table's columns and counts, never its rows. File names are the person's own text, shown as text.
A held or blocked file says it was not taken and why. Projects list their lab data with where each file stands, and a
file dropped on that section is sent like a chosen one.

### Reviewed R analyses: an analysis is a statement

A reviewed R analysis is code no model wrote: the render service's template runs on a table as it was saved and keeps
every digit of its results. A person reaches one from a table, and the page has five
jobs:

1. **Ask by the question, not the test.** Analyze sits beside Make a figure on a finished table and on a prepared lab
   table. The dialog lists the catalog's templates under the question each answers (describe a column, compare groups,
   animals over time, relate columns, genetic scans, plan a study), says in one plain sentence what each does, and
   puts the ones the table fits first; one that does not fit says what it needs ("Needs 2 numeric columns this table
   does not have"). Settings start from the table: the animal column by name, a time column named as a time, a measure
   that is not a time, a category for groups. A prepared table in the long layout starts on the tolerance test.
2. **The page leads with what ran.** The outcome in one sentence, the template's statement of the method set like a
   methods section, then the headline statistics read from the checked `statistics.csv` as a ledger: the template's
   own names in mono, values in tabular figures, rounded for reading, a value too small for its decimals in scientific
   form and never 0, every digit on request. The template's notes follow, then the results table and R's figure.
3. **A refusal points at its setting.** The broker's words stand as written. Each setting the refusal names is a
   button that opens the dialog with that setting marked and focused.
4. **Animals, not readings.** A tolerance test's table has one row per animal, so its page offers the comparison it
   is made for (a group comparison for two groups, ANOVA for more) with the animal column as the experimental unit. A
   comparison that finds repeated rows of one animal is refused, and its page offers the per-animal summary first, on
   the same table, carrying the groups along.
5. **Runs, not overwrites.** Changing the settings makes a new run that names the one it revises; the record lists
   the runs, every attempt's receipt (service version, image, times, package checksum) and who asked: a person, or
   the research lead in a turn of its run.

The title field shows the name the analysis gets when it is left empty, and that name is the one sent, so the page,
the project and the Detailed view name it the same way. The Detailed view lists the lab's files and prepared tables
under Lab data, apart from the analyses, and a lab takes lab data only when the reader's identity says it holds the
dataset method.

### Material, type, color, motion

- Tokens: the workspace's current roles, which follow the reference application. Light: white canvas, Aggie Blue headings and
  fills, black tints for text and rules, groups on a faint Aggie Blue wash. Dark: black canvas, `#191919`, `#333333`
  and `#4C4C4C` surfaces, Aggie Gold fills and focus. Work in progress Gunrock (light) and Rec Pool (dark); failure
  Double Decker and Rose; finished is ink with a check. Only published UC Davis values.
- Translucent material only on floating chrome (the bar over scrolled content, sheets, popovers, the stuck status
  line), with a solid fallback when the reader asks for less transparency or the browser lacks backdrop blur.
- Type: the system interface face (SF Pro on Apple devices, Proxima Nova where installed); the system serif for the
  question, findings and captions; the system monospace for machine facts. The existing scale stays.
- Shape: controls 8 px, groups and popovers 12, sheets 16; figures and data marks square on white paper.
- Motion with a job, transform and opacity only, one ease curve (`cubic-bezier(.32, .72, 0, 1)`): the status line
  carries from a home row into the project (view transition); Simple to Detailed carries the answer readout into the
  editor's Overview tab; a step mark fills in 220 ms; sheets rise in 280 ms; popovers in 140 ms. Reduced motion
  removes all of it.

### The detail that should feel ahead

- Simple view: every number shows where it came from, in the margin beside it.
- Detailed view: the thread grows while the agent works. Each new file appears in the explorer and its line back to
  the question draws in the inspector as the record arrives.

### Words

| Say | Never on ordinary navigation |
|---|---|
| question | goal, prompt |
| step | action, action key |
| analysis | task |
| the agent worked (Timeline) | run, research run, turn, work session |
| saved version | revision, checkpoint |
| data used, data source | workspace, input refs, profile |
| method | tool, profile, recipe key |
| checks | admission, receipt |
| files | artifacts |
| the agent's reading | assessment |
| waiting for you | review required |

### Refused

A chat transcript as the main view; percentages or finish times the records do not hold; the agent's plan presented as
a summary or an answer; a view that computes new numbers, or a browser drawing offered as the figure to publish;
normal waiting listed as a problem; an empty editor; model names or versions; runtime words on ordinary navigation;
and the standing bans (callout boxes, tinted badges, banners, card grids, bouncy controls, blinking dots, glow,
sparkles, invented marks, marketing copy).

## Quiet Instrument for the workspace

Recorded before building. The workspace should be easy to follow at every step, with the flow and finish of an
Apple app: the common path obvious, advanced options one step away, and elegant, polished
motion, controls, surfaces, type and spacing in UC Davis colors, light and dark. Three places
overwhelmed: the landing page, the menus and the pages under them, and the detailed view's sidebar. This section sets
the shell, the page structure and the material for all three. Where it conflicts with an earlier section, this one
wins; the earlier sections' figure, table, provenance, wording and refusal rules stay.

It joins the "Quiet Instrument" direction of the site's the reference application application, the reference for how the UC Davis
colors sit: hierarchy from type, spacing and alignment; a surface only where it
holds something the reader acts on; Aggie Blue structure with Aggie Gold as a thin signal line; three layers of depth;
the same shapes and motion budget. The workspace keeps its own composition: a scientist's desk where the agent's work
arrives for review.

### What the earlier screens showed

- Seven places in the bar plus a lab menu: "Waiting for you" appeared three times on the landing page, "Detailed
  view" as a place, a starting point and a switch, and "Labs" repeated the lab menu.
- The landing page's four starting points were drawn as empty squares that read as unchecked boxes, and the filter
  column took a third of the width before the first project.
- A project page ran 7,500 pixels at desktop width and an analysis page 8,200: every section one after another, with
  the status, the answer, instructions and actions competing at the top.
- The detailed view opened on every lab and project, with secondary text cut off mid-word on most rows and a details
  panel open under an empty editor.
- Dark mode was a black page with nothing grouped.

### Rules

1. **One question per screen, one primary action.** The landing page answers what needs the reader and what is
   happening; a project answers whether it is done, what it found and what to do next; an analysis answers what came
   out and how it was made. Each has one filled button at most.
2. **Advanced options are one step away, never gone.** Filters, other methods, technical details and the full record
   sit behind a control that says what it opens (a Filter button, a "More" menu, a tab, a disclosure), so nothing a
   scientist used before is lost.
3. **Navigation stays put.** The bar keeps the same places on every page; a project and an analysis switch sections
   with a tab bar under their heading instead of a long scroll and a second rail.
4. **A surface means a group.** Lists of things to open are grouped inset lists on a raised surface (12 pixel corners,
   hairline rows, a chevron); tables stay ruled; figures stay on white paper. No card grids, no box around a single
   item.

### Shell

- **The bar** keeps the Open OnDemand order, with fewer places: the lab
  letterhead (its menu now lists every lab with its project count, replacing the separate Labs menu), then Your
  research, Waiting for you with its count, Results, Library (a menu: papers, the commons, shared notes) and Detailed
  view. Inspection moves into the account menu for administrators. Display and the account stay at the right.
- **Menus** open as popovers on a raised surface with a soft shadow, 12 pixel corners, rows of a title and one plain
  line, the current one marked; they fade and scale up from 98 percent in 140 ms and close on Escape or a click away.
- **The rail** is gone on wide screens; below 1,180 pixels the drawer keeps the same places.

### Pages

- **Your research (landing).** A title and one line naming the lab, with Start a project (filled) and Run one method
  beside it. If anything waits for the reader, a "Waiting for you" group comes first: one row per project with what it
  waits for and when. Then the projects: a segmented control (All, Started by you, Started by colleagues), a search
  field and a Filter button that opens the lab and standing filters as a popover (a sheet on phones), then one grouped
  list per lab. A row holds the title, the question in one serif line, the status mark with its words and one quiet
  line of who, sharing and when. The starting-point squares are removed; their places are in the bar.
- **A project.** The head (lab, sharing, title, the question as the abstract, the Simple and Detailed switch), then the
  status: the standing in large type, the step line, one sentence and the actions. Under it a tab bar, sticky below
  the masthead: Overview, Results, What the agent did, Checks, Data and downloads, People. Overview holds "Did it
  answer your question?", the agent's notes, the newest figure plates and "Tell the agent what to change". Each tab
  is its own address, so a link opens the same tab.
- **An analysis.** The same pattern: head and status, then tabs for Results, How it was made, Files, Steps, Review and
  Activity.
- **Detailed view.** The sidebar opens on the current project, chosen from a project picker at its top that lists
  every lab and project the reader can open. Under the picker: a filter field, then three groups with small headings
  (The question, The work, The record), each row a 20 pixel line icon, a name and a count, with no secondary text cut
  short. The open row is a raised pill with a gold mark at its left edge. The Trace, Problems and Activity panel starts
  closed as a slim bar with its counts and opens on a click.

### Labs

- **Research labs lead; operator labs sit apart.** A lab whose key ends in `-qualification` holds the platform's own
  qualification runs. The lab menu lists it under its own "Operator" label after the research labs, a first visit
  opens a research lab, and its projects and waiting items show only while it is the chosen lab, so qualification runs
  never crowd a scientist's page.
- **A lab offers only its own methods.** Run one method and the research controls list the methods granted to the
  project's or chosen lab (broker migration 052), never another lab's.
- **A new lab welcomes the reader.** A lab with no project yet opens on one panel: the lab is ready, what a project is,
  what the lab can work from (its starting points by name and description) and the one primary action, Start a
  project, with Run one method and the lab's library beside it. The list, the filters and the header actions return
  with the first project.

### Material

- **Light:** white canvas; grouped surfaces on a faint Aggie Blue wash (`rgb(2 40 81 / .035)`) or white with a
  hairline and a soft blue shadow; rules in the published black tints; Aggie Blue headings and filled controls.
- **Dark:** the reference application's neutral reading room (black canvas, the published `#191919`, `#333333` and `#4C4C4C` surfaces) with
  the groups raised on `#191919` and white veils for hover and selection, so lists read as grouped instead of a flat
  black page; Aggie Gold is the filled control, focus and current-place mark. Aggie Blue is not the dark canvas.
- **Shape:** controls 8 pixels, groups and popovers 12, sheets 16, the search field and segmented track fully round.
  Data marks keep square corners.
- **Type:** one family (Proxima Nova where present, the system face otherwise). Page titles 28 pixels with tight
  tracking, section heads 18, list titles 15.5 at 600 weight, quiet lines 12.5; the question and captions stay in the
  serif.
- **Icons:** one monoline set on a 20 pixel grid (1.6 stroke, round joins) for navigation rows only, as in the reference application.

### Motion

Every movement explains a change of place or state, and reduced motion removes all of it.

- A page change cross-fades with an 8 pixel rise of the content (220 ms) where the browser supports view
  transitions.
- The selected tab's mark slides to the new tab (220 ms, `cubic-bezier(.32, .72, 0, 1)`) and the content cross-fades.
- Popovers fade and scale from 98 percent (140 ms); a phone sheet rises from the bottom (280 ms).
- A list row takes its hover wash in 120 ms and its chevron moves 2 pixels; disclosures open with their height.
- The first view of a session rises into place once, in a 60 ms stagger; later views arrive still.
- The waveform still marks real ongoing work only.

### The detail that should feel ahead

The status of a project travels with it: the standing word and step line a scientist read on the landing row are
the same object at the top of the project page, carried across by a view transition, so the eye never has to find
its place again. In the detailed view, choosing a project in the picker moves the sidebar's gold mark to its question
row, so the reader always starts at the top of the thread.

## Reader and proof object

The reader is a nutrition, food or agricultural scientist who is not a software engineer. They come back to a project
to answer three questions: what did we find, can I trust how it was made, and what do I do next.

The proof object is the project read as a working paper: the question as its abstract, the R figures and tables as
numbered figures and tables, how they were made as its methods, and the dated record of everything that happened.
Scientists read papers every day, so the page borrows that order and those conventions instead of an app dashboard's.

## Family and shell

The workspace belongs with the reference application, the reference for how the UC Davis colors sit. It shares its
token semantics and shell: a masthead plane with a gold rule, a text-only name with the institution beneath it (there is no product mark and none is invented), a grouped left rail, and reader text-size
controls. The composition inside the shell is this product's own.

- Masthead: the reader's lab over the organization the site names (see "Lab letterhead"), the current page after a divider,
  and on the right the Display control (theme and text size) and the account.
- Left rail on wide screens: the reader's projects by title with a plain status word, then All analyses and, for
  administrators, Inspection. On phones the rail is a drawer and the home page is the same list at full width.

## The page

A project reads top to bottom as a paper:

1. Running head (visibility, group, last update), the title, and the question set as the abstract in the serif,
   with the constraints after it.
2. One status sentence and the next action. When something needs the scientist, it is written as a short list
   marked by a change bar in the margin, the way an editor marks a manuscript. No tinted box.
3. Numbered sections: 1 Results, 2 How it was done, 3 Plan, 4 Record, 5 People and access. Numbers are set in the
   margin on wide screens; a sticky outline in the right margin tracks the reader's place.
4. Results: each analysis is a subsection. Figures are the retained R PDFs and SVGs on white paper, numbered through
   the project, with captions below ("Figure 2. Feature changes, diet A against the reference.") and the provenance
   beside them in the margin: file, pages, checksum result, and a link to the code and input that made it. Tables
   are publication style (three rules, no boxes, tabular figures) with captions above.
5. How it was done: method notes open by default, then parameters, code, inputs, environment and logs.
6. Record: dated entries in a notebook column. Each entry names its source in words (You, a colleague, Research
   agent: its reading, not verified, System record, Administrator). Filters are plain words with counts.
7. People and access: a ruled list, invite and remove, and the access record.

An analysis that ran model-written code adds a numbered "Model-written code" section after its results: a serif
sentence saying the code is unvalidated until read, the checks that ran before it as glyph-and-sentence lines
(admission twice, then the isolation probe in plain words), the outcome, the code reader, and a small record of
template, checksum and environment. Its figure notes carry the same margin change bar. No warning box and no badge.

Analyses and administration pages use the same paper conventions. Tables of projects or analyses are ruled lists,
not card grids.

## Material and type

White stock and navy ink in light mode; a neutral dark reading room in dark mode, as in the reference application. Figures always sit on
white paper because R draws them for white paper, framed by a quiet stage. Interface text uses Proxima Nova where
the reader has it and the system sans otherwise (UC Davis licenses Proxima Nova; this site does not load it). The
question, captions' lead words and the abstract use the system serif, standing in for UC Davis's Freight Text.
Machine facts (checksums, identifiers, paths) use monospace and stay in margins or Technical details.

## Color

Official UC Davis values (marketingtoolbox.ucdavis.edu/brand-guide/colors), assigned as the reference application does.

| Role | Light | Dark |
|---|---|---|
| Canvas | `#FFFFFF` | `#000000` |
| Raised surface (rail, dialogs) | `#FFFFFF` with rules | `#191919` |
| Text | `#191919` | `#FFFFFF` |
| Secondary text | `#4C4C4C` | `#CCCCCC` |
| Headings | Aggie Blue `#022851` | `#FFFFFF` |
| Rules | `#CCCCCC` | `#333333` |
| Masthead plane | Aggie Blue with a gold rule | `#191919` with a gold rule |
| Primary action | Aggie Blue, white text | Aggie Gold, Aggie Blue text |
| Editor's mark (change bar, active rule, figure number) | Aggie Blue `#022851` | Aggie Gold `#FFBF00` |
| Focus | Gunrock `#0047BA` | Aggie Gold |
| Failure | Double Decker `#C10230` | Rose `#FF8189` |
| Work in progress | Gunrock `#0047BA` | Rec Pool `#6FCFEB` |

Only published values are used; there is no darkened or faded gold. Bright gold is never text or a thin mark on white
(1.6:1), so the light page marks in Aggie Blue and keeps gold for the masthead rule and filled controls. A disabled
control turns neutral grey instead of fading its fill. Completion is a word and a check in ink, not green. UC Davis publishes no dark-mode specification; the dark theme is this
application's accessible interpretation, shared with the reference application.

## Motion

A figure opens from its place into a dark light-table view (View Transition) with zoom and page controls. The outline
marker slides to the section being read. A three-bar waveform shows real ongoing work and stops when the work stops.
Hover and focus changes take 120 to 220 ms. Nothing bounces, blinks or repeats; reduced motion gets instant changes.

## The detail that should feel ahead

Every figure knows how it was made. Its margin note names the code and input files from the retained package, and
"Show how this was made" moves to those files in section 2 and marks them with the change bar, so a scientist can go
from a plot to the exact R code and data behind it in one step.

## Workbench: instrument bench

The workbench (`#/workbench`) is a second way into the same records for a scientist who wants
every element of a project side by side. It borrows a code editor's layout because that layout already handles many
related files, one open at a time, with nothing hidden. It keeps this file's shell, tokens, type and refusals.

- Explorer: labs are the reader's research groups, then projects, then the question and plan, research runs and their
  turns, analyses with their files grouped by kind, recorded steps, model-written code, the record, people and the
  evidence package. It shows only what the broker returns to this reader; a lab-visible project appears to the lab.
- Editor: one tab per open record. A single click opens a preview tab, set in italics, which the next preview
  replaces; a double click keeps it. Figures stay the retained R files on white paper.
- Details panel: Trace, Problems and Activity. Activity is service movement and says it is not scientific evidence.
- Status bar in the masthead plane: group, visibility, the reader's access, the run's state and the checksum state
  of the open file.
- Keyboard: arrow keys, Home and End in the explorer, typing a name jumps to it, Ctrl or Command K finds any record
  by name, pane edges resize with the pointer or arrow keys.
- Phones show one pane at a time, switched from a bar within thumb reach above the status bar.
- Row glyphs are drawn from rules, as elsewhere. The editor's mark (Aggie Blue, Aggie Gold in dark) marks the open
  row, the open tab, the current trace hop and the selected search result.

The detail that should feel ahead is the Trace. From any file it walks back through the code and inputs that made it,
the analysis, the agent turn that proposed the analysis and the plan step, to the question. Hovering a hop lights its
row in the explorer and clicking opens it. Every hop is a recorded link; a hop the records do not support is left
out, and an analysis a person requested says so instead of showing an invented agent turn.

## Lab letterhead

A review of the running workspace found that the workbench and the per-analysis
records worked, but it was unclear how analyses belong to projects, how projects are organized and shared by lab and person,
and how the project page's long scroll of results and methods fits together. This section changes the shell and the
project and analysis pages; the tokens, type, figure treatment and refusals above stay.

- **The lab is the letterhead.** The masthead names the reader's lab in the title position, with the organization
  beneath it when the site names one (the broker's `--site-organization`, such as a research center). The page being read follows
  the divider as before. A person in several labs opens a lab menu from the lab name; the choice is kept in this
  browser and filters the rail, the home page and All analyses. Nothing names a person as a brand and there is still
  no product mark.
- **The rail is the lab's shelf.** Under the lab: its projects, each with who can open it (Private or Lab) and a plain
  status word, then All analyses and the workbench. The foot names the reader and, once, in faint small text, "runs on
  enrichedlab".
- **A project is a contents page.** Title, the question as the abstract, one status sentence and the next action, then
  the analyses as numbered entries. Each entry shows its title, status and date and its retained figures as small
  plates on white paper. Plan, History and People and access are tabs under the contents, not further screens of
  scroll. How an analysis was done moves to that analysis.
- **An analysis is a section of the paper.** Its running head always reads lab, project and who can open it, whichever
  route opened it; an analysis no listed project contains says so. The page reads Results, How this was done, Inputs,
  then All files, Recorded steps and Activity, with Technical details closed.
- **Failures are visible where they happen.** If an analysis finished but its files could not be read, the page says
  that, keeps the finished status, and offers Try again. The workbench lists the same failure under Problems.
- **No inferred people.** The trace names who asked from the broker's origin record once it is read (a person, or a
  research run and its turn). Until then, or where no record exists, it says the submitting run is not in the loaded
  record; it never concludes that a person asked for it.

The detail that should feel ahead: a figure plate on the contents page opens into the same figure at the top of its
analysis (a View Transition that carries the plate to its place), so the reader keeps hold of what they chose. The page
then keeps that figure in place while the figures above it finish drawing, and lets go as soon as the reader scrolls.
Reduced motion gets the page without the move.

## Where this came from

Every analysis page ends
its results with a "Where this came from" section, read from the broker's lineage record. It is set like a methods
note, not a diagram: a summary sentence in the serif, then ruled rows in reading order (Asked for by, Plan step, Data,
Method, Code, Ran in, Results, Review), each a plain sentence with qualifying notes beneath. Identifiers, digests,
image builds and resource limits sit under a closed Technical details list after the rows, so the
rows read as a methods note a scientist can follow without them.

- Every part the record does not hold is said in words on its row and marked with the editor's change bar in the
  margin; the summary counts those parts. A gap code the page does not know yet is still listed, under "Also not
  recorded", so the count and the margin always agree.
- Who asked is the reader ("You"), a named colleague by display name, a named service account, or "Not recorded"
  with the account it runs under. A run's analysis reads "The research agent, in turn 2 of a run you started." Nothing
  is filled in by guessing, and a template the record does not name is not named.
- The method says what ran in plain words (model-written R code under a reviewed template, a reviewed service tool, a
  research agent working with the method's tools), and the results row says which of those made the files.
- The plan step is the one first recorded for the analysis; a later revision of its wording is noted beside it,
  never put in its place.
- If the lineage cannot be read, the section says so and offers Try again; the rest of the page stays usable.
- The running head takes the analysis's project from the broker's own record. If that project cannot be read just
  now, the head says its project and who can open it could not be looked up; it never states access it does not know.

The detail that should feel ahead: from any analysis a scientist can read, in one place and in their own terms, who
asked, what data and method were used and what the record cannot vouch for, without opening the workbench.

## Lab index and research notebook

A review of the running workspace found that the landing did not say
what was what, the rail's flat list of project titles made no sense, and a project did not say plainly whether the
agent had finished and answered the question. "Agent runs", "revisions" and an empty "question and plan" were not
understood. This section replaces the rail's project list, the landing and the project page's order. The tokens, type,
figure treatment, lab letterhead, "Where this came from" and the refusals above stay.

The reader is a scientist with ten minutes who is not technical. They ask, in order: is it done, what did it find, can
I trust how it was made, and what do I do next. The page answers in that order and keeps every record one step away.

- **The landing is a lab index.** It borrows the catalog search scientists already use (a library or PubMed): facets
  in a narrow left column, results beside them. Facets are Lab (every lab the reader belongs to, with counts),
  Started by (You, Colleagues) and Where it stands (Working now, Waiting for you, Finished, Did not finish, Not
  started), each with its count, and one search box for titles and questions. On phones the facets fold into a
  Filter control that opens a sheet. When anything waits for the reader it leads the page as "Waiting for you",
  marked with the editor's change bar. Results group by lab, then "Started by you" and "Started by colleagues". A row
  is the title, the question in one serif line, one status sentence, who started it, who can open it and when it
  changed. Rows carry no figure plates: drawing them would open every project's result packages on each visit, so
  the project page leads with them instead. A sixth standing, Paused, covers a session a person paused.
- **The rail holds places, not projects.** Your research, Waiting for you with its count, each lab with its count,
  All results, and the Detailed view; Inspection for administrators. On a project the rail adds "This project" with
  its sections: Summary, Results, What the agent did, Checks, Data used, Downloads, People and access.
- **A project reads as a report, then a notebook.** The head names lab, title, who started it and when, who can open
  it, and a Simple or Detailed switch; Detailed opens the workbench on the same project. Then:
  1. Status: one large plain sentence and a step line, one short rule per plan step (finished steps in ink, the
     current one carrying the three-bar waveform, later ones faint), then the next actions as buttons.
  2. What the agent noted: its saved notes and hypotheses, each linked to the result it relies on and labeled as the
     agent's reading, not checked by a person. Its findings with bound values open "Did it answer your question?" in
     the status block (below). The page never says the question was answered unless a record says so.
  3. Results: every figure and table across the analyses, numbered, captioned, each with "How was this made?".
  4. What the agent did: a dated notebook in reading order. Starts and stops of the agent's work sessions, what it
     planned and why, each analysis with its outcome, the checks that ran, the reader's own instructions. An entry
     opens in place to Inputs, Method and code, Results, Checks and Where this came from.
  5. Checks, Data used (each input in plain words with its kind, such as public reference data), Downloads
     (everything, and how to rerun it on your own computer), People and access, the full record, Technical details.
- **Ask for changes.** When the agent has stopped or finished, a plain box under the status reads "Tell the agent what
  to change or do next". Sending it saves a new version of the question with the instruction under "Your
  instructions", attributed and dated, and starts the agent again with the same methods. The instruction stays in
  the notebook.
- **Words.** Work session, not run. Saved version, not revision or checkpoint. Step, not action. Analysis, not task.
  The agent's reading, not assessment. Detailed view, not workbench, on ordinary navigation.

The detail that should feel ahead is "Follow the thread": a result's plate carries itself into its analysis page (a
View Transition), which opens at that figure beside the inputs, code and checks behind it; "How was this made?" opens
the same page. Reduced motion jumps there.

As built: the summary names the standing in one large word pair ("Waiting for you", "Working now"),
the recorded sentence under it and a step line; "Did it answer your question?" shows the agent's newest reading,
labelled as unchecked, and says only the scientist can decide. "Ask for changes" saves the instruction as a
constraint that begins "Scientist's instruction: ", so the research agent reads it with the question and every later
saved version keeps it; the notebook shows it as the scientist's entry, attributed by the version that first carried
it. Detailed view is the workbench, reached from the Simple and Detailed switch in the project head.

As built: once the agent records findings, "Did it answer your question?" opens with its
answer in one sentence that says it is the agent's ("It says the results answer part of your question"), then each
finding as a serif sentence on an Aggie Blue rule. The values inside a finding are data, not the agent's words: they
come from the stored digest of the result file, set in the interface face with tabular figures on a thin rule, shown to
at most four significant digits (whole numbers as written, so one sentence reads alike whatever rounding the analysis
wrote) with the exact text, file, row and column in the title, and a line under each finding leads to
the analysis it came from. A value that cannot be read says so in italics; nothing is filled in from the agent's text.

As built: an analysis the agent configured instead of wrote says so wherever its code is
described. Its code section is "The recipe and its settings", with the chosen settings as a two-column list in the
record style, the figure note reads "A configured recipe in the reviewed template, with settings the agent chose", and
"Where this came from" lists the settings under the code row. The workbench calls the node "Analysis code" until the
record is read.

As built: a person's review reads like a referee's record. The analysis page's Review
section opens with one plain sentence on what a review claims and does not, then each review newest first under the
heading rule: the decision as a status mark with the reviewer's name ("Accepted by you", "Changes requested by Example
colleague"), the time, the note in the serif face as the person's own words, and what it covered by checksum. The form
lists the three decisions as full-width rows with a one-line meaning each; the chosen row takes the editor's mark on
its left edge, as a chosen file does. The newest review sits beside the analysis's status, in its lineage row with a
link down to the section, beside the analysis in the project report's results and as a line in its checks.

Refused here as well: progress percentages or time estimates the records do not hold, a claim that the question was
answered without a record, runtime words on ordinary navigation, and any number in the agent's prose that did not
come from a result file.

## The commons

The commons page (`#/commons`, "Commons: What every lab receives" in the rail)
extends the working paper: it lists the public reference data, papers and tools the labs share, one kind at a time
behind plain-word tabs, each record a ruled entry with its title, a serif summary, its class, rights and date, and a
status word ("Every lab reads it" with a drawn check, or "Commons only"). Identifiers and sources sit under Technical
details.

A person who leads the commons sees "Mark baseline" or "Withdraw the mark" on each record, and every active record not
yet marked carries the editor's change bar, as anything that needs the reader does. When the mark is made the bar draws
back into the margin (200 ms, from the top), so the list shows what still waits without a counter to reread; reduced
motion removes it at once. Anyone else reads the same list with no control and one sentence saying who can change it.
The workspace never decides a mark: the knowledge service does, from the person's own sign-in, and a refusal reads as
its sentence on the record.

## Portal navigation

Superseded by the Glass Box shell above: the sidebar holds the places, the bar carries search and
the Simple and Detailed switch, and crumbs sit at the top of the page column. Results as the jobs table and the
account menu stay as recorded here.

The workspace should be one that scientists who use Open OnDemand (the SCINet and campus HPC portals) recognize
at once, custom to this cluster and polished in every font, size, control and label. Open
OnDemand's shell is a dark top bar with the site name on the left, menus for Files, Jobs, Clusters and Interactive
Apps, and "My Interactive Sessions", Help and the account on the right; its home is a dashboard of pinned apps; jobs
are a sortable table with a status per row; pages carry a breadcrumb under the bar. This section borrows that shell
and its vocabulary where it fits research work, and changes nothing inside a project: the working paper, the lab
letterhead, the tokens, the type and the refusals above stay.

- **The top bar is the menu.** On wide screens the masthead plane carries the lab letterhead on the left, then the
  places a scientist moves between, in OnDemand's order of weight: Your research, Waiting for you (with its count),
  Results, Library (a menu: the lab's papers, the commons and shared notes), Labs (a menu of every lab with its project
  count) and Detailed view; administrators also see Inspection. Display and the account sit at the right. Start a
  project stays the filled control on the home page, where OnDemand keeps Launch on the app it starts. A menu opens on
  click or Enter, closes on Escape or a click outside, and every item is a link. Below 1,180 pixels the menu becomes the
  drawer it already was.
- **A breadcrumb under the bar.** Every page but the home names where it sits: Your research, then the lab, the
  project and the analysis, each a link. It replaces the single crumb beside the lab name.
- **No rail unless a project is open.** Wide pages use the full width. A project page keeps a left sidebar with the
  project's title and its sections, the way an OnDemand app keeps its launcher list beside the form.
- **The home starts like a dashboard.** Before the lab index sits one ruled row of starting points, drawn as text with
  a rule glyph rather than icon tiles: Waiting for you (with how many items ask for the reader), Run one method, the
  lab's library and Detailed view. The Waiting for you section then says what it counts: projects, while the bar's
  badge counts the items inside them, and each says so.
- **Results is the jobs table.** One row per analysis with its status mark, the question, the project, the method,
  when it was requested and how long it took, sortable by the column heads and filtered by status, the way scientists
  scan Active Jobs. Rows open the analysis. On phones each row folds to two lines.
- **The account says who you are once.** A menu from the initials holds the name, the address (once), the role in
  the current lab, and in faint small text "runs on enrichedlab".

Refused here as well: colored job-state badges (the status mark with its glyph and words stays), app-tile grids with
icons, a terminal or file-system vocabulary on ordinary navigation, and hover-only menus.

## Refused

Per-file-type icon sets, minimaps, a terminal pane, source-control decorations and syntax colors on anything that is
not code, in the workbench. Its explorer rows carry one monoline icon per kind of record (question, work
session, analysis, folder, file, steps, code, record, people, package), which is not a file-type set. Everywhere: invented logos or marks, cards around every item, small-capital labels behind a colored dash, tinted pills or
badges, colored callout boxes, full-width colored banners, gradients, glow, sparkle marks, green success states,
status dots pinned to corners, runtime vocabulary in ordinary navigation (lease, sandbox, pod, checkpoint, actor),
invented product names, and any client-drawn substitute for an R figure.
