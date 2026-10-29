# Broker scientific history

The broker exposes paged canonical provenance while a supported legacy logical-agent
task is running. It does not create a second event stream. The worker retains only
the immutable state-export files it has already copied into broker artifact custody.

At controller start, the worker reads the controller's JSON `agent status` record
and accepts only an idle scientific logical agent whose name, principal owner,
state profile, server-issued `agent_id`, and `state_run_id` match the claimed task.
It binds the task's current attempt and lease generation to the next canonical
provenance segment. The controller reports the actual final segment after the run;
that immutable boundary closes the binding. If a stopped run cannot report its
final segment, a later server-issued binding for the same task/run caps the earlier
open range before its first segment.

`GET /api/v1/tasks/{task}/scientific-history` accepts only a server-returned
`v1.<attempt>.<lease-generation>.<sequence>` cursor. It never accepts a provenance
run ID. The endpoint first verifies task ownership through broker functions, then
reads the same PostgreSQL `provenance_runs` and `provenance_events` relation used by
the controller. It returns event sequence, protocol identity, lifecycle metadata,
hash-chain metadata, and an evidence count. It excludes event metadata JSON,
restricted content, content digests, prompts, tool arguments, and model reasoning.

For a selected event, `GET /api/v1/tasks/{task}/scientific-history/events/{event}/evidence`
requires the history attempt and lease generation plus a bounded artifact-ID cursor.
It returns explicit `observed_after` file-version references only: broker artifact
ID, SHA-256, size, media type, and role. The client resolves bytes through the
existing owner-scoped artifact download route. A reference records that a machine
snapshot observed the retained version after a terminal tool event. It does not
claim that the tool caused the change.

The legacy continuity producer observes only `/sandbox/work/evidence`, never the
whole workspace or `.agent-state`. Bind transport captures changed regular files
after each completed, failed, or errored tool receipt. Sync transport preserves the
same receipt set and captures it after its existing workspace pull. Both write the
closed machine-authored snapshot receipt with the exact canonical
event-ID/sequence/hash. `logs`, `reasoning`, and `trace` paths remain restricted.
The legacy worker accepts a standard reference only after it validates that receipt
and its already-retained researcher-visible artifact ID/SHA/size under the current
task attempt and lease generation. Repeating the same checked handoff is
idempotent. Restricted versions and model-authored snapshot records are not linked
or returned.

Coverage is `available` only after the broker can see the matching canonical run
with its exact subject. A bound active attempt is `pending` until that occurs; a
missing binding, an unsupported direct-Sandbox task, or a terminal binding whose
canonical run is absent is `unavailable`. This avoids claiming that an embedded or
separate controller provenance store is readable through the broker.

Direct Sandbox has no trusted observer/export producer yet, so it remains
`unavailable` for this evidence path. Its future producer must emit the same
closed snapshot receipt and retain original versions through the broker lease fence;
the table alone does not make direct task evidence available.

The legacy worker role may create/finalize its exact lease-fenced binding and record
checked evidence links. The API role may call only the owner-scoped status,
history, and evidence reader functions. Both roles have no table privilege.
Migration `009_broker_scientific_history_evidence.sql` keeps migration 007's
already-granted history reader intact and adds a separately granted
evidence-aware reader. Apply both migrations and rerun
`grant_broker_runtime.psql` as the migration owner before enabling these API and
worker calls. This source change does not apply either one.
