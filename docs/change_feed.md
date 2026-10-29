# Change feed and live views

Each signed-in reader holds one server-sent event stream that names what to read again when a person,
a worker or the research coordinator changes something the reader can see. Views update without polling; each open
page costs one feed read per burst of change, and a replica runs at most four at once.

## What is recorded

Migration 061 adds `broker_changes`. A trigger writes one row in the same transaction as each change a workspace view
shows: projects and their saved versions, sharing, grants and ownership, plan records, an administrator's inspection
begun, ended or removed, the project's audit (exports among it), research runs, turns and deferrals, tasks and their
events, reviews, receipts, digests, result files a researcher may see (registered, or registered again with new
details) and their kinds, each attempt's recorded inputs, execution, outputs and platform tool jobs, scientific
history, working copies, figure versions and manuscript marks, template-analysis requests, datasets and preparations,
inbox acknowledgements and looks, lab notes, library papers and their events and labels, labs, lab memberships,
methods and the methods granted to a lab, a person's display name or sign-in turned off or on, and each new catalog
the render or analysis service reports. A row holds the keys to read again, the ids that decide who may see it, and
the writing transaction's id. It holds no title, text, value or research data.

Some writes are not changes, so they add no row: a task's or a research run's lease renewal, a run claimed and handed
back within one turn while its tools work, a look at a revision already seen, the sign-in rewrite of a membership row,
the custody dispatcher's periodic observation of a working copy, a catalog seed rerun that changes a lab or method
row's time only, a sign-in that keeps the person's name, a service reporting a catalog it reported before, and a file
only operators see. The catalog seed writes only what differs from the database, so seeding the same catalog again
adds no row; a seed that changes a lab or method resets every member of the labs it touches once.

A transaction adds one row per distinct change: an analysis that registers a thousand files in one transaction adds
one task row, and the work behind a task's keys (its project, dataset, source and the notes that cite it) runs once
per task per transaction. Readers read after the commit, so a second identical row would tell them nothing more.

The trigger notifies channel `broker_changes` with the tenant. PostgreSQL sends one notification per transaction at
commit, however many rows it wrote. Rows older than a day whose transactions finished before every running one are
pruned (`broker_prune_changes`, run by each API replica a minute after it starts and then hourly); a younger row stays
even when it sits below an older one in feed order, and the newest removed position is kept as the tenant's horizon.

## Keys

| Key | Read again |
|---|---|
| `projects` | the project list |
| `project:<id>` | the project, its runs, reviews and acknowledgements |
| `tasks` | the task list |
| `task:<id>` | the task, its reviews, files, lineage, figures and template analyses; a figure version or template analysis also names its source analysis, and a manuscript mark names the figure, the marked version, the version that held the mark before (when one did) and the source analysis |
| `dataset:<id>` | the dataset, with each of its steps' status and receipts |
| `lab:<key>` | the lab's papers, notes and people (members, who may be invited, names); a change to an analysis a shared note cites names the note's lab, since the note's state follows that analysis's review and files |
| `catalogs` | the render and analysis catalogs an open figure or analysis editor offers |
| `inbox` | the inbox and the Waiting for you count |

The workspace maps keys to reads (`clients/workspace/src/live.ts`). `commons` is reserved for the knowledge
commons, which the broker does not write; those views keep their timer. A membership or name change names
`lab:<key>`, which is the key for a project's list of members who may be invited. Two things end by the clock with no
write, so no row names them: a note's review date (`review_by`) and an administrator's inspection
(`inspection_expires_at` on the project). A view that shows either reads again at that time and when its tab becomes
visible.

## Who receives a change

`broker_change_feed` decides per reader, in the database, with the same rules as the reads:

- a row that names a project goes to people who may read the project (`broker_project_can`);
- else a row that names a task goes to people who may read the task (`broker_task_can`: a task outside any project is
  its submitter's alone);
- else a row that names a lab goes to the lab's members;
- a look or an acknowledgement goes only to the person who made it, and a look only while that person may still read
  its project;
- a `project:<id>` key goes only to people who may read that project, whatever row carries it: a shared note's row
  names its project, and a labmate who may not read the project learns only that the lab's notes changed;
- a `task:<id>` key that names a task besides the row's own (a source analysis, a figure's versions) goes only to
  people who may read that task (`broker_task_can`).

A change to someone's own access (a grant, a membership, an administrator enrollment or inspection, becoming or ceasing
to be a project's owner) sends that person `reset` with reason `access_changed`, even when they can no longer read what
changed. A change to a lab, its methods, a method it holds, or a project's visibility, owner or lab does the same for
every member of the lab (and of a lab the project left), a lab just disabled included, and for every administrator
inspecting one of the lab's projects from outside it. A reader who may not see a row
moves past it without learning anything about it.

## Order and resume

A position is `x<transaction id>-<row id>`. A reader receives only rows whose transactions are older than every
transaction still running (`pg_snapshot_xmin`), so a row that commits late is never skipped: a lower row id can commit
after a higher one, but a transaction id below the snapshot's lowest running one cannot. A write transaction left open
anywhere in the database cluster holds delivery back until it ends; it never loses a row. A resumed position must be
one the feed gave out: a row it still keeps, the horizon, or the empty start `x0-0`. Any other position, one from a
restored or newer database included, reads as `behind`, and so does any position at or above the watermark: every
position the feed gives out is below it, and the watermark never falls. Once every row is pruned the horizon is the
newest position, so an idle reader is not sent back again.

## The stream

`GET /api/v1/changes/stream[?after=<position>]`, authenticated like every route. `GET /api/v1/me` says
`change_stream: true` when it is served (`--change-stream`, `AGENT_SANDBOX_BROKER_CHANGE_STREAM`, default on); the
workspace opens the stream only then.

- `ready`, data `{}`, after any replay; its id is the current position.
- `changed`, data `{"keys": [...], "busy": {"<project id>": bool}}`; `busy` says whether an analysis or a research run
  still works in each named project the reader may read, and is left out when no project is named.
- `reset`, data `{"reason": "behind" | "access_changed"}`: read everything shown again. `behind` means the position is
  older than the pruned horizon, from a newer database, not one the feed gave out, more than 2,000 rows back, or that
  the change names more than 256 keys (the workspace refuses an event of more than 500).
- `stream_closed`, data `{"code": "principal_disabled"}`, when the reader may no longer sign in; no `ready` follows.
- `stream_complete`, data `{"reason": "renew"}`, at the 15-minute lifetime; a keepalive comment every 15 s.

One person holds at most 16 streams on a replica, one per open page; the next is refused with 429 and code
`change_streams_full` until one ends. A first connection gets only `ready`. A reconnection with `?after=` or the
browser's `Last-Event-ID` gets one `changed` with the union of what it missed, or `reset`, then `ready`. Every event's
id is the position after it.

Each API replica holds one `LISTEN` connection outside its pool, checks it every 30 s and reconnects with backoff. A
notification wakes every open stream on the replica; each waits 250 ms so a burst becomes one read, then reads the feed
from its own position. At most four feed reads run at once per replica, so streams never take the connection pool. A
stream also reads every 30 s without a notification, so a notification lost with a dropped connection still arrives.
Replicas share nothing in memory: each hears every notification.

Migration 061 takes every lock it needs before it changes anything and waits at most 2 s for each, below the services'
5 s, so on a busy database it fails whole and is run again unchanged instead of holding live writes back. Its new column
takes the strongest lock on `broker_research_run_events`, so reads of run events wait for the migration's own run.

## Conditional reads

Every JSON read under `/api/v1/` answers with a weak `ETag` that hashes its body, and a matching `If-None-Match` with
304 and no body. Two readers whose answers differ never share a tag. Responses keep `Cache-Control: no-store`: the
workspace holds the last body per address in the tab's memory and sends the tag itself, so nothing is written to the
browser's disk cache. Bodies over 8 MiB, streams and downloads carry no tag.

## Checks

`scripts/qualify_remote_broker.sh change-feed` applies migration 061 twice and checks, in isolated PostgreSQL: the
rows written and not written above, each reader's keys and the per-key project and task rules, one row per distinct
change in a transaction, files only operators see and files registered again, access changes for grants, roles,
inspections (removal included), owners, labs (an inspector from outside the lab included) and methods, names, sign-ins
turned off and catalogs, a quiet second seed of the same catalog, notes told when an analysis they cite changes,
commit order behind a running transaction, a position at or above the watermark, one notification for a transaction of
several changes, every `behind` case and the empty feed after a prune, a prune that keeps a younger row, busy
projects, each lab's dataset signal (`lab_data`), the readers granted to the API role only, the stream through a real
listener with resume, reset and the per-person limit, conditional reads, a project started from several sets, with the
option off refusing both `prepare` and a direct saved version, and the migration failing within its lock timeout while
a service holds a table. The `figure-versions` and `lab-datasets` modes check the keys a manuscript mark (set and
cleared) and a dataset's steps name.

## Capacity

One API replica serves the stream today. Each open page adds one feed read per burst of change,
whether or not the change concerns its reader. Scale-out replaces that with one read per burst
that names the readers a change concerns, so only their streams read. A load test with a few hundred simulated readers
is the measure before any capacity number is quoted, and before replicas are added (they first need shared artifact
storage).
