-- History import qualification (migration 078). Runs as the database superuser in the disposable qualification
-- container after the broker migrations and the evidence fixtures. The superuser owns the functions here, as the
-- migration owner does in a deployment, so it is the one role that may import. Every check raises on failure.
\set ON_ERROR_STOP on
SET client_min_messages = warning;

-- Synthetic site: one tenant, two labs, a project in lab A with one run, a task in that project and a task in lab B
-- with no project, and nine rows in eight append-guarded tables. Parent rows the broker would create through its
-- own functions are written directly with triggers off (session_replication_role), then triggers are back on.
SELECT set_config('enrichedlab.tenant_id', 'import-site', false);
INSERT INTO enrichedlab_agent.broker_tenants (tenant_id, name) VALUES ('import-site', 'Import site');
INSERT INTO enrichedlab_agent.broker_research_groups (tenant_id, group_key, name, max_active_agents,
    max_active_agents_per_principal, max_queued_tasks, retention_days)
  VALUES ('import-site', 'lab-a', 'Lab A', 4, 2, 16, 30), ('import-site', 'lab-b', 'Lab B', 4, 2, 16, 30);
INSERT INTO enrichedlab_agent.broker_principals (tenant_id, principal_id, issuer, subject, email, principal_class)
  VALUES ('import-site', '10000000-0000-4000-8000-000000000001', 'https://issuer.example.invalid', 'import-person',
          'import-person@example.invalid', 'person');
INSERT INTO enrichedlab_agent.broker_projects (tenant_id, project_id, group_key, owner_id, title, visibility)
  VALUES ('import-site', '10000000-0000-4000-8000-0000000000a1', 'lab-a', '10000000-0000-4000-8000-000000000001',
          'Import project', 'lab'),
         ('import-site', '10000000-0000-4000-8000-0000000000b1', 'lab-b', '10000000-0000-4000-8000-000000000001',
          'Other project', 'lab');
INSERT INTO enrichedlab_agent.broker_research_runs (tenant_id, run_id, project_id, actor_id, initial_revision,
    allowed_profiles, max_turns, max_parallel, status)
  VALUES ('import-site', '10000000-0000-4000-8000-00000000a0a1', '10000000-0000-4000-8000-0000000000a1',
          '10000000-0000-4000-8000-000000000001', 0, ARRAY['analysis'], 4, 1, 'failed');

SET session_replication_role = replica;
INSERT INTO enrichedlab_agent.broker_tasks (tenant_id, task_id, principal_id, group_key, profile_key, objective, status,
    desired_status, attempt, max_task_attempts, logical_agent, profile_snapshot, idempotency_key, request_sha256,
    lease_owner, lease_generation, lease_expires_at, submitted_at)
  VALUES ('import-site', '10000000-0000-4000-8000-0000000000c1', '10000000-0000-4000-8000-000000000001', 'lab-a',
          'general-research', 'Synthetic task in the project', 'completed', 'run', 1, 1,
          'task-' || replace('10000000-0000-4000-8000-0000000000c1', '-', ''), '{}'::jsonb,
          'import-task-c1', repeat('a', 64), NULL, 0, NULL, '2026-10-01T10:00:00Z'),
         ('import-site', '10000000-0000-4000-8000-0000000000c2', '10000000-0000-4000-8000-000000000001', 'lab-b',
          'general-research', 'Synthetic task with no project', 'completed', 'run', 1, 1,
          'task-' || replace('10000000-0000-4000-8000-0000000000c2', '-', ''), '{}'::jsonb,
          'import-task-c2', repeat('b', 64), NULL, 0, NULL, '2026-10-01T11:00:00Z');
INSERT INTO enrichedlab_agent.broker_project_tasks (tenant_id, project_id, task_id, action_key, parent_task_id)
  VALUES ('import-site', '10000000-0000-4000-8000-0000000000a1', '10000000-0000-4000-8000-0000000000c1', 'step-1', NULL);
INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind, state, code, metadata, occurred_at)
  VALUES ('import-site', '10000000-0000-4000-8000-0000000000c1', 1, 'state', 'succeeded', 'done',
          '{"note":"synthetic"}', '2026-10-01T10:05:00Z'),
         ('import-site', '10000000-0000-4000-8000-0000000000c2', 1, 'state', 'succeeded', 'done later',
          '{"note":"synthetic"}', '2026-10-01T11:05:00Z'),
         -- A task that does not exist: its scope does not resolve, so it is not imported.
         ('import-site', '10000000-0000-4000-8000-0000000000c9', 1, 'state', 'succeeded', 'done',
          '{"note":"synthetic"}', '2026-10-01T12:05:00Z');
-- More than a hundred unresolved rows in one table: every one must be named in the seal's receipt.
INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind, state, code, metadata, occurred_at)
  SELECT 'import-site', '10000000-0000-4000-8000-0000000000c9', n, 'state', 'succeeded', 'done', '{"note":"synthetic"}',
         '2026-10-01T12:06:00Z' FROM generate_series(2, 102) AS n;
-- Scope identifiers that disagree: the run is in lab A's project, the project named is lab B's.
INSERT INTO enrichedlab_agent.broker_document_request_keys (tenant_id, request_id, task_id, principal_id, project_id,
    run_id, kind, created_at)
  VALUES ('import-site', '10000000-0000-4000-8000-0000000000d1', '10000000-0000-4000-8000-0000000000c1',
          '10000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-0000000000b1',
          '10000000-0000-4000-8000-00000000a0a1', 'docx', '2026-10-02T11:00:00Z');
INSERT INTO enrichedlab_agent.broker_plan_revisions (tenant_id, project_id, revision, source, run_id, turn, principal_id,
    step_changes, recorded_at)
  VALUES ('import-site', '10000000-0000-4000-8000-0000000000a1', 1, 'person', NULL, NULL,
          '10000000-0000-4000-8000-000000000001', 1, '2026-10-02T09:00:00Z');
INSERT INTO enrichedlab_agent.broker_plan_events (tenant_id, project_id, revision, action_key, step_number, changes,
    status_before, status_after, task_id, result_count, title, rationale, task_status, error_code)
  VALUES ('import-site', '10000000-0000-4000-8000-0000000000a1', 1, 'step-1', 1, ARRAY['planned'], NULL, 'pending', NULL,
          0, 'A synthetic step title', 'A synthetic rationale', NULL, NULL);
-- A turn record over the 64 KiB payload limit: it is model text, so the record carries only its digest.
INSERT INTO enrichedlab_agent.broker_research_turns (tenant_id, run_id, turn, generation, context_revision,
    checkpoint_revision, record, created_at)
  VALUES ('import-site', '10000000-0000-4000-8000-00000000a0a1', 1, 1, 1, 1,
          jsonb_build_object('reply', repeat('synthetic reply text ', 4000)), '2026-10-02T10:00:00Z');
INSERT INTO enrichedlab_agent.broker_research_contexts (tenant_id, sha256, content, created_at)
  VALUES ('import-site', 'sha256:' || encode(sha256(convert_to('synthetic context', 'UTF8')), 'hex'),
          convert_to('synthetic context', 'UTF8'), '2026-10-02T09:59:00Z');
INSERT INTO enrichedlab_agent.broker_admin_audit (tenant_id, event_id, actor_id, kind, detail, occurred_at)
  VALUES ('import-site', '10000000-0000-4000-8000-0000000000e1', '10000000-0000-4000-8000-000000000001',
          'synthetic', '{"change":"synthetic"}', '2026-10-03T08:00:00Z');
INSERT INTO enrichedlab_agent.provenance_events (tenant_id, run_id, sequence, event_id, subject, segment, harness,
    provider, model, engine, parser, session_id, kind, evidence_role, visibility, actor, occurred_at, source_event_id,
    parent_source_event_id, metadata, restricted_content, content_sha256, previous_hash, hash)
  VALUES ('import-site', '10000000-0000-4000-8000-00000000a0a1', 1, '10000000-0000-4000-8000-0000000000f1', 'turn', 1,
          'synthetic', 'local', 'synthetic-model', 'synthetic-engine', 'none', 'session-1', 'reply', 'reasoning',
          'restricted', 'model', '2026-10-02T10:00:01Z', 'event-1', NULL, '{}', 'synthetic restricted reasoning',
          repeat('c', 64), repeat('0', 64), repeat('d', 64)),
         -- A text time that is not RFC 3339: digested, and the run's creation time is used.
         ('import-site', '10000000-0000-4000-8000-00000000a0a1', 2, '10000000-0000-4000-8000-0000000000f2', 'turn', 1,
          'synthetic', 'local', 'synthetic-model', 'synthetic-engine', 'none', 'session-1', 'reply', 'reasoning',
          'restricted', 'model', 'yesterday evening', 'event-2', 'event-1', '{}', NULL,
          repeat('c', 64), repeat('d', 64), repeat('e', 64)),
         -- A token-shaped text time, and one in RFC 3339 form with an impossible date: both digested, not copied.
         ('import-site', '10000000-0000-4000-8000-00000000a0a1', 3, '10000000-0000-4000-8000-0000000000f3', 'turn', 1,
          'synthetic', 'local', 'synthetic-model', 'synthetic-engine', 'none', 'session-1', 'reply', 'reasoning',
          'restricted', 'model', 'not-a-time', 'event-3', 'event-2', '{}', NULL,
          repeat('c', 64), repeat('e', 64), repeat('f', 64)),
         ('import-site', '10000000-0000-4000-8000-00000000a0a1', 4, '10000000-0000-4000-8000-0000000000f4', 'turn', 1,
          'synthetic', 'local', 'synthetic-model', 'synthetic-engine', 'none', 'session-1', 'reply', 'reasoning',
          'restricted', 'model', '2026-13-45T10:00:00Z', 'event-4', 'event-3', '{}', NULL,
          repeat('c', 64), repeat('f', 64), repeat('a', 64));
SET session_replication_role = origin;

-- Rows per append-guarded table for this tenant, before the import.
CREATE TEMP TABLE import_rows AS
  SELECT it.table_name, x.n FROM evidence.import_tables() it
  CROSS JOIN LATERAL (SELECT 0::bigint AS n) x;
DO $$
DECLARE
  t record;
  c bigint;
BEGIN
  FOR t IN SELECT table_name FROM evidence.import_tables() LOOP
    EXECUTE format('SELECT count(*) FROM enrichedlab_agent.%I WHERE tenant_id = %L', t.table_name, 'import-site')
      INTO c;
    UPDATE import_rows SET n = c WHERE table_name = t.table_name;
  END LOOP;
  IF (SELECT sum(n) FROM import_rows) <> 115 OR (SELECT count(*) FROM import_rows) <> 55 THEN
    RAISE EXCEPTION 'expected 115 synthetic rows in 55 tables, found % in %', (SELECT sum(n) FROM import_rows),
      (SELECT count(*) FROM import_rows);
  END IF;
END $$;

-- Another role cannot import.
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'import_probe') THEN
    CREATE ROLE import_probe LOGIN NOSUPERUSER NOBYPASSRLS;
  END IF;
END $$;
SET SESSION AUTHORIZATION import_probe;
DO $$ BEGIN
  PERFORM evidence.import_begin('import-site');
  RAISE EXCEPTION 'another role began an import';
EXCEPTION WHEN insufficient_privilege THEN NULL;
END $$;
RESET SESSION AUTHORIZATION;

-- Import every table in batches of 2, then check the result.
CREATE TEMP TABLE import_results (result jsonb);
DO $$
DECLARE
  t record;
  r jsonb;
  offset_at bigint;
BEGIN
  PERFORM evidence.import_begin('import-site');
  FOR t IN SELECT table_name FROM evidence.import_tables() LOOP
    offset_at := 0;
    LOOP
      r := evidence.import_history('import-site', t.table_name, 'lab-a', offset_at, 2);
      INSERT INTO import_results VALUES (r);
      EXIT WHEN (r->>'rows')::int < 2;
      offset_at := offset_at + 2;
    END LOOP;
  END LOOP;
END $$;

DO $$
DECLARE
  producer uuid;
  missing text;
  rec record;
  p jsonb;
BEGIN
  SELECT producer_id INTO producer FROM evidence.producers WHERE tenant_id = 'import-site' AND identity = 'broker:history';
  -- One record per row, per table, except the two rows whose scope does not resolve.
  SELECT string_agg(ir.table_name, ', ') INTO missing FROM import_rows ir
    WHERE ir.n - CASE ir.table_name WHEN 'broker_task_events' THEN 102 WHEN 'broker_document_request_keys' THEN 1 ELSE 0 END
      <> (SELECT count(*) FROM evidence.records r WHERE r.tenant_id = 'import-site' AND r.producer_id = producer
          AND r.event_type = evidence.import_event_type(ir.table_name));
  IF missing IS NOT NULL THEN RAISE EXCEPTION 'record counts differ from row counts: %', missing; END IF;
  IF (SELECT sum((result->>'unresolved')::int) FROM import_results) <> 103 THEN
    RAISE EXCEPTION 'expected 103 unresolved rows, found %', (SELECT sum((result->>'unresolved')::int) FROM import_results);
  END IF;
  -- A copy-candidate text value with a space is digested, not copied.
  IF NOT EXISTS (SELECT 1 FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.task_events'
                 AND scope_kind = 'lab' AND payload::jsonb->'digests' ? 'code' AND NOT (payload::jsonb->'fields' ? 'code')) THEN
    RAISE EXCEPTION 'a text value with a space was copied';
  END IF;
  -- An invalid text time (free text, a token, an impossible date) is digested and the run's creation time is used.
  FOR rec IN SELECT * FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.provenance_events'
      AND payload::jsonb->'key'->>'sequence' IN ('2', '3', '4') LOOP
    IF rec.payload::jsonb->>'time' <> 'parent' OR rec.payload::jsonb->'fields' ? 'occurred_at'
      OR NOT (rec.payload::jsonb->'digests' ? 'occurred_at') OR rec.payload LIKE '%yesterday evening%'
      OR rec.payload LIKE '%not-a-time%' OR rec.payload LIKE '%2026-13-45%' THEN
      RAISE EXCEPTION 'invalid time imported wrongly: %', left(rec.payload, 400);
    END IF;
  END LOOP;
  IF (SELECT count(*) FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.provenance_events'
      AND payload::jsonb->'key'->>'sequence' IN ('2', '3', '4')) <> 3 THEN
    RAISE EXCEPTION 'expected three records with invalid times';
  END IF;

  -- The oversized turn record: model text by digest only, in the run's stream.
  SELECT * INTO rec FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.research_turns';
  p := rec.payload::jsonb;
  IF rec.run_id IS DISTINCT FROM '10000000-0000-4000-8000-00000000a0a1' OR p->'fields' ? 'record'
    OR p->'digests'->'record'->>'class' <> 'model-text' OR NOT (p->'normalised' ? 'record')
    OR octet_length(rec.payload) > 4096 OR p->>'origin' <> 'imported' OR p->>'scope' <> 'run' THEN
    RAISE EXCEPTION 'turn record imported wrongly: %', left(rec.payload, 400);
  END IF;
  IF rec.payload LIKE '%synthetic reply text%' THEN RAISE EXCEPTION 'model text reached the record'; END IF;

  -- The research context: bytes digested; the digest equals the row's own key.
  SELECT * INTO rec FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.research_contexts';
  p := rec.payload::jsonb;
  IF 'sha256:' || (p->'digests'->'content'->>'sha256') <> (p->'key'->>'sha256')
    OR p->'digests'->'content'->>'encoding' <> 'bytes' OR rec.scope_kind <> 'standalone' OR rec.group_key <> 'lab-a' THEN
    RAISE EXCEPTION 'context record imported wrongly: %', left(rec.payload, 400);
  END IF;

  -- No time column: the parent's time.
  SELECT * INTO rec FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.plan_events';
  IF rec.payload::jsonb->>'time' <> 'parent' OR rec.source_time <> '2026-10-02T09:00:00Z'::timestamptz
    OR rec.payload LIKE '%synthetic rationale%' THEN
    RAISE EXCEPTION 'plan event imported wrongly: % %', rec.source_time, left(rec.payload, 400);
  END IF;
  SELECT * INTO rec FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.project_tasks';
  IF rec.payload::jsonb->>'time' <> 'parent' OR rec.source_time <> '2026-10-01T10:00:00Z'::timestamptz THEN
    RAISE EXCEPTION 'project task imported wrongly: %', rec.source_time;
  END IF;

  -- Task events: the project's stream for the task in it, the lab's for the task without one.
  IF NOT EXISTS (SELECT 1 FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.task_events'
                 AND scope_kind = 'project' AND project_id = '10000000-0000-4000-8000-0000000000a1'
                 AND payload::jsonb->>'scope' = 'project')
    OR NOT EXISTS (SELECT 1 FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.task_events'
                   AND scope_kind = 'lab' AND group_key = 'lab-b' AND payload::jsonb->>'scope' = 'lab') THEN
    RAISE EXCEPTION 'task events scoped wrongly';
  END IF;

  -- Tenant-wide audit: standalone in the named lab, its JSON detail by digest.
  SELECT * INTO rec FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.admin_audit';
  IF rec.scope_kind <> 'standalone' OR rec.payload::jsonb->'fields' ? 'detail'
    OR rec.payload::jsonb->'fields'->>'kind' <> 'synthetic' THEN
    RAISE EXCEPTION 'admin audit imported wrongly: %', left(rec.payload, 400);
  END IF;

  -- Provenance events: a text run id still finds the run; restricted content is model text by digest.
  SELECT * INTO rec FROM evidence.records WHERE tenant_id = 'import-site' AND event_type = 'import.provenance_events'
    AND payload::jsonb->'key'->>'sequence' = '1';
  IF rec.run_id IS DISTINCT FROM '10000000-0000-4000-8000-00000000a0a1' OR rec.payload::jsonb->>'time' <> 'row'
    OR rec.payload::jsonb->'digests'->'restricted_content'->>'class' <> 'model-text'
    OR rec.payload LIKE '%synthetic restricted reasoning%' THEN
    RAISE EXCEPTION 'provenance event imported wrongly: %', left(rec.payload, 400);
  END IF;

END $$;

-- Every stream the import wrote is a sound chain.
DO $$
DECLARE
  s record;
  r record;
  previous text;
  producer_identity text;
BEGIN
  FOR s IN SELECT DISTINCT st.* FROM evidence.streams st JOIN evidence.records rr ON rr.tenant_id = st.tenant_id
           AND rr.stream_id = st.stream_id WHERE st.tenant_id = 'import-site' LOOP
    previous := repeat('0', 64);
    FOR r IN SELECT * FROM evidence.records WHERE tenant_id = s.tenant_id AND stream_id = s.stream_id ORDER BY sequence LOOP
      SELECT identity INTO producer_identity FROM evidence.producers
        WHERE tenant_id = r.tenant_id AND producer_id = r.producer_id;
      IF r.previous_hash <> previous OR r.record_hash <> encode(sha256(convert_to(evidence.record_material_v1(
           r.tenant_id, r.stream_id, r.sequence, r.record_id, r.group_key, r.scope_kind, r.project_id, r.run_id,
           producer_identity, r.event_type, r.actor_role, r.source_time, r.ingested_at, r.payload_sha256,
           r.previous_hash), 'UTF8')), 'hex')
         OR r.payload_sha256 <> encode(sha256(convert_to(r.payload, 'UTF8')), 'hex') THEN
        RAISE EXCEPTION 'chain broken in stream % at %', s.stream_id, r.sequence;
      END IF;
      previous := r.record_hash;
    END LOOP;
    IF previous <> s.head_hash THEN RAISE EXCEPTION 'stream % head differs', s.stream_id; END IF;
  END LOOP;
END $$;

-- Every row against the record: matched or unresolved, nothing missing, no digest differs, no orphan or duplicate.
DO $$
DECLARE
  v jsonb;
  bad text;
BEGIN
  v := evidence.import_verify('import-site', 'lab-a');
  SELECT string_agg(key || '=' || value::text, '; ') INTO bad FROM jsonb_each(v) AS x(key, value)
    WHERE (value->>'missing')::int <> 0 OR (value->>'differs')::int <> 0 OR (value->>'orphan_records')::int <> 0
       OR (value->>'duplicate_records')::int <> 0
       OR (value->>'matched')::int + (value->>'unresolved')::int <> (value->>'rows')::int;
  IF bad IS NOT NULL THEN RAISE EXCEPTION 'verification: %', bad; END IF;
  IF (v->'broker_task_events'->>'unresolved')::int <> 102 OR (v->'broker_document_request_keys'->>'unresolved')::int <> 1 THEN
    RAISE EXCEPTION 'verification did not report the unresolved rows: %', v;
  END IF;
END $$;

-- Verification compares each row with its record: a row changed after import shows as a differing digest.
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_admin_audit SET detail = '{"change":"altered"}' WHERE tenant_id = 'import-site';
SET session_replication_role = origin;
DO $$ BEGIN
  IF (evidence.import_verify('import-site', 'lab-a')->'broker_admin_audit'->>'differs')::int <> 1 THEN
    RAISE EXCEPTION 'verification missed a changed row';
  END IF;
END $$;
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_admin_audit SET detail = '{"change":"synthetic"}' WHERE tenant_id = 'import-site';
SET session_replication_role = origin;
-- The row digest does not depend on the session's time zone.
SET TimeZone = 'Pacific/Auckland';
DO $$ BEGIN
  IF (evidence.import_verify('import-site', 'lab-a')->'broker_task_events'->>'differs')::int <> 0 THEN
    RAISE EXCEPTION 'the row digest changed with the session time zone';
  END IF;
END $$;
RESET TimeZone;
-- Verification re-derives scope: a project moved to another lab shows its records as differing.
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_projects SET group_key = 'lab-b'
  WHERE tenant_id = 'import-site' AND project_id = '10000000-0000-4000-8000-0000000000a1';
SET session_replication_role = origin;
DO $$ BEGIN
  IF (SELECT sum((value->>'differs')::int) FROM jsonb_each(evidence.import_verify('import-site', 'lab-a'))) = 0 THEN
    RAISE EXCEPTION 'verification missed a changed scope';
  END IF;
  -- The task in the moved project: its lab (A) and its project's lab (now B) disagree, so its records differ.
  IF (evidence.import_verify('import-site', 'lab-a')->'broker_task_events'->>'differs')::int < 1 THEN
    RAISE EXCEPTION 'verification missed a task whose project moved to another lab';
  END IF;
  IF (evidence.import_verify('import-site', 'lab-b')->'broker_admin_audit'->>'differs')::int <> 1 THEN
    RAISE EXCEPTION 'verification missed a standalone record under another lab';
  END IF;
END $$;
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_projects SET group_key = 'lab-a'
  WHERE tenant_id = 'import-site' AND project_id = '10000000-0000-4000-8000-0000000000a1';
SET session_replication_role = origin;

-- A rerun appends nothing new.
DO $$
DECLARE
  t record;
  r jsonb;
  before bigint;
BEGIN
  SELECT count(*) INTO before FROM evidence.records WHERE tenant_id = 'import-site';
  FOR t IN SELECT table_name FROM evidence.import_tables() LOOP
    r := evidence.import_history('import-site', t.table_name, 'lab-a', 0, 100);
    IF (r->>'appended')::int <> 0 OR (r->>'replayed')::int + (r->>'unresolved')::int <> (r->>'rows')::int THEN
      RAISE EXCEPTION 'rerun appended: %', r;
    END IF;
  END LOOP;
  IF (SELECT count(*) FROM evidence.records WHERE tenant_id = 'import-site') <> before THEN
    RAISE EXCEPTION 'rerun changed the record';
  END IF;
END $$;

-- A row changed after its import is refused, and the change is put back.
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_admin_audit SET kind = 'changed' WHERE tenant_id = 'import-site';
SET session_replication_role = origin;
DO $$ BEGIN
  PERFORM evidence.import_history('import-site', 'broker_admin_audit', 'lab-a', 0, 100);
  RAISE EXCEPTION 'a changed row was imported again';
EXCEPTION WHEN unique_violation THEN
  IF SQLERRM <> 'evidence:idempotency_conflict' THEN RAISE; END IF;
END $$;
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_admin_audit SET kind = 'synthetic' WHERE tenant_id = 'import-site';
SET session_replication_role = origin;

-- A row changed under verification fails the seal, which then changes nothing.
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_task_events SET metadata = '{"note":"changed"}'
  WHERE tenant_id = 'import-site' AND task_id = '10000000-0000-4000-8000-0000000000c1';
SET session_replication_role = origin;
DO $$ BEGIN
  PERFORM evidence.import_seal('import-site', 'lab-a');
  RAISE EXCEPTION 'a seal passed over a changed row';
EXCEPTION WHEN unique_violation OR object_not_in_prerequisite_state THEN
  IF SQLERRM NOT IN ('evidence:idempotency_conflict', 'evidence:import_verification_failed') THEN RAISE; END IF;
END $$;
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_task_events SET metadata = '{"note":"synthetic"}'
  WHERE tenant_id = 'import-site' AND task_id = '10000000-0000-4000-8000-0000000000c1';
SET session_replication_role = origin;
DO $$ BEGIN
  IF NOT (SELECT enabled FROM evidence.producers WHERE tenant_id = 'import-site' AND identity = 'broker:history') THEN
    RAISE EXCEPTION 'a failed seal disabled the producer';
  END IF;
END $$;

-- An active research run blocks the seal.
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_research_runs SET status = 'queued' WHERE tenant_id = 'import-site';
SET session_replication_role = origin;
DO $$ BEGIN
  PERFORM evidence.import_seal('import-site', 'lab-a');
  RAISE EXCEPTION 'a seal passed with an active run';
EXCEPTION WHEN object_not_in_prerequisite_state THEN
  IF SQLERRM <> 'evidence:import_run_active' THEN RAISE; END IF;
END $$;
SET session_replication_role = replica;
UPDATE enrichedlab_agent.broker_research_runs SET status = 'failed' WHERE tenant_id = 'import-site';
SET session_replication_role = origin;

-- Sealing verifies, removes the grants and disables the producer for good.
DO $$
DECLARE
  r jsonb;
BEGIN
  r := evidence.import_seal('import-site', 'lab-a');
  -- Three grants: project A (its run, its own records and its task's), lab B (the task without a project) and the
  -- standalone scope in lab A.
  IF (r->>'grants_removed')::int <> 3 OR (r->'records'->>'import.task_events')::int <> 2 OR (r->>'appended')::int <> 0
    OR (r->'chains'->>'broken')::int <> 0 OR (r->'chains'->>'streams')::int < 3
    OR jsonb_array_length(r->'unresolved_keys'->'broker_task_events') <> 102
    OR jsonb_array_length(r->'unresolved_keys'->'broker_document_request_keys') <> 1 THEN
    RAISE EXCEPTION 'seal reported %', left(r::text, 600);
  END IF;
  IF EXISTS (SELECT 1 FROM evidence.producer_grants g JOIN evidence.producers p ON p.tenant_id = g.tenant_id
             AND p.producer_id = g.producer_id WHERE p.tenant_id = 'import-site' AND p.identity = 'broker:history')
    OR (SELECT enabled FROM evidence.producers WHERE tenant_id = 'import-site' AND identity = 'broker:history') THEN
    RAISE EXCEPTION 'the history producer still has grants or is enabled';
  END IF;
  BEGIN
    PERFORM evidence.import_begin('import-site');
    RAISE EXCEPTION 'a sealed import began again';
  EXCEPTION WHEN insufficient_privilege THEN
    IF SQLERRM <> 'evidence:import_finished' THEN RAISE; END IF;
  END;
  BEGIN
    PERFORM evidence.import_history('import-site', 'broker_admin_audit', 'lab-a', 0, 1);
    RAISE EXCEPTION 'a sealed import appended';
  EXCEPTION WHEN insufficient_privilege THEN
    IF SQLERRM <> 'evidence:import_not_begun' THEN RAISE; END IF;
  END;
END $$;
-- The seal alone imports: a second site with one row, begun and sealed with no batch before.
SELECT set_config('enrichedlab.tenant_id', 'seal-site', false);
INSERT INTO enrichedlab_agent.broker_tenants (tenant_id, name) VALUES ('seal-site', 'Seal site');
INSERT INTO enrichedlab_agent.broker_research_groups (tenant_id, group_key, name, max_active_agents,
    max_active_agents_per_principal, max_queued_tasks, retention_days)
  VALUES ('seal-site', 'lab-s', 'Lab S', 4, 2, 16, 30);
SET session_replication_role = replica;
INSERT INTO enrichedlab_agent.broker_admin_audit (tenant_id, event_id, actor_id, kind, detail, occurred_at)
  VALUES ('seal-site', '20000000-0000-4000-8000-0000000000e1', '20000000-0000-4000-8000-000000000001',
          'synthetic', '{"change":"synthetic"}', '2026-10-03T08:00:00Z');
SET session_replication_role = origin;
DO $$
DECLARE
  r jsonb;
BEGIN
  PERFORM evidence.import_begin('seal-site');
  r := evidence.import_seal('seal-site', 'lab-s');
  IF (r->>'appended')::int <> 1 OR (r->'records'->>'import.admin_audit')::int <> 1
    OR (r->'verified'->'broker_admin_audit'->>'matched')::int <> 1 THEN
    RAISE EXCEPTION 'the seal alone did not import: %', r;
  END IF;
  IF (SELECT enabled FROM evidence.producers WHERE tenant_id = 'seal-site' AND identity = 'broker:history') THEN
    RAISE EXCEPTION 'the seal left the producer enabled';
  END IF;
END $$;
SELECT set_config('enrichedlab.tenant_id', 'import-site', false);

SELECT 'evidence import qualification passed';
