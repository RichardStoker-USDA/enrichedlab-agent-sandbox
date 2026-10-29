-- Evidence record qualification (migration 075). Runs as the database superuser in the disposable
-- qualification container after the broker migrations and runtime grants; acts as the broker roles through
-- SET SESSION AUTHORIZATION so the functions see the caller a deployment would. Every check raises on failure.
\set ON_ERROR_STOP on
SET client_min_messages = warning;

-- Synthetic site: one tenant, two labs, a project in each, two runs in project A (one finished, since a project has one
-- active run), three people (A owns both projects and is a lab A member, B belongs to no lab, C is a lab A member), a
-- lab A service account, four producers and two adapter roles.
SELECT set_config('enrichedlab.tenant_id', 'evidence-site', false);
INSERT INTO enrichedlab_agent.broker_tenants (tenant_id, name) VALUES ('evidence-site', 'Evidence site')
  ON CONFLICT DO NOTHING;
INSERT INTO enrichedlab_agent.broker_research_groups (tenant_id, group_key, name, max_active_agents,
    max_active_agents_per_principal, max_queued_tasks, retention_days)
  VALUES ('evidence-site', 'lab-a', 'Lab A', 4, 2, 16, 30), ('evidence-site', 'lab-b', 'Lab B', 4, 2, 16, 30)
  ON CONFLICT DO NOTHING;
INSERT INTO enrichedlab_agent.broker_principals (tenant_id, principal_id, issuer, subject, email, principal_class)
  VALUES ('evidence-site', '00000000-0000-4000-8000-000000000001', 'https://issuer.example.invalid', 'person-a',
          'person-a@example.invalid', 'person'),
         ('evidence-site', '00000000-0000-4000-8000-000000000002', 'https://issuer.example.invalid', 'person-b',
          'person-b@example.invalid', 'person'),
         ('evidence-site', '00000000-0000-4000-8000-000000000003', 'https://issuer.example.invalid', 'service-a',
          'service-a@example.invalid', 'service_account'),
         ('evidence-site', '00000000-0000-4000-8000-000000000004', 'https://issuer.example.invalid', 'person-c',
          'person-c@example.invalid', 'person')
  ON CONFLICT DO NOTHING;
INSERT INTO enrichedlab_agent.broker_group_memberships (tenant_id, group_key, principal_id, role)
  VALUES ('evidence-site', 'lab-a', '00000000-0000-4000-8000-000000000001', 'researcher'),
         ('evidence-site', 'lab-a', '00000000-0000-4000-8000-000000000003', 'researcher'),
         ('evidence-site', 'lab-a', '00000000-0000-4000-8000-000000000004', 'researcher')
  ON CONFLICT DO NOTHING;
INSERT INTO enrichedlab_agent.broker_projects (tenant_id, project_id, group_key, owner_id, title, visibility)
  VALUES ('evidence-site', '00000000-0000-4000-8000-0000000000a1', 'lab-a', '00000000-0000-4000-8000-000000000001',
          'Project A', 'lab'),
         ('evidence-site', '00000000-0000-4000-8000-0000000000b1', 'lab-b', '00000000-0000-4000-8000-000000000001',
          'Project B', 'lab')
  ON CONFLICT DO NOTHING;
INSERT INTO enrichedlab_agent.broker_research_runs (tenant_id, run_id, project_id, actor_id, initial_revision,
    allowed_profiles, max_turns, max_parallel, status)
  VALUES ('evidence-site', '00000000-0000-4000-8000-00000000a0a1', '00000000-0000-4000-8000-0000000000a1',
          '00000000-0000-4000-8000-000000000001', 0, ARRAY['analysis'], 4, 1, 'queued'),
         ('evidence-site', '00000000-0000-4000-8000-00000000a0a2', '00000000-0000-4000-8000-0000000000a1',
          '00000000-0000-4000-8000-000000000001', 0, ARRAY['analysis'], 4, 1, 'failed'),
         ('evidence-site', '00000000-0000-4000-8000-00000000b0b1', '00000000-0000-4000-8000-0000000000b1',
          '00000000-0000-4000-8000-000000000001', 0, ARRAY['analysis'], 4, 1, 'queued')
  ON CONFLICT DO NOTHING;
DO $$
BEGIN
  IF (SELECT count(*) FROM enrichedlab_agent.broker_research_runs WHERE tenant_id = 'evidence-site') <> 3 THEN
    RAISE EXCEPTION 'synthetic runs were not all created';
  END IF;
END $$;

DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'evidence_adapter_qual') THEN
    CREATE ROLE evidence_adapter_qual LOGIN NOSUPERUSER NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'evidence_adapter_two') THEN
    CREATE ROLE evidence_adapter_two LOGIN NOSUPERUSER NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'evidence_rls_probe') THEN
    CREATE ROLE evidence_rls_probe LOGIN NOSUPERUSER NOBYPASSRLS;
  END IF;
END $$;
GRANT USAGE ON SCHEMA evidence TO evidence_adapter_qual, evidence_adapter_two;
GRANT EXECUTE ON FUNCTION evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text)
  TO evidence_adapter_qual, evidence_adapter_two;
GRANT EXECUTE ON FUNCTION evidence.adapter_producers(text) TO evidence_adapter_qual, evidence_adapter_two;
GRANT EXECUTE ON FUNCTION evidence.ensure_record_partitions(integer) TO evidence_adapter_qual;

INSERT INTO evidence.producers (tenant_id, producer_id, identity, kind, database_role) VALUES
  ('evidence-site', '00000000-0000-4000-8000-0000000000e1', 'broker/api', 'broker', 'broker_api_qual'),
  ('evidence-site', '00000000-0000-4000-8000-0000000000e2', 'broker/worker', 'broker', 'broker_worker_qual'),
  ('evidence-site', '00000000-0000-4000-8000-0000000000e3', 'openshell/sandbox-qual', 'sandbox', NULL),
  ('evidence-site', '00000000-0000-4000-8000-0000000000e4', 'openshell/sandbox-two', 'sandbox', NULL)
  ON CONFLICT DO NOTHING;
INSERT INTO evidence.adapter_bindings (tenant_id, producer_id, role_name) VALUES
  ('evidence-site', '00000000-0000-4000-8000-0000000000e3', 'evidence_adapter_qual'),
  ('evidence-site', '00000000-0000-4000-8000-0000000000e4', 'evidence_adapter_two')
  ON CONFLICT DO NOTHING;
INSERT INTO evidence.producer_grants (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, all_runs,
    event_types, actor_role)
VALUES
  ('evidence-site', '00000000-0000-4000-8000-0000000000e1', 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1',
   NULL, true, ARRAY['step.started', 'step.finished', 'review.accepted'], 'broker'),
  ('evidence-site', '00000000-0000-4000-8000-0000000000e3', 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1',
   '00000000-0000-4000-8000-00000000a0a1', false, ARRAY['sandbox.receipt', 'review.accepted'], 'sandbox'),
  ('evidence-site', '00000000-0000-4000-8000-0000000000e4', 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1',
   '00000000-0000-4000-8000-00000000a0a2', false, ARRAY['sandbox.receipt'], 'sandbox'),
  ('evidence-site', '00000000-0000-4000-8000-0000000000e1', 'lab-a', 'lab', NULL, NULL, false, ARRAY['lab.note'], 'broker')
  ON CONFLICT DO NOTHING;
-- Migration 079 closes untrusted producers' event types to registered schemas. This site's checks use free-form
-- sandbox payloads, so it registers an open one; broker_evidence_custody_qualification.sql registers the real schema.
SELECT evidence.register_event_schema('evidence-site', 'sandbox.receipt', '{"type":"object","open":true}');

-- Helpers: append as the current session user, and expect a statement to fail with a named message.
CREATE OR REPLACE FUNCTION pg_temp.put(p_producer text, p_key text, p_payload text, p_event text DEFAULT 'step.started',
  p_project uuid DEFAULT '00000000-0000-4000-8000-0000000000a1', p_run uuid DEFAULT '00000000-0000-4000-8000-00000000a0a1',
  p_group text DEFAULT 'lab-a', p_digest text DEFAULT NULL)
RETURNS jsonb LANGUAGE sql AS $$
  SELECT evidence.append('evidence-site', p_producer, p_group, 'project', p_project, p_run, p_event, p_key,
    '2026-10-23T12:00:00Z'::timestamptz, p_payload,
    coalesce(p_digest, encode(sha256(convert_to(p_payload, 'UTF8')), 'hex')))
$$;
CREATE OR REPLACE FUNCTION pg_temp.refused(p_sql text, p_message text) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE p_sql;
  RAISE EXCEPTION 'expected refusal % did not happen for: %', p_message, p_sql;
EXCEPTION WHEN OTHERS THEN
  IF SQLERRM NOT LIKE p_message || '%' THEN
    RAISE EXCEPTION 'expected % but got % for: %', p_message, SQLERRM, p_sql;
  END IF;
END;
$$;

-- 1. The API role appends as its broker producer; an identical retry returns the original receipt.
SET SESSION AUTHORIZATION broker_api_qual;
DO $$
DECLARE first jsonb; again jsonb; second jsonb;
BEGIN
  first := pg_temp.put('broker/api', 'qual-key-0001', '{"step":"profile","n":1}');
  IF (first->>'sequence')::bigint <> 1 OR (first->>'replayed')::boolean THEN
    RAISE EXCEPTION 'first append wrong: %', first;
  END IF;
  again := pg_temp.put('broker/api', 'qual-key-0001', '{"step":"profile","n":1}');
  IF again->>'record_id' <> first->>'record_id' OR NOT (again->>'replayed')::boolean THEN
    RAISE EXCEPTION 'identical retry did not return the original receipt: % vs %', again, first;
  END IF;
  -- 2. The same key with another request is refused, including a source time that differs only by era.
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0001', '{"step":"profile","n":2}')$q$,
    'evidence:idempotency_conflict');
  PERFORM pg_temp.refused($q$SELECT evidence.append('evidence-site', 'broker/api', 'lab-a', 'project',
    '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a1', 'step.started', 'qual-key-0001',
    '2026-10-23 12:00:00+00 BC'::timestamptz, '{"step":"profile","n":1}',
    encode(sha256(convert_to('{"step":"profile","n":1}', 'UTF8')), 'hex'))$q$, 'evidence:source_time_invalid');
  PERFORM pg_temp.refused($q$SELECT evidence.append('evidence-site', 'broker/api', 'lab-a', 'project',
    '00000000-0000-4000-8000-0000000000a1', NULL, 'step.started', 'qual-key-0019', clock_timestamp() + interval '2 days',
    '{"x":1}', encode(sha256(convert_to('{"x":1}', 'UTF8')), 'hex'))$q$, 'evidence:source_time_invalid');
  second := pg_temp.put('broker/api', 'qual-key-0002', '{"step":"profile","n":2}', 'step.finished');
  IF (second->>'sequence')::bigint <> 2 THEN RAISE EXCEPTION 'second append wrong: %', second; END IF;
  -- 3. Grants bound labs, projects, runs and event types; scopes must be real.
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0003', '{"x":1}', 'step.started',
    '00000000-0000-4000-8000-0000000000b1', '00000000-0000-4000-8000-00000000b0b1', 'lab-b')$q$, 'evidence:grant_missing');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0004', '{"x":1}', 'model.call')$q$,
    'evidence:grant_missing');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0005', '{"x":1}', 'step.started',
    '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000b0b1')$q$, 'evidence:scope_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0006', '{"x":1}', 'step.started',
    '00000000-0000-4000-8000-0000000000b1', NULL, 'lab-a')$q$, 'evidence:scope_invalid');
  -- 4. Payloads: digest, object and JSON checks.
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0007', '{"x":1}', 'step.started',
    '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a1', 'lab-a', repeat('0', 64))$q$,
    'evidence:payload_digest_mismatch');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0008', '[1,2]')$q$, 'evidence:payload_not_object');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0009', '{"x":')$q$, 'evidence:payload_not_json');
  -- 5. A broker role cannot append as another producer, nor as a producer that only an adapter may name.
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/worker', 'qual-key-0010', '{"x":1}')$q$, 'evidence:producer_not_caller');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'qual-key-0011', '{"x":1}', 'sandbox.receipt')$q$,
    'evidence:producer_not_caller');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/unknown', 'qual-key-0012', '{"x":1}')$q$, 'evidence:producer_unknown');
END $$;

-- 6. The caller's tenant setting is the same after an append; a project-level record (no run) uses the all-runs grant.
DO $$
DECLARE r jsonb;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', 'caller-setting', true);
  r := pg_temp.put('broker/api', 'qual-key-0013', '{"note":"project"}', 'step.started',
    '00000000-0000-4000-8000-0000000000a1', NULL);
  IF current_setting('enrichedlab.tenant_id', true) <> 'caller-setting' THEN
    RAISE EXCEPTION 'append left the tenant setting at %', current_setting('enrichedlab.tenant_id', true);
  END IF;
  IF (r->>'sequence')::bigint <> 1 THEN RAISE EXCEPTION 'project stream append wrong: %', r; END IF;
END $$;

-- 7. A runtime role has no table privilege: it cannot read, write, truncate, drop or detach, and cannot verify.
DO $$
BEGIN
  PERFORM pg_temp.refused('SELECT count(*) FROM evidence.records', 'permission denied');
  PERFORM pg_temp.refused('SELECT count(*) FROM evidence.receipts', 'permission denied');
  PERFORM pg_temp.refused($q$INSERT INTO evidence.adapter_bindings (tenant_id, producer_id, role_name)
    VALUES ('evidence-site', '00000000-0000-4000-8000-0000000000e4', 'broker_api_qual')$q$, 'permission denied');
  PERFORM pg_temp.refused('TRUNCATE evidence.records', 'permission denied');
  PERFORM pg_temp.refused('DROP TABLE evidence.receipts', 'must be owner');
  PERFORM pg_temp.refused(format('ALTER TABLE evidence.records DETACH PARTITION evidence.%I',
    (SELECT relname FROM pg_class WHERE relname LIKE 'records_y%' ORDER BY relname LIMIT 1)), 'must be owner');
END $$;
RESET SESSION AUTHORIZATION;
SET SESSION AUTHORIZATION broker_worker_qual;
DO $$ BEGIN
  PERFORM pg_temp.refused($q$SELECT evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001',
    (SELECT gen_random_uuid()), 'review')$q$, 'permission denied');
END $$;
RESET SESSION AUTHORIZATION;

-- 8. An adapter names only its bound producer, only for that producer's run; a sandbox never writes a review record.
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
DECLARE r jsonb;
BEGIN
  r := pg_temp.put('openshell/sandbox-qual', 'qual-key-0101', '{"receipt":"run-1"}', 'sandbox.receipt');
  IF (r->>'sequence')::bigint <> 3 THEN RAISE EXCEPTION 'sandbox append did not continue the run chain: %', r; END IF;
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'qual-key-0102', '{"x":1}', 'review.accepted')$q$,
    'evidence:event_type_reserved');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0103', '{"x":1}')$q$, 'evidence:producer_not_caller');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-two', 'qual-key-0104', '{"x":1}', 'sandbox.receipt',
    '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a2')$q$, 'evidence:producer_not_caller');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'qual-key-0105', '{"x":1}', 'sandbox.receipt',
    '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a2')$q$, 'evidence:grant_missing');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'qual-key-0106', '{"x":1}', 'sandbox.receipt',
    '00000000-0000-4000-8000-0000000000a1', NULL)$q$, 'evidence:grant_missing');
END $$;
RESET SESSION AUTHORIZATION;
SET SESSION AUTHORIZATION evidence_adapter_two;
DO $$
DECLARE r jsonb;
BEGIN
  r := pg_temp.put('openshell/sandbox-two', 'qual-key-0201', '{"receipt":"run-2"}', 'sandbox.receipt',
    '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a2');
  IF (r->>'sequence')::bigint <> 1 THEN RAISE EXCEPTION 'second sandbox append wrong: %', r; END IF;
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'qual-key-0202', '{"x":1}', 'sandbox.receipt')$q$,
    'evidence:producer_not_caller');
END $$;
RESET SESSION AUTHORIZATION;

-- 9. Verification answers only for a person who may read the stream, for a named purpose, records each answered read,
-- and leaves the caller's tenant setting alone. A lab member who does not own the project reads it at member scope.
SET SESSION AUTHORIZATION broker_api_qual;
DO $$
DECLARE s uuid; v jsonb;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', 'caller-setting', true);
  SELECT (pg_temp.put('broker/api', 'qual-key-0001', '{"step":"profile","n":1}')->>'stream_id')::uuid INTO s;
  v := evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001', s, 'review');
  IF NOT (v->>'valid')::boolean OR (v->>'records')::int <> 3 THEN RAISE EXCEPTION 'chain does not verify: %', v; END IF;
  IF current_setting('enrichedlab.tenant_id', true) <> 'caller-setting' THEN
    RAISE EXCEPTION 'verification left the tenant setting changed';
  END IF;
  v := evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000004', s, 'reproduce');
  IF NOT (v->>'valid')::boolean THEN RAISE EXCEPTION 'a lab member cannot verify: %', v; END IF;
  PERFORM pg_temp.refused(format($q$SELECT evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000002', %L, 'review')$q$, s),
    'evidence:stream_unknown');
  PERFORM pg_temp.refused(format($q$SELECT evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000003', %L, 'review')$q$, s),
    'evidence:stream_unknown');
  PERFORM pg_temp.refused(format($q$SELECT evidence.verify_stream('another-site', '00000000-0000-4000-8000-000000000001', %L, 'review')$q$, s),
    'evidence:stream_unknown');
  PERFORM pg_temp.refused(format($q$SELECT evidence.verify_stream('evidence-site', NULL, %L, 'review')$q$, s),
    'evidence:stream_unknown');
  PERFORM pg_temp.refused(format($q$SELECT evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001', %L, 'curiosity')$q$, s),
    'evidence:purpose_invalid');
END $$;
RESET SESSION AUTHORIZATION;
DO $$
DECLARE n bigint;
BEGIN
  SELECT count(*) INTO n FROM evidence.reads WHERE reader = 'verify_stream' AND purpose IN ('review', 'reproduce');
  IF n <> 2 THEN RAISE EXCEPTION 'expected 2 recorded reads, found %', n; END IF;
  SELECT count(*) INTO n FROM evidence.reads WHERE actor_id = '00000000-0000-4000-8000-000000000004' AND scope = 'member';
  IF n <> 1 THEN RAISE EXCEPTION 'the member read was not recorded at member scope'; END IF;
  SELECT count(*) INTO n FROM evidence.reads WHERE actor_id = '00000000-0000-4000-8000-000000000001' AND scope = 'restricted';
  IF n <> 1 THEN RAISE EXCEPTION 'the owner read was not recorded at restricted scope'; END IF;
END $$;

-- 9b. A disabled tenant's evidence reads as unknown, for project and lab-level streams alike.
SET SESSION AUTHORIZATION broker_api_qual;
DO $$
DECLARE lab_stream uuid; run_stream uuid; person constant uuid := '00000000-0000-4000-8000-000000000001';
BEGIN
  lab_stream := (evidence.append('evidence-site', 'broker/api', 'lab-a', 'lab', NULL, NULL, 'lab.note', 'qual-key-0301',
    '2026-10-23T12:00:00Z', '{"note":"lab"}', encode(sha256(convert_to('{"note":"lab"}', 'UTF8')), 'hex'))->>'stream_id')::uuid;
  IF NOT (evidence.verify_stream('evidence-site', person, lab_stream, 'review')->>'valid')::boolean THEN
    RAISE EXCEPTION 'a lab member cannot verify the lab stream';
  END IF;
  SELECT (pg_temp.put('broker/api', 'qual-key-0001', '{"step":"profile","n":1}')->>'stream_id')::uuid INTO run_stream;
  PERFORM set_config('qual.lab_stream', lab_stream::text, false);
  PERFORM set_config('qual.run_stream', run_stream::text, false);
END $$;
RESET SESSION AUTHORIZATION;
UPDATE enrichedlab_agent.broker_tenants SET enabled = false WHERE tenant_id = 'evidence-site';
SET SESSION AUTHORIZATION broker_api_qual;
DO $$
BEGIN
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'qual-key-0302', '{"x":1}')$q$, 'evidence:tenant_disabled');
  PERFORM pg_temp.refused(format($q$SELECT evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001', %L, 'review')$q$,
    current_setting('qual.lab_stream')), 'evidence:stream_unknown');
  PERFORM pg_temp.refused(format($q$SELECT evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001', %L, 'review')$q$,
    current_setting('qual.run_stream')), 'evidence:stream_unknown');
END $$;
RESET SESSION AUTHORIZATION;
UPDATE enrichedlab_agent.broker_tenants SET enabled = true WHERE tenant_id = 'evidence-site';

-- 10. Row security is the second line: a non-owner reader sees only its tenant's rows.
SELECT set_config('qual.expected_records', count(*)::text, false) FROM evidence.records WHERE tenant_id = 'evidence-site';
GRANT USAGE ON SCHEMA evidence TO evidence_rls_probe;
GRANT SELECT ON evidence.records TO evidence_rls_probe;
SET SESSION AUTHORIZATION evidence_rls_probe;
DO $$
DECLARE n bigint;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', 'another-site', false);
  SELECT count(*) INTO n FROM evidence.records;
  IF n <> 0 THEN RAISE EXCEPTION 'another tenant sees % records', n; END IF;
  PERFORM set_config('enrichedlab.tenant_id', 'evidence-site', false);
  SELECT count(*) INTO n FROM evidence.records;
  IF n::text <> current_setting('qual.expected_records') OR n < 5 THEN
    RAISE EXCEPTION 'own tenant sees % records, expected %', n, current_setting('qual.expected_records');
  END IF;
END $$;
RESET SESSION AUTHORIZATION;
REVOKE SELECT ON evidence.records FROM evidence_rls_probe;
REVOKE USAGE ON SCHEMA evidence FROM evidence_rls_probe;
SELECT set_config('enrichedlab.tenant_id', 'evidence-site', false);

-- 11. Receipts and records never change, even for their owner. A change made with triggers off (a database superuser)
-- does not verify, and verifies again once restored.
DO $$
DECLARE s uuid; original text; v jsonb;
BEGIN
  PERFORM pg_temp.refused('UPDATE evidence.records SET payload = payload', 'evidence:append_only');
  PERFORM pg_temp.refused('DELETE FROM evidence.receipts', 'evidence:append_only');
  PERFORM pg_temp.refused('TRUNCATE evidence.records', 'evidence:append_only');
  SELECT stream_id INTO s FROM evidence.streams WHERE run_id = '00000000-0000-4000-8000-00000000a0a1';
  SELECT payload INTO original FROM evidence.records WHERE stream_id = s AND sequence = 2;
  PERFORM set_config('session_replication_role', 'replica', true);
  UPDATE evidence.records SET payload = '{"step":"profile","n":9}' WHERE stream_id = s AND sequence = 2;
  v := evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001', s, 'audit');
  IF (v->>'valid')::boolean OR v->>'reason' <> 'hash_mismatch' OR (v->>'at_sequence')::int <> 2 THEN
    RAISE EXCEPTION 'a changed payload verified: %', v;
  END IF;
  UPDATE evidence.records SET previous_hash = repeat('f', 64), payload = original WHERE stream_id = s AND sequence = 2;
  v := evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001', s, 'audit');
  IF (v->>'valid')::boolean OR v->>'reason' <> 'chain_break' THEN RAISE EXCEPTION 'a broken link verified: %', v; END IF;
  UPDATE evidence.records r SET previous_hash = p.record_hash FROM evidence.records p
    WHERE r.stream_id = s AND r.sequence = 2 AND p.stream_id = s AND p.sequence = 1;
  PERFORM set_config('session_replication_role', 'origin', true);
  IF NOT (evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001', s, 'audit')->>'valid')::boolean THEN
    RAISE EXCEPTION 'the restored chain does not verify';
  END IF;
END $$;

-- 12. Seven months of record and read partitions exist, each with forced row security.
DO $$
DECLARE n int;
BEGIN
  SELECT count(*) INTO n FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
    WHERE i.inhparent IN ('evidence.records'::regclass, 'evidence.reads'::regclass)
      AND c.relrowsecurity AND c.relforcerowsecurity;
  IF n < 14 THEN RAISE EXCEPTION 'expected at least 14 secured partitions, found %', n; END IF;
END $$;

SELECT 'evidence record qualification passed' AS result;
