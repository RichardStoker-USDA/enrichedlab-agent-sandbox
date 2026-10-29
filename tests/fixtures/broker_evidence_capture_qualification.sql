-- Live capture qualification (migration 081). Runs as the database superuser in the disposable qualification container
-- after the broker migrations and the other evidence fixtures. The superuser owns the functions, as the migration owner
-- does in a deployment. The worker role writes broker rows only through security-definer functions; the helper below
-- stands in for one, so the rows are written with the worker as session_user. Writes are logged as they commit; the
-- recorder (evidence.capture_record, which the append service runs on a timer) is called here before each check.
-- Every check raises on failure.
\set ON_ERROR_STOP on
SET client_min_messages = warning;
-- The recorder and scope preparation refuse a statement not bounded below deadlock_timeout less their 200 ms lock wait;
-- this session bounds each statement as the append service does.
SET statement_timeout = '800ms';

-- Synthetic site: two labs, a project in each, a run in lab A's project, and four tasks: c1 and c3 in lab A; c2 in lab
-- B but linked to lab A's project, so rows about c2 name two labs and their scope does not resolve; c4 in lab B's
-- project. Rows the broker would create through its own functions are written directly with triggers off.
SELECT set_config('enrichedlab.tenant_id', 'capture-site', false);
INSERT INTO enrichedlab_agent.broker_tenants (tenant_id, name) VALUES ('capture-site', 'Capture site');
INSERT INTO enrichedlab_agent.broker_research_groups (tenant_id, group_key, name, max_active_agents,
    max_active_agents_per_principal, max_queued_tasks, retention_days)
  VALUES ('capture-site', 'lab-a', 'Lab A', 4, 2, 16, 30), ('capture-site', 'lab-b', 'Lab B', 4, 2, 16, 30);
INSERT INTO enrichedlab_agent.broker_principals (tenant_id, principal_id, issuer, subject, email, principal_class)
  VALUES ('capture-site', '20000000-0000-4000-8000-000000000001', 'https://issuer.example.invalid', 'capture-person',
          'capture-person@example.invalid', 'person');
INSERT INTO enrichedlab_agent.broker_projects (tenant_id, project_id, group_key, owner_id, title, visibility)
  VALUES ('capture-site', '20000000-0000-4000-8000-0000000000a1', 'lab-a', '20000000-0000-4000-8000-000000000001',
          'Capture project', 'lab'),
         ('capture-site', '20000000-0000-4000-8000-0000000000b1', 'lab-b', '20000000-0000-4000-8000-000000000001',
          'Other project', 'lab');
INSERT INTO enrichedlab_agent.broker_research_runs (tenant_id, run_id, project_id, actor_id, initial_revision,
    allowed_profiles, max_turns, max_parallel, status)
  VALUES ('capture-site', '20000000-0000-4000-8000-00000000a0a1', '20000000-0000-4000-8000-0000000000a1',
          '20000000-0000-4000-8000-000000000001', 0, ARRAY['analysis'], 4, 1, 'failed');
SET session_replication_role = replica;
INSERT INTO enrichedlab_agent.broker_tasks (tenant_id, task_id, principal_id, group_key, profile_key, objective, status,
    desired_status, attempt, max_task_attempts, logical_agent, profile_snapshot, idempotency_key, request_sha256,
    lease_owner, lease_generation, lease_expires_at, submitted_at)
  SELECT 'capture-site', t.id::uuid, '20000000-0000-4000-8000-000000000001', t.lab, 'general-research',
         'Synthetic capture task', 'completed', 'run', 1, 1, 'task-' || replace(t.id, '-', ''), '{}'::jsonb,
         'capture-' || t.id, repeat('a', 64), NULL, 0, NULL, '2026-10-25T05:00:00Z'
  FROM (VALUES ('20000000-0000-4000-8000-0000000000c1', 'lab-a'), ('20000000-0000-4000-8000-0000000000c2', 'lab-b'),
               ('20000000-0000-4000-8000-0000000000c3', 'lab-a'), ('20000000-0000-4000-8000-0000000000c4', 'lab-b'),
               ('20000000-0000-4000-8000-0000000000c6', 'lab-a'))
    AS t(id, lab);
INSERT INTO enrichedlab_agent.broker_project_tasks (tenant_id, project_id, task_id, action_key, parent_task_id)
  VALUES ('capture-site', '20000000-0000-4000-8000-0000000000a1', '20000000-0000-4000-8000-0000000000c2', 'cross-lab',
          NULL),
         ('capture-site', '20000000-0000-4000-8000-0000000000b1', '20000000-0000-4000-8000-0000000000c4', 'step-4',
          NULL);
SET session_replication_role = origin;

CREATE FUNCTION public.capture_test_write(p_sql text) RETURNS void LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog AS $$ BEGIN EXECUTE p_sql; END $$;
REVOKE ALL ON FUNCTION public.capture_test_write(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.capture_test_write(text) TO broker_worker_qual;

-- 1. Before the tenant opts in, a row is logged and the recorder discards it: nothing is recorded (catch-up records it
-- later from the broker's own table).
INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind, state, code, metadata, occurred_at)
  VALUES ('capture-site', '20000000-0000-4000-8000-0000000000c1', 1, 'state', 'queued', 'queued', '{}',
          '2026-10-25T05:01:00Z');
DO $$ DECLARE result jsonb; BEGIN
  IF NOT EXISTS (SELECT 1 FROM evidence.capture_log WHERE tenant_id = 'capture-site') THEN
    RAISE EXCEPTION 'a committed row was not logged'; END IF;
  result := evidence.capture_record(10000);
  IF (result->>'discarded')::int < 1 OR (result->>'appended')::int <> 0 THEN
    RAISE EXCEPTION 'the recorder did not discard a tenant that has not opted in: %', result; END IF;
  IF EXISTS (SELECT 1 FROM evidence.receipts WHERE tenant_id = 'capture-site') THEN
    RAISE EXCEPTION 'a row was recorded before its tenant opted in'; END IF;
  IF EXISTS (SELECT 1 FROM evidence.capture_log WHERE tenant_id = 'capture-site') THEN
    RAISE EXCEPTION 'the recorder left a discarded entry'; END IF;
END $$;

-- 2. Only the migration owner opts a tenant in, catches up or counts what is missing, and only into an enabled lab.
SET SESSION AUTHORIZATION broker_worker_qual;
DO $$ BEGIN
  PERFORM evidence.capture_enable('capture-site', 'lab-a');
  RAISE EXCEPTION 'the worker opted a tenant in';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
DO $$ BEGIN
  PERFORM evidence.capture_catchup('capture-site', ARRAY['broker_task_events']);
  RAISE EXCEPTION 'the worker ran a catch-up';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
DO $$ BEGIN
  PERFORM evidence.capture_missing('capture-site');
  RAISE EXCEPTION 'the worker counted missing records';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
DO $$ BEGIN
  PERFORM evidence.capture_record(10);
  RAISE EXCEPTION 'the worker ran the recorder';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
DO $$ BEGIN
  PERFORM evidence.capture_prepare_scopes('capture-site', 'lab-a', 10);
  RAISE EXCEPTION 'the worker prepared scopes';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
-- The recorder's and catch-up's locks are rows a runtime role cannot lock.
DO $$ BEGIN
  PERFORM 1 FROM evidence.capture_recorder FOR NO KEY UPDATE;
  RAISE EXCEPTION 'the worker locked the recorder';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
DO $$ BEGIN
  PERFORM 1 FROM evidence.capture_tenants FOR KEY SHARE;
  RAISE EXCEPTION 'the worker locked an opt-in row';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
RESET SESSION AUTHORIZATION;
-- The owner check holds even for a role granted EXECUTE by mistake.
GRANT EXECUTE ON FUNCTION evidence.capture_enable(text, text) TO broker_worker_qual;
SET SESSION AUTHORIZATION broker_worker_qual;
DO $$ BEGIN
  PERFORM evidence.capture_enable('capture-site', 'lab-a');
  RAISE EXCEPTION 'a granted worker opted a tenant in';
EXCEPTION WHEN insufficient_privilege THEN
  IF SQLERRM <> 'evidence:capture_not_migration_role' THEN RAISE; END IF;
END $$;
RESET SESSION AUTHORIZATION;
REVOKE EXECUTE ON FUNCTION evidence.capture_enable(text, text) FROM broker_worker_qual;
DO $$ BEGIN
  PERFORM evidence.capture_enable('capture-site', 'no-such-lab');
  RAISE EXCEPTION 'a tenant opted in with an unknown lab';
EXCEPTION WHEN no_data_found THEN NULL; END $$;
SELECT evidence.capture_enable('capture-site', 'lab-a');

-- 2b. The recorder and scope preparation refuse a statement without a bound below deadlock_timeout less their lock
-- wait. Catch-up refuses a scope not yet prepared, and runs once the tenant's scopes are prepared.
SET statement_timeout = 0;
DO $$ BEGIN
  PERFORM evidence.capture_record(1);
  RAISE EXCEPTION 'the recorder ran without a statement bound';
EXCEPTION WHEN invalid_parameter_value THEN
  IF SQLERRM <> 'evidence:capture_statement_unbounded' THEN RAISE; END IF;
END $$;
SET statement_timeout = '900ms';
DO $$ BEGIN
  PERFORM evidence.capture_prepare_scopes('capture-site', 'lab-a', 10);
  RAISE EXCEPTION 'scope preparation ran with a bound that leaves no room for its lock wait';
EXCEPTION WHEN invalid_parameter_value THEN
  IF SQLERRM <> 'evidence:capture_statement_unbounded' THEN RAISE; END IF;
END $$;
SET statement_timeout = '800ms';
DO $$ BEGIN
  PERFORM evidence.capture_catchup('capture-site', ARRAY['broker_task_events']);
  RAISE EXCEPTION 'catch-up recorded into a scope not yet prepared';
EXCEPTION WHEN object_not_in_prerequisite_state THEN
  IF SQLERRM <> 'evidence:capture_scope_unprepared' THEN RAISE; END IF;
END $$;
DO $$ DECLARE first jsonb; BEGIN
  first := evidence.capture_prepare_scopes('capture-site', 'lab-a', 2);
  IF (first->>'prepared')::int <> 2 OR (first->>'remaining')::int < 1 THEN
    RAISE EXCEPTION 'scope preparation did not stop at its limit: %', first; END IF;
END $$;
SELECT evidence.capture_prepare_scopes('capture-site', 'lab-a', 1000)->>'remaining' AS unprepared;
DO $$ BEGIN
  IF (evidence.capture_prepare_scopes('capture-site', 'lab-a', 1000)->>'remaining')::int <> 0 THEN
    RAISE EXCEPTION 'scopes remain unprepared'; END IF;
END $$;

-- 2c. The append service's role records only through evidence.capture_record_now (the grants of the runtime script):
-- the function itself refuses it, and the top-level call records and returns the recorder's result.
GRANT EXECUTE ON PROCEDURE evidence.capture_record_now(integer, jsonb) TO evidence_adapter_qual;
GRANT EXECUTE ON FUNCTION evidence.capture_record(integer) TO evidence_adapter_qual;
GRANT EXECUTE ON FUNCTION evidence.capture_log_age() TO evidence_adapter_qual;
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$ BEGIN
  PERFORM evidence.capture_record(10);
  RAISE EXCEPTION 'the append service''s role ran the recorder outside its procedure';
EXCEPTION WHEN insufficient_privilege THEN
  IF SQLERRM <> 'evidence:capture_record_by_call_only' THEN RAISE; END IF;
END $$;
CALL evidence.capture_record_now(10, NULL);
RESET SESSION AUTHORIZATION;

-- 2d. Every capture function is the migration owner's: no runtime role can execute one, and the append service's role
-- only the recorder's three entry points (the function refuses it outside the procedure).
DO $$ DECLARE leaked text; BEGIN
  SELECT string_agg(p.proname || ' ' || r.rolname, ', ' ORDER BY p.proname, r.rolname) INTO leaked
    FROM pg_proc p CROSS JOIN pg_roles r
    WHERE p.pronamespace = 'evidence'::regnamespace AND p.proname LIKE 'capture\_%'
      AND r.rolname IN ('broker_api_qual', 'broker_worker_qual', 'broker_director_qual', 'broker_custodian_qual',
                        'evidence_adapter_qual')
      AND has_function_privilege(r.oid, p.oid, 'EXECUTE')
      AND NOT (r.rolname = 'evidence_adapter_qual'
               AND p.proname IN ('capture_record', 'capture_record_now', 'capture_log_age'));
  IF leaked IS NOT NULL THEN
    RAISE EXCEPTION 'a runtime role can execute a capture function: %', leaked;
  END IF;
END $$;

-- 3. The record is derived when recorded, from the committed rows, so a row resolves its scope from rows written later
-- in the same transaction: the event comes before the task's project link and still lands in the project. The producer is the capture producer,
-- not the role that wrote the row, and the worker gains no producer. The writer's session uses another time zone; the
-- record's times and digest are still UTC.
SET SESSION AUTHORIZATION broker_worker_qual;
SET TimeZone = 'America/Los_Angeles';
BEGIN;
SELECT public.capture_test_write($q$INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind,
    state, code, metadata, occurred_at) VALUES ('capture-site', '20000000-0000-4000-8000-0000000000c3', 1, 'state',
    'queued', 'queued', '{}', '2026-10-25T05:02:00Z')$q$);
SELECT public.capture_test_write($q$INSERT INTO enrichedlab_agent.broker_project_tasks (tenant_id, project_id, task_id,
    action_key, parent_task_id) VALUES ('capture-site', '20000000-0000-4000-8000-0000000000a1',
    '20000000-0000-4000-8000-0000000000c3', 'step-3', NULL)$q$);
COMMIT;
RESET TimeZone;
RESET SESSION AUTHORIZATION;
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ DECLARE r record; BEGIN
  SELECT rc.group_key, rc.scope_kind, rc.project_id, rc.run_id, rc.actor_role, rc.source_time, rc.payload::jsonb AS body,
         p.identity, p.database_role, p.kind
    INTO r FROM evidence.records rc
    JOIN evidence.producers p ON p.tenant_id = rc.tenant_id AND p.producer_id = rc.producer_id
    WHERE rc.tenant_id = 'capture-site' AND rc.event_type = 'broker.task_events'
      AND rc.payload::jsonb->'key'->>'task_id' = '20000000-0000-4000-8000-0000000000c3';
  IF NOT FOUND THEN RAISE EXCEPTION 'the task event was not recorded'; END IF;
  IF r.group_key <> 'lab-a' OR r.scope_kind <> 'project' OR r.project_id <> '20000000-0000-4000-8000-0000000000a1'
    OR r.run_id IS NOT NULL THEN
    RAISE EXCEPTION 'the task event was recorded in % % %, not lab A''s project', r.group_key, r.scope_kind, r.project_id;
  END IF;
  IF r.identity <> 'broker:capture' OR r.database_role <> 'evidence_capture' OR r.kind <> 'broker'
    OR r.actor_role <> 'broker' THEN
    RAISE EXCEPTION 'the record''s producer is % (%), not the capture producer', r.identity, r.database_role; END IF;
  IF r.body->>'origin' <> 'live' OR r.body->>'change' <> 'insert' OR r.body->>'table' <> 'broker_task_events'
    OR r.body->>'scope' <> 'project' OR r.body->>'time' <> 'row' OR r.source_time <> '2026-10-25T05:02:00Z' THEN
    RAISE EXCEPTION 'the record''s payload is not the live derivation: %', r.body; END IF;
  IF r.body->'fields'->>'occurred_at' !~ '\+00:00$' OR r.body->>'row_sha256' <> (SELECT encode(sha256(convert_to(
      to_jsonb(e)::text, 'UTF8')), 'hex') FROM enrichedlab_agent.broker_task_events e WHERE e.tenant_id = 'capture-site'
      AND e.task_id = '20000000-0000-4000-8000-0000000000c3') THEN
    RAISE EXCEPTION 'the record''s times or digest follow the writer''s time zone: %', r.body->'fields'; END IF;
  IF NOT EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.project_tasks' AND rc.scope_kind = 'project'
      AND rc.payload::jsonb->'key'->>'task_id' = '20000000-0000-4000-8000-0000000000c3') THEN
    RAISE EXCEPTION 'the project link was not recorded'; END IF;

  IF (SELECT count(*) FROM evidence.receipts rc WHERE rc.tenant_id = 'capture-site'
      AND rc.idempotency_key LIKE 'live:task_events:%') <> 1 THEN
    RAISE EXCEPTION 'the event has more or fewer than one live record'; END IF;
  IF EXISTS (SELECT 1 FROM evidence.producers p WHERE p.tenant_id = 'capture-site' AND p.identity <> 'broker:capture')
  THEN
    RAISE EXCEPTION 'capture registered a producer other than the capture producer'; END IF;
END $$;

-- 4. No session appends as the capture producer: evidence.append refuses it, even for a login role with its database
-- role's name, and runtime roles cannot execute the unauthenticated core.
CREATE ROLE evidence_capture LOGIN;
GRANT USAGE ON SCHEMA evidence TO evidence_capture;
GRANT EXECUTE ON FUNCTION evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text)
  TO evidence_capture;
SET SESSION AUTHORIZATION evidence_capture;
DO $$ BEGIN
  PERFORM evidence.append('capture-site', 'broker:capture', 'lab-a', 'lab', NULL, NULL, 'broker.task_events',
    'forged-capture-0', '2026-10-25T05:02:30Z', '{"forged":true}', encode(sha256(convert_to('{"forged":true}', 'UTF8')),
    'hex'));
  RAISE EXCEPTION 'a session named after the capture role appended as the capture producer';
EXCEPTION WHEN insufficient_privilege THEN
  IF SQLERRM <> 'evidence:producer_not_caller' THEN RAISE; END IF;
END $$;
RESET SESSION AUTHORIZATION;
REVOKE ALL ON FUNCTION evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text)
  FROM evidence_capture;
REVOKE USAGE ON SCHEMA evidence FROM evidence_capture;
DROP ROLE evidence_capture;
SET SESSION AUTHORIZATION broker_worker_qual;
DO $$ BEGIN
  PERFORM evidence.append('capture-site', 'broker:capture', 'lab-a', 'lab', NULL, NULL, 'broker.task_events',
    'forged-capture-1', '2026-10-25T05:02:30Z', '{"forged":true}', encode(sha256(convert_to('{"forged":true}', 'UTF8')),
    'hex'));
  RAISE EXCEPTION 'the worker appended as the capture producer';
EXCEPTION WHEN insufficient_privilege THEN
  IF SQLERRM <> 'evidence:producer_not_caller' THEN RAISE; END IF;
END $$;
DO $$ BEGIN
  PERFORM evidence.append_as('capture-site', (SELECT gen_random_uuid()), 'lab-a', 'lab', NULL, NULL,
    'broker.task_events', 'forged-capture-2', '2026-10-25T05:02:30Z', '{"forged":true}',
    encode(sha256(convert_to('{"forged":true}', 'UTF8')), 'hex'));
  RAISE EXCEPTION 'the worker executed the unauthenticated append';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
RESET SESSION AUTHORIZATION;

-- 4b. The worker cannot read, change or remove a log entry, and discarding its session's temporary objects
-- mid-transaction does not touch capture. No role can change or truncate the log.
SET SESSION AUTHORIZATION broker_worker_qual;
DO $$ BEGIN
  PERFORM 1 FROM evidence.capture_log;
  RAISE EXCEPTION 'the worker read the capture log';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
DO $$ BEGIN
  DELETE FROM evidence.capture_log;
  RAISE EXCEPTION 'the worker removed log entries';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
BEGIN;
SELECT public.capture_test_write($q$INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind,
    state, code, metadata, occurred_at) VALUES ('capture-site', '20000000-0000-4000-8000-0000000000c3', 20, 'state',
    'running', 'running', '{}', '2026-10-25T05:02:40Z')$q$);
DISCARD TEMP;
COMMIT;
RESET SESSION AUTHORIZATION;
DO $$ BEGIN
  UPDATE evidence.capture_log SET change = 'retire';
  IF EXISTS (SELECT 1 FROM evidence.capture_log) THEN RAISE EXCEPTION 'a log entry was changed'; END IF;
EXCEPTION WHEN object_not_in_prerequisite_state THEN NULL; END $$;
DO $$ BEGIN
  TRUNCATE evidence.capture_log;
  RAISE EXCEPTION 'the log was truncated';
EXCEPTION WHEN object_not_in_prerequisite_state THEN NULL; END $$;
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.task_events' AND rc.payload::jsonb->'key'->>'task_id' = '20000000-0000-4000-8000-0000000000c3'
      AND (rc.payload::jsonb->'key'->>'sequence')::int = 20) THEN
    RAISE EXCEPTION 'a capture was lost when the session discarded its temporary objects'; END IF;
  IF EXISTS (SELECT 1 FROM evidence.capture_log) THEN RAISE EXCEPTION 'the recorder left recorded entries'; END IF;
END $$;

-- 5. Model text reaches the record only as a digest.
SET SESSION AUTHORIZATION broker_worker_qual;
SELECT public.capture_test_write($q$INSERT INTO enrichedlab_agent.broker_research_turns (tenant_id, run_id, turn,
    generation, context_revision, checkpoint_revision, record, created_at) VALUES ('capture-site',
    '20000000-0000-4000-8000-00000000a0a1', 1, 1, 1, 1, '{"reply":"synthetic capture reply text"}',
    '2026-10-25T05:03:00Z')$q$);
RESET SESSION AUTHORIZATION;
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ DECLARE r record; BEGIN
  SELECT rc.scope_kind, rc.run_id, rc.payload INTO r FROM evidence.records rc
    WHERE rc.tenant_id = 'capture-site' AND rc.event_type = 'broker.research_turns';
  IF NOT FOUND OR r.scope_kind <> 'project' OR r.run_id <> '20000000-0000-4000-8000-00000000a0a1' THEN
    RAISE EXCEPTION 'the turn was not recorded in its run'; END IF;
  IF position('synthetic capture reply' IN r.payload) > 0 OR NOT (r.payload::jsonb->'digests' ? 'record')
    OR r.payload::jsonb->'digests'->'record'->>'class' <> 'model-text' THEN
    RAISE EXCEPTION 'the turn''s model text reached the record: %', r.payload; END IF;
END $$;

-- 6. A row whose scope does not resolve still commits; the recorder writes its gap entry. It is in no lab's stream: its gap entry is owner-only and holds
-- the table, the key and row digests, the intended event and key, the reason and the writing role, not its values.
SET SESSION AUTHORIZATION broker_worker_qual;
SELECT public.capture_test_write($q$INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind,
    state, code, metadata, occurred_at) VALUES ('capture-site', '20000000-0000-4000-8000-0000000000c2', 1, 'state',
    'queued', 'cross lab marker', '{}', '2026-10-25T05:04:00Z')$q$);
DO $$ BEGIN
  PERFORM 1 FROM evidence.capture_gaps;
  RAISE EXCEPTION 'the worker read the gap entries';
EXCEPTION WHEN insufficient_privilege THEN NULL; END $$;
RESET SESSION AUTHORIZATION;
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ DECLARE g record; BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_task_events WHERE tenant_id = 'capture-site'
      AND task_id = '20000000-0000-4000-8000-0000000000c2') THEN
    RAISE EXCEPTION 'a row whose scope does not resolve did not commit'; END IF;
  SELECT * INTO g FROM evidence.capture_gaps WHERE tenant_id = 'capture-site' AND table_name = 'broker_task_events';
  IF NOT FOUND OR g.reason <> 'scope_unresolved' OR g.change <> 'insert' OR g.origin <> 'live'
    OR g.caller_role <> 'broker_worker_qual' OR g.event_type <> 'broker.task_events'
    OR g.idempotency_key !~ '^live:task_events:[0-9a-f]{64}$' OR g.key_sha256 !~ '^[0-9a-f]{64}$'
    OR g.row_sha256 !~ '^[0-9a-f]{64}$' OR g.group_key IS NOT NULL OR g.project_id IS NOT NULL THEN
    RAISE EXCEPTION 'the gap entry is wrong: %', to_jsonb(g); END IF;
  IF position('cross lab marker' IN to_jsonb(g)::text) > 0 THEN RAISE EXCEPTION 'the gap entry holds row values'; END IF;
  IF EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND (rc.payload::jsonb->'key'->>'task_id' = '20000000-0000-4000-8000-0000000000c2'
           OR rc.event_type LIKE 'capture%')) THEN
    RAISE EXCEPTION 'a row whose scope does not resolve reached a stream'; END IF;
END $$;
DO $$ BEGIN
  UPDATE evidence.capture_gaps SET reason = 'lab_disabled' WHERE tenant_id = 'capture-site';
  RAISE EXCEPTION 'a gap entry was changed';
EXCEPTION WHEN object_not_in_prerequisite_state THEN NULL; END $$;

-- 7. A rolled-back row leaves nothing.
BEGIN;
INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind, state, code, metadata, occurred_at)
  VALUES ('capture-site', '20000000-0000-4000-8000-0000000000c1', 2, 'state', 'running', 'running', '{}',
          '2026-10-25T05:05:00Z');
ROLLBACK;
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.payload::jsonb->'key'->>'task_id' = '20000000-0000-4000-8000-0000000000c1') THEN
    RAISE EXCEPTION 'a rolled-back row was recorded'; END IF;
END $$;

-- 8. A row of a lab that is disabled when it is recorded gets a gap entry naming the scope it resolved to.
UPDATE enrichedlab_agent.broker_research_groups SET enabled = false WHERE tenant_id = 'capture-site' AND group_key = 'lab-b';
SET SESSION AUTHORIZATION broker_worker_qual;
SELECT public.capture_test_write($q$INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind,
    state, code, metadata, occurred_at) VALUES ('capture-site', '20000000-0000-4000-8000-0000000000c4', 1, 'state',
    'queued', 'queued', '{}', '2026-10-25T05:06:00Z')$q$);
RESET SESSION AUTHORIZATION;
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
UPDATE enrichedlab_agent.broker_research_groups SET enabled = true WHERE tenant_id = 'capture-site' AND group_key = 'lab-b';
DO $$ DECLARE g record; BEGIN
  SELECT * INTO g FROM evidence.capture_gaps WHERE tenant_id = 'capture-site' AND reason = 'lab_disabled';
  IF NOT FOUND OR g.group_key <> 'lab-b' OR g.scope_kind <> 'project'
    OR g.project_id <> '20000000-0000-4000-8000-0000000000b1' THEN
    RAISE EXCEPTION 'the disabled lab''s gap entry is wrong: %', to_jsonb(g); END IF;
END $$;

-- 8b. A disabled tenant's write (the broker finishing a task as no longer allowed) commits, with a gap entry.
UPDATE enrichedlab_agent.broker_tenants SET enabled = false WHERE tenant_id = 'capture-site';
INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind, state, code, metadata, occurred_at)
  VALUES ('capture-site', '20000000-0000-4000-8000-0000000000c1', 3, 'state', 'cancelled', 'tenant_disabled', '{}',
          '2026-10-25T05:06:30Z');
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
UPDATE enrichedlab_agent.broker_tenants SET enabled = true WHERE tenant_id = 'capture-site';
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM evidence.capture_gaps WHERE tenant_id = 'capture-site' AND reason = 'tenant_disabled'
      AND group_key = 'lab-a') THEN
    RAISE EXCEPTION 'a disabled tenant''s write has no gap entry'; END IF;
END $$;

-- 9. A tool group grant's revocation is its own event.
INSERT INTO enrichedlab_agent.broker_tool_groups (tenant_id, tool_group_key, version, title, description)
  VALUES ('capture-site', 'capture-group', 1, 'Capture group', 'A synthetic tool group');
INSERT INTO enrichedlab_agent.broker_tool_group_grants (tenant_id, group_key, tool_group_key, version, granted_at)
  VALUES ('capture-site', 'lab-a', 'capture-group', 1, '2026-10-25T05:07:00Z');
UPDATE enrichedlab_agent.broker_tool_group_grants SET revoked_at = '2026-10-25T05:08:00Z'
  WHERE tenant_id = 'capture-site' AND tool_group_key = 'capture-group';
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ DECLARE r record; BEGIN
  SELECT rc.group_key, rc.scope_kind, rc.source_time, rc.payload::jsonb AS body INTO r FROM evidence.records rc
    WHERE rc.tenant_id = 'capture-site' AND rc.event_type = 'broker.tool_group_grant_revoked';
  IF NOT FOUND OR r.group_key <> 'lab-a' OR r.scope_kind <> 'lab' OR r.body->>'change' <> 'revoke'
    OR r.body->'fields'->>'revoked_at' IS NULL THEN
    RAISE EXCEPTION 'the revocation was not recorded: %', r; END IF;
  IF NOT EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.tool_group_grants' AND rc.payload::jsonb->>'change' = 'insert'
      AND rc.payload::jsonb->'fields'->'revoked_at' = 'null'::jsonb) THEN
    RAISE EXCEPTION 'the grant itself was not recorded as granted'; END IF;
  IF NOT EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.tool_groups' AND rc.scope_kind = 'standalone' AND rc.group_key = 'lab-a') THEN
    RAISE EXCEPTION 'the tool group was not recorded in the standalone lab'; END IF;
END $$;

-- 9a. A row with no time of its own (a tool group member has neither a time column nor a parent with one) takes its
-- writing transaction's start, carried in its log entry, not the recorder's time. The entry is written here as the
-- owner can, with a start an hour before the recording.
SELECT set_config('capture.member_written_at', (now() - interval '1 hour')::text, false) IS NOT NULL AS noted;
INSERT INTO evidence.capture_log (tenant_id, table_name, change, row_value, written_at, written_by)
  VALUES ('capture-site', 'broker_tool_group_members', 'insert',
          '{"tenant_id": "capture-site", "tool_group_key": "capture-group", "version": 1, "profile_key": "synthetic-profile"}',
          current_setting('capture.member_written_at')::timestamptz, 'broker_worker_qual');
DO $$ BEGIN
  IF evidence.capture_log_age() < 3599 THEN
    RAISE EXCEPTION 'the log age did not report an entry written an hour ago: %', evidence.capture_log_age(); END IF;
END $$;
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.tool_group_members' AND rc.payload::jsonb->>'time' = 'captured'
      AND rc.source_time = current_setting('capture.member_written_at')::timestamptz) THEN
    RAISE EXCEPTION 'a timeless row did not take its writing transaction''s start: %',
      (SELECT json_agg(json_build_object('time', rc.payload::jsonb->>'time', 'source', rc.source_time))
         FROM evidence.records rc WHERE rc.tenant_id = 'capture-site' AND rc.event_type = 'broker.tool_group_members');
  END IF;
END $$;

-- 9b. An update made while capture was off is caught up as its own event: the row's digest no longer matches the one
-- its insert record holds. The permitted updates of datasets and reviewed queries are recognised the same way.
INSERT INTO enrichedlab_agent.broker_tool_group_grants (tenant_id, group_key, tool_group_key, version, granted_at)
  VALUES ('capture-site', 'lab-b', 'capture-group', 1, '2026-10-25T05:09:00Z');
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DELETE FROM evidence.capture_tenants WHERE tenant_id = 'capture-site';
UPDATE enrichedlab_agent.broker_tool_group_grants SET revoked_at = '2026-10-25T05:10:00Z'
  WHERE tenant_id = 'capture-site' AND group_key = 'lab-b';
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
SELECT evidence.capture_enable('capture-site', 'lab-a');
-- Another permitted producer appends under the revocation's predictable key; it is not the revocation's record.
CREATE ROLE evidence_forger LOGIN;
GRANT USAGE ON SCHEMA evidence TO evidence_forger;
GRANT EXECUTE ON FUNCTION evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text)
  TO evidence_forger;
INSERT INTO evidence.producers (tenant_id, identity, kind, database_role)
  VALUES ('capture-site', 'broker:forger', 'broker', 'evidence_forger');
INSERT INTO evidence.producer_grants (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, all_runs,
    event_types, actor_role)
  SELECT 'capture-site', producer_id, 'lab-b', 'lab', NULL, NULL, false, ARRAY['broker.tool_group_grant_revoked'], 'broker'
  FROM evidence.producers WHERE tenant_id = 'capture-site' AND identity = 'broker:forger';
SELECT set_config('capture.forged_key', evidence.capture_idempotency_key('broker_tool_group_grants', 'revoke',
    evidence.import_key(to_jsonb(g), ARRAY['grant_id'], (SELECT column_types FROM evidence.capture_tables()
      WHERE table_name = 'broker_tool_group_grants'))), false)
  FROM enrichedlab_agent.broker_tool_group_grants g WHERE g.tenant_id = 'capture-site' AND g.group_key = 'lab-b';
SET SESSION AUTHORIZATION evidence_forger;
SELECT evidence.append('capture-site', 'broker:forger', 'lab-b', 'lab', NULL, NULL, 'broker.tool_group_grant_revoked',
  current_setting('capture.forged_key'), '2026-10-25T05:10:30Z', '{"forged":true}',
  encode(sha256(convert_to('{"forged":true}', 'UTF8')), 'hex')) IS NOT NULL AS forged;
RESET SESSION AUTHORIZATION;
REVOKE ALL ON FUNCTION evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text)
  FROM evidence_forger;
REVOKE USAGE ON SCHEMA evidence FROM evidence_forger;
DROP ROLE evidence_forger;
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM evidence.records r JOIN evidence.producers p ON p.tenant_id = r.tenant_id
      AND p.producer_id = r.producer_id WHERE r.tenant_id = 'capture-site' AND p.identity = 'broker:capture'
      AND r.event_type = 'broker.tool_group_grant_revoked' AND r.group_key = 'lab-b') THEN
    RAISE EXCEPTION 'an update made while capture was off was recorded'; END IF;
  IF evidence.capture_update_change('broker_datasets', '{"intake_task_id":null}', '{"intake_task_id":"t"}')
      IS DISTINCT FROM 'intake'
    OR evidence.capture_update_change('broker_reviewed_queries', '{"definition":{"state":"enabled"}}',
      '{"definition":{"state":"retired"}}') IS DISTINCT FROM 'retire'
    OR evidence.capture_update_change('broker_tool_group_grants', '{"revoked_at":null}', '{"revoked_at":"t"}')
      IS DISTINCT FROM 'revoke'
    OR evidence.capture_update_change('broker_tool_group_grants', '{"revoked_at":"t"}', '{"revoked_at":"u"}') IS NOT NULL
    OR evidence.capture_update_change('broker_task_events', '{}', '{}') IS NOT NULL THEN
    RAISE EXCEPTION 'a permitted update is not recognised'; END IF;
END $$;

-- 9c. Constraint timing does not touch capture: with every constraint immediate, a new task's first event written before
-- its project link in the same transaction is still recorded in the project, once.
SET SESSION AUTHORIZATION broker_worker_qual;
BEGIN;
SET CONSTRAINTS ALL IMMEDIATE;
SELECT public.capture_test_write($q$INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind,
    state, code, metadata, occurred_at) VALUES ('capture-site', '20000000-0000-4000-8000-0000000000c6', 1, 'state',
    'queued', 'queued', '{}', '2026-10-25T05:02:50Z')$q$);
SELECT public.capture_test_write($q$INSERT INTO enrichedlab_agent.broker_project_tasks (tenant_id, project_id, task_id,
    action_key, parent_task_id) VALUES ('capture-site', '20000000-0000-4000-8000-0000000000a1',
    '20000000-0000-4000-8000-0000000000c6', 'step-6', NULL)$q$);
COMMIT;
RESET SESSION AUTHORIZATION;
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ BEGIN
  IF (SELECT count(*) FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.task_events' AND rc.scope_kind = 'project'
      AND rc.project_id = '20000000-0000-4000-8000-0000000000a1'
      AND rc.payload::jsonb->'key'->>'task_id' = '20000000-0000-4000-8000-0000000000c6') <> 1 THEN
    RAISE EXCEPTION 'an event written under immediate constraints was not recorded once in its project'; END IF;
END $$;

-- 9d. An entry the recorder has not taken is pending, not missing, and catch-up leaves it to the recorder, which
-- records it once. An entry whose change already has its record (a duplicate, written here as the owner can) is removed
-- without a second record.
INSERT INTO enrichedlab_agent.broker_admin_audit (tenant_id, actor_id, kind, detail)
  VALUES ('capture-site', '20000000-0000-4000-8000-000000000001', 'capture_probe', '{}');
DO $$ DECLARE counts jsonb; BEGIN
  counts := evidence.capture_missing('capture-site')->'tables'->'broker_admin_audit';
  IF (counts->>'pending')::int <> 1 OR (counts->>'missing')::int <> 0 THEN
    RAISE EXCEPTION 'an unrecorded log entry was not counted as pending: %', counts; END IF;
END $$;
SELECT evidence.capture_prepare_scopes('capture-site', 'lab-a', 1000)->>'remaining' AS unprepared;
SELECT set_config('capture.audit_catchup', evidence.capture_catchup('capture-site', ARRAY['broker_admin_audit'])::text,
  false);
DO $$ DECLARE caught jsonb := current_setting('capture.audit_catchup')::jsonb; recorded jsonb; BEGIN
  IF (caught->>'appended')::int <> 0 OR (caught->>'pending')::int <> 1 THEN
    RAISE EXCEPTION 'catch-up recorded a change the log holds: %', caught; END IF;
  recorded := evidence.capture_record(10000);
  IF (recorded->>'appended')::int <> 1 THEN RAISE EXCEPTION 'the recorder did not record the entry: %', recorded; END IF;
  INSERT INTO evidence.capture_log (tenant_id, table_name, change, row_value, written_at, written_by)
    SELECT a.tenant_id, 'broker_admin_audit', 'insert', to_jsonb(a), now(), 'broker_worker_qual'
      FROM enrichedlab_agent.broker_admin_audit a WHERE a.tenant_id = 'capture-site';
  recorded := evidence.capture_record(10000);
  IF (recorded->>'already_recorded')::int <> 1 OR (recorded->>'appended')::int <> 0 THEN
    RAISE EXCEPTION 'the recorder did not drop an entry whose change has its record: %', recorded; END IF;
  IF (SELECT count(*) FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.admin_audit') <> 1 THEN
    RAISE EXCEPTION 'the audit row has more or fewer than one record'; END IF;
END $$;

-- 10. Cutover: one catch-up over every captured table records the rows written before the opt-in (origin caught_up),
-- places the disabled lab's row now that its lab is enabled and links its gap entry, keeps one gap entry per row, and
-- leaves nothing missing or pending. A second catch-up in the same transaction is refused; a rerun appends nothing.
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
SELECT evidence.capture_prepare_scopes('capture-site', 'lab-a', 1000)->>'remaining' AS unprepared;
DO $$ BEGIN
  PERFORM evidence.capture_catchup('capture-site', ARRAY['broker_admin_audit']);
  PERFORM evidence.capture_catchup('capture-site', ARRAY['broker_admin_audit']);
  RAISE EXCEPTION 'a second catch-up ran in one transaction';
EXCEPTION WHEN object_not_in_prerequisite_state THEN
  IF SQLERRM <> 'evidence:capture_catchup_once_per_transaction' THEN RAISE; END IF;
END $$;
SELECT set_config('capture.first_catchup', evidence.capture_catchup('capture-site')::text, false);
DO $$ DECLARE result jsonb := current_setting('capture.first_catchup')::jsonb; appended bigint; again bigint;
  missing jsonb; r record; BEGIN
  appended := (result->>'appended')::bigint;
  -- c1's two events (before the opt-in, and while the tenant was disabled), c4's event, c4's project link and lab
  -- B's grant revocation (despite the forged receipt under its key).
  IF appended <> 5 THEN RAISE EXCEPTION 'catch-up appended % records', appended; END IF;
  IF NOT EXISTS (SELECT 1 FROM evidence.records rr JOIN evidence.producers p ON p.tenant_id = rr.tenant_id
      AND p.producer_id = rr.producer_id WHERE rr.tenant_id = 'capture-site' AND p.identity = 'broker:capture'
      AND rr.event_type = 'broker.tool_group_grant_revoked' AND rr.group_key = 'lab-b'
      AND rr.payload::jsonb->>'origin' = 'caught_up') THEN
    RAISE EXCEPTION 'catch-up did not record the update made while capture was off'; END IF;
  -- The rerun is the next statement's own transaction.
  PERFORM 1;
END $$;
SELECT set_config('capture.second_catchup', evidence.capture_catchup('capture-site')::text, false);
DO $$ DECLARE again bigint := (current_setting('capture.second_catchup')::jsonb->>'appended')::bigint; missing jsonb;
  r record; BEGIN
  IF again <> 0 THEN RAISE EXCEPTION 'a catch-up rerun appended % records', again; END IF;
  missing := evidence.capture_missing('capture-site');
  IF (missing->>'missing')::int <> 0 OR (missing->>'gap_only')::int <> 2 OR (missing->>'pending')::int <> 0 THEN
    RAISE EXCEPTION 'after catch-up: %', missing; END IF;
  IF (SELECT count(*) FROM evidence.capture_gaps WHERE tenant_id = 'capture-site') <> 4 THEN
    RAISE EXCEPTION 'gap entries: %', (SELECT count(*) FROM evidence.capture_gaps WHERE tenant_id = 'capture-site');
  END IF;
  SELECT rc.scope_kind, rc.payload::jsonb AS body, x.gap_id INTO r FROM evidence.records rc
    JOIN evidence.capture_gap_resolutions x ON x.tenant_id = rc.tenant_id AND x.record_id = rc.record_id
    WHERE rc.tenant_id = 'capture-site' AND rc.event_type = 'broker.task_events'
      AND rc.payload::jsonb->'key'->>'task_id' = '20000000-0000-4000-8000-0000000000c4';
  IF NOT FOUND THEN
    RAISE EXCEPTION 'the disabled lab''s row was not placed and linked: records %, resolutions %',
      (SELECT json_agg(json_build_object('scope', rc.scope_kind, 'origin', rc.payload::jsonb->>'origin'))
         FROM evidence.records rc WHERE rc.tenant_id = 'capture-site' AND rc.event_type = 'broker.task_events'
           AND rc.payload::jsonb->'key'->>'task_id' = '20000000-0000-4000-8000-0000000000c4'),
      (SELECT json_agg(json_build_object('reason', g.reason, 'resolved', x.record_id IS NOT NULL))
         FROM evidence.capture_gaps g LEFT JOIN evidence.capture_gap_resolutions x
           ON x.tenant_id = g.tenant_id AND x.gap_id = g.gap_id WHERE g.tenant_id = 'capture-site');
  END IF;
  IF r.scope_kind <> 'project' OR r.body->>'origin' <> 'caught_up' THEN
    RAISE EXCEPTION 'the disabled lab''s row was placed as % %', r.scope_kind, r.body->>'origin'; END IF;
  IF NOT EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.task_events' AND rc.scope_kind = 'lab' AND rc.payload::jsonb->>'origin' = 'caught_up'
      AND rc.payload::jsonb->'key'->>'task_id' = '20000000-0000-4000-8000-0000000000c1') THEN
    RAISE EXCEPTION 'catch-up did not record the row written before the opt-in'; END IF;
END $$;

-- 10b. Catch-up does not record a change the log still holds: a grant made and revoked before the recorder runs is left
-- to the recorder (pending), which records the grant as granted and then its revocation.
INSERT INTO enrichedlab_agent.broker_tool_group_grants (tenant_id, group_key, tool_group_key, version, granted_at)
  VALUES ('capture-site', 'lab-a', 'capture-group', 1, '2026-10-25T05:11:00Z');
SELECT set_config('capture.late_grant', (SELECT max(grant_id)::text FROM enrichedlab_agent.broker_tool_group_grants
  WHERE tenant_id = 'capture-site'), false) IS NOT NULL AS noted;
UPDATE enrichedlab_agent.broker_tool_group_grants SET revoked_at = '2026-10-25T05:12:00Z'
  WHERE tenant_id = 'capture-site' AND grant_id = current_setting('capture.late_grant')::bigint;
SELECT evidence.capture_prepare_scopes('capture-site', 'lab-a', 1000)->>'remaining' AS unprepared;
SELECT set_config('capture.late_catchup', evidence.capture_catchup('capture-site',
  ARRAY['broker_tool_group_grants'])::text, false) IS NOT NULL AS noted;
DO $$ DECLARE caught jsonb := current_setting('capture.late_catchup')::jsonb; counts jsonb; recorded jsonb; BEGIN
  IF (caught->>'appended')::int <> 0 OR (caught->>'pending')::int <> 1 THEN
    RAISE EXCEPTION 'catch-up recorded a logged change: %', caught; END IF;
  counts := evidence.capture_missing('capture-site')->'tables'->'broker_tool_group_grants';
  IF (counts->>'pending')::int <> 1 OR (counts->>'missing')::int <> 0 THEN
    RAISE EXCEPTION 'the logged grant was not counted as pending: %', counts; END IF;
END $$;
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.tool_group_grants' AND rc.payload::jsonb->>'origin' = 'live'
      AND rc.payload::jsonb->'key'->>'grant_id' = current_setting('capture.late_grant')
      AND rc.payload::jsonb->'fields'->'revoked_at' = 'null'::jsonb) THEN
    RAISE EXCEPTION 'the grant was not recorded as granted'; END IF;
  IF NOT EXISTS (SELECT 1 FROM evidence.records rc WHERE rc.tenant_id = 'capture-site'
      AND rc.event_type = 'broker.tool_group_grant_revoked' AND rc.payload::jsonb->>'origin' = 'live'
      AND rc.payload::jsonb->'key'->>'grant_id' = current_setting('capture.late_grant')) THEN
    RAISE EXCEPTION 'the revocation was not recorded'; END IF;
END $$;

-- 11. Every chain verifies: each stream's records run 1..head, each links to its predecessor, and each hash matches
-- its material and payload.
DO $$ DECLARE head evidence.streams%ROWTYPE; r record; expected bigint; previous text; checked integer := 0; BEGIN
  FOR head IN SELECT * FROM evidence.streams WHERE tenant_id = 'capture-site' LOOP
    expected := 1;
    previous := repeat('0', 64);
    FOR r IN SELECT rec.*, p.identity FROM evidence.records rec
        JOIN evidence.producers p ON p.tenant_id = rec.tenant_id AND p.producer_id = rec.producer_id
        WHERE rec.tenant_id = head.tenant_id AND rec.stream_id = head.stream_id ORDER BY rec.sequence LOOP
      IF r.sequence <> expected OR r.previous_hash <> previous
        OR encode(sha256(convert_to(evidence.record_material_v1(r.tenant_id, r.stream_id, r.sequence, r.record_id,
             r.group_key, r.scope_kind, r.project_id, r.run_id, r.identity, r.event_type, r.actor_role, r.source_time,
             r.ingested_at, r.payload_sha256, r.previous_hash), 'UTF8')), 'hex') <> r.record_hash
        OR encode(sha256(convert_to(r.payload, 'UTF8')), 'hex') <> r.payload_sha256 THEN
        RAISE EXCEPTION 'stream % breaks at sequence %', head.stream_id, r.sequence;
      END IF;
      previous := r.record_hash;
      expected := expected + 1;
    END LOOP;
    IF expected - 1 <> head.head_sequence OR previous <> head.head_hash THEN
      RAISE EXCEPTION 'stream % head does not match its records', head.stream_id; END IF;
    checked := checked + 1;
  END LOOP;
  IF checked < 5 THEN RAISE EXCEPTION 'only % capture streams', checked; END IF;
END $$;

-- 12. Every row trigger in the broker schema that runs BEFORE an update or a delete, whatever its name, is in the
-- reviewed list broker_evidence_capture_guards.txt with its disposition: `captured` (the table's inserts are recorded),
-- `captured+<change>` (its one permitted update is recorded too) or `mutable` (a table whose rows change by design,
-- not append-guarded). A new trigger or table fails here until it is reviewed, and the dispositions must match the
-- capture triggers that exist.
\set expected_guards `cat /workspace/tests/fixtures/broker_evidence_capture_guards.txt`
SELECT set_config('capture.expected_guards', :'expected_guards', false);
DO $$ DECLARE added text; removed text; wrong text; BEGIN
  WITH actual AS (
    SELECT c.relname || ' ' || t.tgname || ' ' || p.proname AS line
      FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_proc p ON p.oid = t.tgfoid
      WHERE c.relnamespace = 'enrichedlab_agent'::regnamespace AND NOT t.tgisinternal
        AND (t.tgtype & 1) <> 0 AND (t.tgtype & 2) <> 0 AND (t.tgtype & (8 | 16)) <> 0),
  expected AS (
    SELECT split_part(line, ' ', 1) || ' ' || split_part(line, ' ', 2) || ' ' || split_part(line, ' ', 3) AS line
      FROM regexp_split_to_table(current_setting('capture.expected_guards'), E'\n') AS line WHERE line <> '')
  SELECT (SELECT string_agg(line, E'\n' ORDER BY line) FROM (SELECT line FROM actual EXCEPT SELECT line FROM expected) x),
         (SELECT string_agg(line, E'\n' ORDER BY line) FROM (SELECT line FROM expected EXCEPT SELECT line FROM actual) y)
    INTO added, removed;
  IF added IS NOT NULL OR removed IS NOT NULL THEN
    RAISE EXCEPTION E'broker update and delete guards changed\nnot in the reviewed list:\n%\nno longer present:\n%',
      coalesce(added, '-'), coalesce(removed, '-');
  END IF;
  WITH listed AS (
    SELECT split_part(line, ' ', 1) AS table_name, split_part(line, ' ', 4) AS disposition
      FROM regexp_split_to_table(current_setting('capture.expected_guards'), E'\n') AS line WHERE line <> ''),
  per_table AS (
    SELECT table_name, min(disposition) AS disposition, count(DISTINCT disposition) AS kinds FROM listed GROUP BY 1)
  SELECT string_agg(table_name || ' ' || disposition, ', ' ORDER BY table_name) INTO wrong FROM per_table pt
    WHERE pt.kinds <> 1
       OR pt.disposition NOT IN ('captured', 'mutable') AND pt.disposition NOT LIKE 'captured+%'
       OR (pt.disposition LIKE 'captured%') <> EXISTS (SELECT 1 FROM evidence.capture_tables() ct
            WHERE ct.table_name = pt.table_name)
       OR (pt.disposition LIKE 'captured%') <> EXISTS (SELECT 1 FROM pg_trigger x JOIN pg_class c ON c.oid = x.tgrelid
            WHERE c.relnamespace = 'enrichedlab_agent'::regnamespace AND c.relname = pt.table_name
              AND x.tgname = 'evidence_capture' AND NOT x.tgdeferrable)
       OR (pt.disposition LIKE 'captured+%') <> (evidence.capture_update_of(pt.table_name) IS NOT NULL)
       OR (pt.disposition LIKE 'captured+%' AND split_part(pt.disposition, '+', 2)
            IS DISTINCT FROM evidence.capture_update_of(pt.table_name))
       OR (pt.disposition LIKE 'captured+%') <> EXISTS (SELECT 1 FROM pg_trigger x JOIN pg_class c ON c.oid = x.tgrelid
            WHERE c.relnamespace = 'enrichedlab_agent'::regnamespace AND c.relname = pt.table_name
              AND x.tgname = 'evidence_capture_update' AND NOT x.tgdeferrable);
  IF wrong IS NOT NULL THEN RAISE EXCEPTION 'guard dispositions do not match capture: %', wrong; END IF;
  -- Every captured table is in the list, so none is captured without review.
  SELECT string_agg(ct.table_name, ', ') INTO wrong FROM evidence.capture_tables() ct
    WHERE NOT EXISTS (SELECT 1 FROM regexp_split_to_table(current_setting('capture.expected_guards'), E'\n') AS line
      WHERE split_part(line, ' ', 1) = ct.table_name);
  IF wrong IS NOT NULL THEN RAISE EXCEPTION 'captured tables with no reviewed guard: %', wrong; END IF;
END $$;

-- 13. What each captured column can contribute (copied when a safe token, digested, or model text by digest only) is
-- frozen in broker_evidence_capture_columns.txt: a new or retyped column fails here until its class is reviewed.
\set expected_columns `cat /workspace/tests/fixtures/broker_evidence_capture_columns.txt`
SELECT set_config('capture.expected_columns', :'expected_columns', false);
DO $$ DECLARE added text; removed text; BEGIN
  WITH actual AS (
    SELECT ct.table_name || ' ' || c.key || ' ' || (ct.column_types->>c.key) || ' '
        || evidence.import_column_class(ct.table_name, c.key, ct.column_types->>c.key) AS line
      FROM evidence.capture_tables() ct, jsonb_object_keys(ct.column_types) AS c(key)),
  expected AS (
    SELECT line FROM regexp_split_to_table(current_setting('capture.expected_columns'), E'\n') AS line WHERE line <> '')
  SELECT (SELECT string_agg(line, E'\n' ORDER BY line) FROM (SELECT line FROM actual EXCEPT SELECT line FROM expected) x),
         (SELECT string_agg(line, E'\n' ORDER BY line) FROM (SELECT line FROM expected EXCEPT SELECT line FROM actual) y)
    INTO added, removed;
  IF added IS NOT NULL OR removed IS NOT NULL THEN
    RAISE EXCEPTION E'captured column classes changed\nnot in the reviewed list:\n%\nno longer present:\n%',
      coalesce(added, '-'), coalesce(removed, '-');
  END IF;
END $$;

-- 14. Capture leaves nothing behind: the log is empty once recorded, and no capture trigger is a constraint trigger.
SELECT evidence.capture_record(10000)->>'taken' AS recorded;
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM evidence.capture_log) THEN RAISE EXCEPTION 'the log kept entries after recording'; END IF;
  IF EXISTS (SELECT 1 FROM pg_trigger x JOIN pg_class c ON c.oid = x.tgrelid
      WHERE c.relnamespace = 'enrichedlab_agent'::regnamespace AND x.tgname LIKE 'evidence_capture%'
        AND (x.tgdeferrable OR x.tgconstraint <> 0)) THEN
    RAISE EXCEPTION 'a capture trigger runs at commit'; END IF;
END $$;

DROP FUNCTION public.capture_test_write(text);
