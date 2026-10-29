# PostgreSQL provenance setup

Use separate identities for schema changes and controller traffic. The migration identity owns the
`enrichedlab_agent` schema. The runtime identity has no superuser, role-management, schema-creation, or row-security
bypass authority.

1. Create the database and two login roles through the database platform. Keep credentials in its secret store.
2. Set `AGENT_SANDBOX_POSTGRES_MIGRATION_DSN` to the migration-owner connection and
   `AGENT_SANDBOX_POSTGRES_DSN` to the runtime connection.
3. Configure the controller and apply the migration:

   ```bash
   agent-sandbox provenance configure --backend postgres --tenant <tenant-id>
   agent-sandbox provenance prepare
   ```

4. Grant the runtime role its fixed table privileges as the migration owner:

   ```bash
   psql "$AGENT_SANDBOX_POSTGRES_MIGRATION_DSN" \
     --set=runtime_role=<runtime-role> \
     --file=migrations/postgres/grant_runtime.psql
   ```

5. Remove the migration DSN from the long-running controller environment. Keep only the runtime DSN there.

The controller sets `enrichedlab.tenant_id` on each connection and the tables force row-level security. A role with
`BYPASSRLS`, superuser authority, schema ownership, or table ownership can cross or change that boundary. Do not use a
database owner or migration identity for controller traffic.

Plaintext PostgreSQL is accepted only when every address in the DSN is loopback. Remote connections need TLS. For
Kubernetes, prefer a database-platform issued client certificate or a rotated Secret and `sslmode=verify-full`. Do not
put either DSN in `controller.toml`, a manifest, a command transcript, or Git.

## Remote broker roles

The shared broker adds function-only runtime roles. Apply the broker migrations with the migration owner, create
separate API, worker, and campaign-director logins and, when trusted custody is enabled, a distinct custodian login.

Before migration 075, a superuser grants the migration owner `SET` on the tenant parameter once:

```sql
GRANT SET ON PARAMETER enrichedlab.tenant_id TO <migration-owner>;
```

The evidence functions pin `enrichedlab.tenant_id` for the length of each call, so a tenant one of them sets never
outlives it, and PostgreSQL 15 and later let a non-superuser define a function with that clause only when it holds `SET`
on the parameter. Without the grant, migration 075 stops with `permission denied to set parameter`.
Then apply:

```bash
psql "$AGENT_SANDBOX_BROKER_MIGRATION_DSN" \
  --set=api_role=<api-role> \
  --set=worker_role=<worker-role> \
  --set=director_role=<campaign-director-role> \
  --set=custodian_role=<custodian-role> \
  --file=migrations/postgres/grant_broker_runtime.psql
```

The API role can resolve invited identities and access only that principal's tasks and researcher-visible artifacts.
The worker role can claim, renew, finish, and register artifacts through generation-fenced functions. The optional
custodian role can only read its current input or terminal custody snapshot and admitted code, check cancellation,
and renew that exact lease. The campaign-director role can only claim, renew, read, apply, and yield generation-fenced
campaign work, start a bounded campaign step, append a model-planner start under that lease, and finish only that
already-started call after a generation fence. It has no table,
sequence, schema-owner, migration, API, or worker authority.
Provisioning checks the director login before changing it, and director startup repeats the check; either refuses
role memberships, privileged attributes, object ownership, direct object grants, or effective access outside the
listed campaign functions.
All runtime roles get a 30-second statement
and idle-transaction timeout plus a 5-second lock timeout. The catalog seed runs as the migration owner and reconciles
groups, profiles, grants, invitations, membership revocation, and principal enablement.

Run `scripts/qualify_remote_broker.sh custodian-role` for the focused third-role check, or
`scripts/qualify_remote_broker.sh` before a release. It uses a disposable pinned PostgreSQL container and checks role
separation, invitations, revocation, quotas, idempotency, cross-principal denial, custody lease fencing, cancellation,
append-only events, and artifact visibility. The cluster uses mounted DSN Secret files and never mounts the migration
credential into an API, worker, custodian, or campaign-director workload.

Migration 066 adds autonomous research campaign records, the catalog-owned
`research.discovery.autonomous` capability, reserved campaign principals, forced tenant row security, append-only
campaign events, generation-fenced director functions, database budget and scope checks, and the migration-owner
stop-all function. The campaign principal uses the `researcher` membership role, the smallest role that can own its
private projects; project ownership provides its run authority. Stopping drains existing runs before disabling that
principal so their receipts and evidence remain complete. Run `scripts/qualify_remote_broker.sh campaigns`. See
[research campaigns](../../docs/campaigns.md).

Migration 067 adds the model-backed campaign planner's append-only call ledger and content-addressed context and reply
texts. The director records a start and exact context under its current generation-fenced lease before network I/O. A
linked terminal record may finish that call after the generation changes, but cannot apply campaign work. Current autonomous-discovery
capability holders for the campaign's lab can page the call metadata and read only a digest a call names. The director
view also supplies bounded method labels and starting-point keys for context compilation. The new call writer is added
to the function-only director contract; no table or sequence grant is added. Run
`scripts/qualify_remote_broker.sh campaigns` and `scripts/qualify_migration_drafts.sh`.

Migration 012 serializes first enrollment by tenant, issuer and subject before the principal lookup. Concurrent
requests for a new identity resolve the same principal and memberships; different identities remain independent.
Invitation consumption and disabled-account checks are unchanged. Run
`scripts/qualify_remote_broker.sh identity-enrollment` for the focused concurrency and revocation regression.

Migration 013 adds operator-only recovery of verified files from retained output volumes. No runtime role gets
execution permission. `broker_recover_artifacts` binds the original custodian terminal record to its checksum,
task identity and recorded terminal runtime event. The operator must separately verify the retained PVC UID and
copy/hash the exact selected bytes into the private recovery path. Registration atomically appends an immutable
recovery receipt and task event; it preserves the failed/cancelled state and all original evidence. Exact replay is
idempotent and changed replay is refused. The API's existing owner checks still govern downloads.

Run `scripts/qualify_remote_broker.sh artifact-recovery` for the focused registration, fencing and role checks.

Migration 014 adds the scientist review surface: append-only research-run events, a tenant administrator audit,
owner-only collaborator listing, administrator inspection, and the one-snapshot project evidence package with its
export audit. Only the API role gets the new read and audit functions. Run `scripts/qualify_remote_broker.sh review`.

Migration 015 adds model-written code for reviewed templates. `broker_profile_templates` is operator configuration;
`broker_task_generated_code` is written only by the coordinator's commit function, in the same transaction as its
task, and is append-only. The worker role reads a digest-only reference and the custodian role the exact bytes, each
through a function fenced to its own current lease; the API role reads a block or a project's blocks only when the
caller can read them. Run `scripts/qualify_remote_broker.sh generated-code`.

Migration 052 adds `broker_get_group_methods`, a read the API alone may run: for each of the reader's labs, the enabled
methods granted to that lab. The identity view carries them as each group's `profiles`, so the workspace offers a lab
only its own methods; submissions keep their own checks. The API is not ready on a schema without 052. Run
`scripts/qualify_remote_broker.sh lab-methods`.

## Migrations no release carries yet

`migrate` applies every migration on each run, so each is idempotent. A migration that no published release carries
may still be edited, and any committed draft of it may have run on a qualification or development database, so an edit
must also bring such a database to the current shape: a table an earlier draft made gains or loses columns and checks
in a block that tests for the old shape, and a function whose result type changed is dropped first only when its
recorded result differs (dropping one removes its grants; apply the grant files again after migrating, as always). Run
`scripts/qualify_migration_drafts.sh` after editing one: it applies each committed draft of the migrations from
`ENRICHEDLAB_BROKER_FIRST_UNPUBLISHED` on (default 061; the newest published release carries 054 to 060) to its own database, upgrades it with the current set, and
compares the result with a fresh database (relations, columns, constraints, indexes, functions with their owner,
arguments and defaults, properties, source and grants, triggers, views, types and row policies; column order and the names PostgreSQL gives unnamed checks aside).
Every database gets the runtime grant files with test roles after its migrations, as a deployment does, and the current
set applied again must change nothing before the grants are applied again, so a migration that drops and recreates a
granted function on every run fails. Raise the variable when a release carries more migrations.

Migration 053 admits live lab database sources (`data_class` `lab_database` with a live snapshot naming the relation in
which the database states its version), adds each query receipt's `source_version`, and a receipt function that takes
it; the 13-argument form an older worker calls records no version, so it cannot complete a lab source's query. The
worker is not ready to serve queries on a schema without 053 or without EXECUTE on the 14-argument receipt function
(`grant_broker_runtime.psql`). Run `scripts/qualify_remote_broker.sh reference-sql`.

Migration 061 adds the change feed: `broker_changes`, written by triggers in the transaction of each
change a workspace view shows, with one notification per transaction on channel `broker_changes`, and the API-only
readers `broker_change_feed`, `broker_projects_busy` and `broker_prune_changes` ([change feed](../../docs/change_feed.md)).
It also adds `broker_get_group_lab_data` for each lab's dataset signal on `/api/v1/me`, and keeps a call's reasoning
text beside its reply: the reader of kept texts serves `reasoning_sha256`, and the nine-argument
`broker_yield_research_run` names a refused reply's reasoning (the eight-argument form an older coordinator calls stays).
The API and the coordinator are not ready on a schema without 061 or without EXECUTE on these functions. Run
`scripts/qualify_remote_broker.sh change-feed` and `decision-contexts`.

Migration 063 lets a lab member read the texts of a methods reply that was refused and then corrected in
the same turn: the reader of kept texts also serves the four addresses under the turn record's
`methods_specialist_refused.receipt`. The coordinator, which writes that receipt, is not ready on a schema without
063; an earlier coordinator never writes it.
Run `scripts/qualify_remote_broker.sh decision-contexts`.

Migration 064 returns a decision's answer tables to the next lead call: the research run's results take
the newest decision that stated findings or gave answer tables, with both lists, and count the newest answer's tables.
The coordinator, which writes answer tables, is not ready on a schema without 064; an earlier one never writes
them and reads the same findings from either definition. Run `scripts/qualify_remote_broker.sh research-runs`.

Migration 068 adds the append-only project package registry. The API allocates revisions through a
per-project-and-disclosure-scope transaction lock, reuses any earlier revision when its digest inventory is unchanged,
and reads the extra per-run reasoning, result-digest, task-origin, and paper records used by the evidence package.
Exact kept reasoning is
returned only to a current lab member who is also an ordinary project reader by ownership, lab visibility, or a
project grant; a read-only administrator inspection does not qualify. Revision history and changes are isolated among
ordinary readers with reasoning, ordinary readers without reasoning, and inspections; the latter two retain paper IDs
without lab-library metadata so each chain has one disclosure ceiling. Apply the migration before starting the API that reads it,
then rerun the runtime grant script so the API role can call both package functions.

Migration 070 adds immutable, input-addressed literature review outcomes by run. The coordinator's
leased reader/writer requires current lab membership; the API reader requires project access and lab membership.
The leased writer validates reading rounds and accepts checked reviews or refusals; the API review reader serves
checked reviews only. Kept-text reads also cover review calls, and member package components carry both outcomes
with their status and guard malformed older rounds. Apply the migration and rerun both runtime and coordinator
grants before enabling `--literature-review-labs`; an enabled coordinator refuses to start without it.
Run `scripts/qualify_remote_broker.sh lead-library`.

Migration 080 adds tool groups: immutable, versioned sets of profiles stored by the catalog seed,
their members, and lab grants that are revoked with a time rather than deleted; row triggers refuse updates and
deletes, and statement triggers refuse TRUNCATE. The seed computes each lab's profile
grants as its direct grants plus the members of the versions it holds. `broker_lab_tools` is the API's member-only read
of a lab's tools and their catalog fields, by the knowledge export's version 6 projections. The seed writes only rows that
differ: an unchanged catalog changes no tenant, lab, method, invitation, capability, principal or membership row. Apply the migration, rerun
the runtime grant script (the API's readiness waits for its grant), then seed a catalog with `tool_groups` and
`tool_group_grants`; an older broker's seed removes profile grants only a tool group held. Run
`scripts/qualify_remote_broker.sh tool-groups`.

Migration 081 records the broker's rows in the research record through an outbox. An ordinary trigger on each
append-guarded table, and an update trigger on the three that permit one update (a tool group grant's revocation, a
dataset's intake task, a reviewed query's retirement), writes each change to `evidence.capture_log` with its writing
transaction's start time and role, and takes no other lock. The recorder, `evidence.capture_record(limit)`, which the
evidence append service runs every few seconds (`--capture-interval-seconds`, 0 turns it off) through the procedure
`evidence.capture_record_now`, which commits before and after recording so no caller holds its locks, takes no lock
by waiting and defers a batch whose lock another session holds, takes committed entries in order, derives each record with the history import's rules and appends it as the tenant's capture producer,
`broker:capture`, deleting the entries in the same transaction. No session can name that producer: `evidence.append`
refuses it, and its unauthenticated core, `evidence.append_as`, runs only inside the migration owner's functions. The
log, the recorder's lock row and the opt-in table are owner-only. A row that cannot be placed gets an owner-only,
immutable entry in `evidence.capture_gaps`. Capture is opt-in per tenant: entries of a tenant that has not opted in
are discarded. As the migration owner's session, run `evidence.capture_enable(tenant, standalone lab)`, run the
recorder until the log holds none of the tenant's entries, then `evidence.capture_catchup(tenant)` (rows written
before migration 081), then `evidence.capture_missing(tenant)`, which must report nothing missing; an entry not yet
recorded counts as pending. Deleting the tenant's row in `evidence.capture_tenants` stops capture. A new
append-guarded table, guard function or captured column fails the evidence check until it is reviewed and listed. Run
`scripts/qualify_remote_broker.sh evidence`, and any mode with `QUALIFY_EVIDENCE_CAPTURE=1`.
