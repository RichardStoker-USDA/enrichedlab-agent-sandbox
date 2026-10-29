-- Evidence lineage qualification (migration 076). Runs after broker_evidence_qualification.sql in
-- the same database, on its own finished run so that file's counts stay exact. Every check raises on failure.
\set ON_ERROR_STOP on
SET client_min_messages = warning;
SELECT set_config('enrichedlab.tenant_id', 'evidence-site', false);
INSERT INTO enrichedlab_agent.broker_research_runs (tenant_id, run_id, project_id, actor_id, initial_revision,
    allowed_profiles, max_turns, max_parallel, status)
  VALUES ('evidence-site', '00000000-0000-4000-8000-00000000a0a3', '00000000-0000-4000-8000-0000000000a1',
          '00000000-0000-4000-8000-000000000001', 0, ARRAY['analysis'], 4, 1, 'failed')
  ON CONFLICT DO NOTHING;
INSERT INTO evidence.producer_grants (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, event_types,
    actor_role)
  VALUES ('evidence-site', '00000000-0000-4000-8000-0000000000e3', 'lab-a', 'project',
          '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a3', ARRAY['sandbox.receipt'], 'sandbox')
  ON CONFLICT DO NOTHING;

CREATE OR REPLACE FUNCTION pg_temp.put(p_producer text, p_key text, p_payload text, p_event text DEFAULT 'step.started')
RETURNS jsonb LANGUAGE sql AS $$
  SELECT evidence.append('evidence-site', p_producer, 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1',
    '00000000-0000-4000-8000-00000000a0a3', p_event, p_key, '2026-10-23T12:00:00Z'::timestamptz, p_payload,
    encode(sha256(convert_to(p_payload, 'UTF8')), 'hex'))
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

SET SESSION AUTHORIZATION broker_api_qual;
DO $$
DECLARE r jsonb; s uuid; t jsonb; held bigint; person constant uuid := '00000000-0000-4000-8000-000000000001';
BEGIN
  -- Step 4: a model planned the analysis over an imported table; a tool produced a result table.
  r := pg_temp.put('broker/api', 'lin-key-0001', $p${"lineage":{"version":1,
    "activity":{"key":"step-4","kind":"step","event":"started"},
    "agents":[{"kind":"model","identity":"open-model","version":"2026-10","role":"planner"},
              {"kind":"tool","identity":"r-analysis","version":"4.5.1","image_digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","role":"executor"}],
    "entities":[{"key":"dataset:intake-1","kind":"dataset","sha256":"2222222222222222222222222222222222222222222222222222222222222222","size":1024,"media_type":"text/csv","disclosure":"project","origin":"imported"}],
    "used":[{"entity":"dataset:intake-1","role":"input"}],
    "model_call":{"agent":0,"decoding":{"temperature":0},"prompt_sha256":"3333333333333333333333333333333333333333333333333333333333333333","completion_sha256":"4444444444444444444444444444444444444444444444444444444444444444","input_tokens":900,"output_tokens":120,"latency_ms":850}}}$p$);
  r := pg_temp.put('broker/api', 'lin-key-0002', $p${"lineage":{"version":1,
    "activity":{"key":"step-4","kind":"step","event":"ended"},
    "entities":[{"key":"result:step-4","kind":"artifact","sha256":"5555555555555555555555555555555555555555555555555555555555555555","size":2048,"media_type":"text/csv","disclosure":"project"}],
    "generated":[{"entity":"result:step-4","role":"output"}],
    "derived":[{"from":"dataset:intake-1","to":"result:step-4","transform":"r.analysis"}]}}$p$, 'step.finished');
  -- Step 19 used step 4's output; step 4 informed it.
  r := pg_temp.put('broker/api', 'lin-key-0003', $p${"lineage":{"version":1,
    "activity":{"key":"step-19","kind":"step","event":"started"},
    "used":[{"entity":"result:step-4","role":"input"}],"informed_by":["step-4"]}}$p$);
  s := (r->>'stream_id')::uuid;

  -- "Step 19's input was step 4's output", read the way the broker reads it: for a person, with a purpose. Every
  -- element names the record and producer that asserted it.
  t := evidence.trace_activity('evidence-site', person, s, 'step-19', 'review');
  IF t #>> '{used,0,entity}' <> 'result:step-4' OR t #>> '{used,0,generated_by}' <> 'step-4'
    OR t #>> '{informed_by,0,activity}' <> 'step-4' OR t #>> '{used,0,producer}' <> 'broker/api'
    OR t #>> '{used,0,record}' IS NULL OR t #>> '{activity,registered_by}' <> 'broker/api'
    OR t->>'scope' <> 'restricted' OR t->'truncated' <> '[]'::jsonb THEN
    RAISE EXCEPTION 'step 19 trace wrong: %', t;
  END IF;
  t := evidence.trace_activity('evidence-site', person, s, 'step-4', 'reproduce');
  IF jsonb_array_length(t->'agents') <> 2 OR jsonb_array_length(t->'events') <> 2
    OR t #>> '{generated,0,derived_from,0,entity}' <> 'dataset:intake-1'
    OR t #>> '{generated,0,derived_from,0,producer}' <> 'broker/api'
    OR t #>> '{used,0,origin}' <> 'imported' OR t::text LIKE '%"uri"%' OR t::text LIKE '%object_uri%' THEN
    RAISE EXCEPTION 'step 4 trace wrong: %', t;
  END IF;
  PERFORM pg_temp.refused(format($q$SELECT evidence.trace_activity('evidence-site', '00000000-0000-4000-8000-000000000002', %L, 'step-4', 'review')$q$, s),
    'evidence:stream_unknown');
  PERFORM pg_temp.refused(format($q$SELECT evidence.trace_activity('evidence-site', '00000000-0000-4000-8000-000000000003', %L, 'step-4', 'review')$q$, s),
    'evidence:stream_unknown');
  PERFORM pg_temp.refused(format($q$SELECT evidence.trace_activity('evidence-site', '00000000-0000-4000-8000-000000000001', %L, 'step-99', 'review')$q$, s),
    'evidence:activity_unknown');
  PERFORM pg_temp.refused(format($q$SELECT evidence.trace_activity('evidence-site', '00000000-0000-4000-8000-000000000001', %L, 'step-4', NULL)$q$, s),
    'evidence:purpose_invalid');

  -- Malformed or dishonest lineage is refused, and the refusal leaves no record behind.
  held := (evidence.verify_stream('evidence-site', person, s, 'audit')->>'records')::bigint;
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0010', '{"lineage":{"version":1,"activity":{"key":"step-20","kind":"step"},"generated":[{"entity":"result:undeclared","role":"output"}]}}')$q$,
    'evidence:lineage_generated_not_declared');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0011', '{"lineage":{"version":1,"activity":{"key":"step-21","kind":"step"},"entities":[{"key":"result:step-4","kind":"artifact","disclosure":"project"}],"generated":[{"entity":"result:step-4","role":"output"}]}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0012', '{"lineage":{"version":1,"activity":{"key":"step-22","kind":"step"},"used":[{"entity":"dataset:never-declared","role":"input"}]}}')$q$,
    'evidence:lineage_entity_unknown');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0013', '{"lineage":{"version":1,"surprise":true}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0014', '{"lineage":{"version":1,"used":[{"entity":"dataset:intake-1","role":"input"}]}}')$q$,
    'evidence:lineage_activity_missing');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0015', '{"lineage":{"version":1,"activity":{"key":"step-4","kind":"analysis"}}}')$q$,
    'evidence:lineage_activity_kind_changed');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0016', '{"lineage":{"version":1,"activity":{"key":"step-23","kind":"step"},"agents":[{"kind":"tool","identity":"x","version":"1","image_digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","role":"executor"}],"model_call":{"agent":0,"prompt_sha256":"3333333333333333333333333333333333333333333333333333333333333333","completion_sha256":"4444444444444444444444444444444444444444444444444444444444444444","input_tokens":1,"output_tokens":1,"latency_ms":1}}}')$q$,
    'evidence:lineage_model_call_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0017', '{"lineage":{"version":1,"activity":{"key":"step-24","kind":"step","event":"started","at":"not a time"}}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0018', '{"lineage":{"version":1,"activity":{"key":"step-25","kind":"step"},"entities":[{"key":"dataset:intake-1","kind":"dataset","sha256":"9999999999999999999999999999999999999999999999999999999999999999","size":1024,"media_type":"text/csv","disclosure":"project","origin":"imported"}]}}')$q$,
    'evidence:lineage_entity_conflict');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0019', '{"lineage":{"version":1,"activity":{"key":"step-26","kind":"step"},"agents":[{"kind":"model","identity":"unnamed-model","role":"planner"}]}}')$q$,
    'evidence:lineage_model_identity_missing');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0020', '{"lineage":{"version":1,"activity":{"key":"step-27","kind":"step"},"used":[{"entity":"dataset:intake-1","role":"input","note":"extra"}]}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0021', '{"lineage":{"version":1,"activity":{"key":"step-28","kind":"step"},"used":"dataset:intake-1"}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0022', '{"lineage":{"version":1,"activity":{"key":"step-29","kind":"step"},"derived":[{"from":"dataset:intake-1","to":"result:step-4","transform":"copy"}]}}')$q$,
    'evidence:lineage_derived_target');
  -- Member values must have their JSON types; no blank strings, no nulls.
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0024', '{"lineage":{"version":"1"}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0025', '{"lineage":{"version":1,"activity":{"key":"step-31","kind":"step"},"agents":[{"kind":"model","identity":{"name":"x"},"version":"1","role":"planner"}]}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0026', '{"lineage":{"version":1,"activity":{"key":"step-32","kind":"step"},"agents":[{"kind":"model","identity":"   ","version":"1","role":"planner"}]}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0027', '{"lineage":{"version":1,"activity":{"key":"step-33","kind":"step"},"agents":[{"kind":"model","identity":"m","version":null,"image_digest":null,"role":"planner"}]}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0029', '{"lineage":{"version":1,"activity":{"key":"step-35","kind":"step"},"agents":[{"kind":"model","identity":"\t\t","version":"\t","role":"planner"}]}}')$q$,
    'evidence:lineage_invalid');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0030', '{"lineage":{"version":1,"activity":{"key":"step-36","kind":"step"},"agents":[{"kind":"tool","identity":"r-analysis","version":"4.5","role":"executor"}]}}')$q$,
    'evidence:lineage_tool_identity_missing');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0031', '{"lineage":{"version":1,"activity":{"key":"step-37","kind":"step"},"agents":[{"kind":"tool","identity":"r-analysis","image_digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","role":"executor"}]}}')$q$,
    'evidence:lineage_tool_identity_missing');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('broker/api', 'lin-key-0028', '{"lineage":{"version":1,"activity":{"key":"step-34","kind":"step"},"agents":[{"kind":"model","identity":"m","version":"1","role":"planner"}],"model_call":{"agent":0,"prompt_sha256":"3333333333333333333333333333333333333333333333333333333333333333","completion_sha256":"4444444444444444444444444444444444444444444444444444444444444444","input_tokens":"900","output_tokens":1,"latency_ms":1}}}')$q$,
    'evidence:lineage_model_call_invalid');
  IF (evidence.verify_stream('evidence-site', person, s, 'audit')->>'records')::bigint <> held THEN
    RAISE EXCEPTION 'a refused lineage left a record behind';
  END IF;
  -- The same imported entity declared again, identically, is the same entity.
  PERFORM pg_temp.put('broker/api', 'lin-key-0023', '{"lineage":{"version":1,"activity":{"key":"step-30","kind":"step"},"entities":[{"key":"dataset:intake-1","kind":"dataset","sha256":"2222222222222222222222222222222222222222222222222222222222222222","size":1024,"media_type":"text/csv","disclosure":"project","origin":"imported"}],"used":[{"entity":"dataset:intake-1","role":"input"}]}}');
  IF NOT (evidence.verify_stream('evidence-site', person, s, 'audit')->>'valid')::boolean THEN
    RAISE EXCEPTION 'the lineage run chain does not verify';
  END IF;
END $$;
RESET SESSION AUTHORIZATION;

-- The graph rows the appends wrote.
DO $$
DECLARE n bigint;
BEGIN
  SELECT count(*) INTO n FROM evidence.model_calls; IF n <> 1 THEN RAISE EXCEPTION 'model calls %', n; END IF;
  SELECT count(*) INTO n FROM evidence.associated; IF n <> 2 THEN RAISE EXCEPTION 'associations %', n; END IF;
  SELECT count(*) INTO n FROM evidence.derived; IF n <> 1 THEN RAISE EXCEPTION 'derivations %', n; END IF;
  SELECT count(*) INTO n FROM evidence.informed; IF n <> 1 THEN RAISE EXCEPTION 'informed edges %', n; END IF;
  SELECT count(*) INTO n FROM evidence.entities WHERE origin = 'produced' AND generated_by IS NOT NULL;
  IF n <> 1 THEN RAISE EXCEPTION 'produced entities %', n; END IF;
END $$;

-- A sandbox producer cannot name a person or record a review activity; it can record its own run.
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
BEGIN
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'lin-key-0101', '{"lineage":{"version":1,"activity":{"key":"sbx-1","kind":"sandbox_run"},"agents":[{"kind":"person","identity":"someone","role":"approver"}]}}', 'sandbox.receipt')$q$,
    'evidence:lineage_person_reserved');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'lin-key-0102', '{"lineage":{"version":1,"activity":{"key":"sbx-2","kind":"review"}}}', 'sandbox.receipt')$q$,
    'evidence:lineage_kind_reserved');
  PERFORM pg_temp.put('openshell/sandbox-qual', 'lin-key-0103', '{"lineage":{"version":1,"activity":{"key":"sbx-3","kind":"sandbox_run","event":"ended"},"agents":[{"kind":"sandbox","identity":"openshell/sandbox-qual","role":"runtime"}]}}', 'sandbox.receipt');
  -- A sandbox cannot extend the broker's activity, nor claim an output another activity generated.
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'lin-key-0104', '{"lineage":{"version":1,"activity":{"key":"step-4","kind":"step","event":"failed"}}}', 'sandbox.receipt')$q$,
    'evidence:lineage_activity_not_owned');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'lin-key-0105', '{"lineage":{"version":1,"activity":{"key":"sbx-4","kind":"sandbox_run"},"derived":[{"from":"dataset:intake-1","to":"result:step-4","transform":"r.analysis"}]}}', 'sandbox.receipt')$q$,
    'evidence:lineage_derived_target');
  -- It may name the broker's activity as the one that informed its own.
  PERFORM pg_temp.put('openshell/sandbox-qual', 'lin-key-0106', '{"lineage":{"version":1,"activity":{"key":"sbx-5","kind":"sandbox_run","event":"started"},"used":[{"entity":"result:step-4","role":"input"}],"informed_by":["step-4"]}}', 'sandbox.receipt');
END $$;
RESET SESSION AUTHORIZATION;

-- Lineage rows are immutable, and runtime roles can neither read them nor call the internal writer.
DO $$
BEGIN
  PERFORM pg_temp.refused('UPDATE evidence.entities SET disclosure = disclosure', 'evidence:append_only');
  PERFORM pg_temp.refused('DELETE FROM evidence.used', 'evidence:append_only');
  PERFORM pg_temp.refused('TRUNCATE evidence.model_calls', 'evidence:append_only');
END $$;
SET SESSION AUTHORIZATION broker_worker_qual;
DO $$ BEGIN
  PERFORM pg_temp.refused('SELECT count(*) FROM evidence.entities', 'permission denied');
  PERFORM pg_temp.refused($q$SELECT evidence.write_lineage('evidence-site',gen_random_uuid(),'broker',gen_random_uuid(),'lab-a',NULL,gen_random_uuid(),now(),now(),'{"version":1}')$q$,
    'permission denied');
  PERFORM pg_temp.refused($q$SELECT evidence.trace_activity('evidence-site','00000000-0000-4000-8000-000000000001',gen_random_uuid(),'step-4','review')$q$,
    'permission denied');
END $$;
RESET SESSION AUTHORIZATION;

-- Restricted entities are listed for the project's owner and withheld from a lab member; a long section is cut at 200.
SET SESSION AUTHORIZATION broker_api_qual;
DO $$
DECLARE r jsonb; s uuid; t jsonb; payload text;
BEGIN
  r := pg_temp.put('broker/api', 'lin-key-0201', $p${"lineage":{"version":1,
    "activity":{"key":"step-40","kind":"step","event":"started"},
    "entities":[{"key":"dataset:clinical-1","kind":"dataset","sha256":"6666666666666666666666666666666666666666666666666666666666666666","size":4096,"disclosure":"restricted","origin":"imported"}],
    "used":[{"entity":"dataset:clinical-1","role":"input"}]}}$p$);
  s := (r->>'stream_id')::uuid;
  t := evidence.trace_activity('evidence-site', '00000000-0000-4000-8000-000000000001', s, 'step-40', 'review');
  IF t #>> '{used,0,entity}' <> 'dataset:clinical-1' THEN RAISE EXCEPTION 'owner trace withheld: %', t; END IF;
  t := evidence.trace_activity('evidence-site', '00000000-0000-4000-8000-000000000004', s, 'step-40', 'review');
  IF t->>'scope' <> 'member' OR NOT (t #>> '{used,0,withheld}')::boolean OR t::text LIKE '%clinical-1%'
    OR t::text LIKE '%6666666666%' THEN
    RAISE EXCEPTION 'member trace shows a restricted entity: %', t;
  END IF;
  SELECT jsonb_build_object('lineage', jsonb_build_object('version', 1,
      'activity', jsonb_build_object('key', 'step-41', 'kind', 'step'),
      'entities', jsonb_agg(jsonb_build_object('key', 'dataset:part-' || i, 'kind', 'dataset', 'disclosure', 'project',
        'origin', 'imported')),
      'used', jsonb_agg(jsonb_build_object('entity', 'dataset:part-' || i, 'role', 'input'))))::text
    INTO payload FROM generate_series(1, 201) i;
  r := pg_temp.put('broker/api', 'lin-key-0202', payload);
  t := evidence.trace_activity('evidence-site', '00000000-0000-4000-8000-000000000001', s, 'step-41', 'review');
  IF jsonb_array_length(t->'used') <> 200 OR t->'truncated' <> '["used"]'::jsonb THEN
    RAISE EXCEPTION 'trace not cut at 200: % used, truncated %', jsonb_array_length(t->'used'), t->'truncated';
  END IF;
END $$;
RESET SESSION AUTHORIZATION;
DO $$
DECLARE n bigint;
BEGIN
  SELECT count(*) INTO n FROM evidence.reads WHERE reader = 'trace_activity';
  IF n <> 5 THEN RAISE EXCEPTION 'expected 5 recorded trace reads (refusals record none), found %', n; END IF;
END $$;
SELECT 'evidence lineage qualification passed' AS result;
