-- Evidence custody qualification (migration 079). Runs after broker_evidence_qualification.sql and
-- broker_evidence_lineage_qualification.sql in the same database, on its own finished run. The custody service's part
-- (hashing, writing and reading copies back) is played here by the adapter role calling the same functions with the
-- facts a store returned; tests/evidence_object_stores.rs covers the stores themselves. Every check raises on failure.
\set ON_ERROR_STOP on
SET client_min_messages = warning;
SELECT set_config('enrichedlab.tenant_id', 'evidence-site', false);
INSERT INTO enrichedlab_agent.broker_research_runs (tenant_id, run_id, project_id, actor_id, initial_revision,
    allowed_profiles, max_turns, max_parallel, status)
  VALUES ('evidence-site', '00000000-0000-4000-8000-00000000a0a4', '00000000-0000-4000-8000-0000000000a1',
          '00000000-0000-4000-8000-000000000001', 0, ARRAY['analysis'], 4, 1, 'failed')
  ON CONFLICT DO NOTHING;
INSERT INTO evidence.producers (tenant_id, producer_id, identity, kind, database_role) VALUES
  ('evidence-site', '00000000-0000-4000-8000-0000000000e5', 'gateway/inference-qual', 'inference', NULL),
  ('evidence-site', '00000000-0000-4000-8000-0000000000e6', 'ops/custody-canary', 'clusterops', NULL)
  ON CONFLICT DO NOTHING;
INSERT INTO evidence.adapter_bindings (tenant_id, producer_id, role_name) VALUES
  ('evidence-site', '00000000-0000-4000-8000-0000000000e5', 'evidence_adapter_qual'),
  ('evidence-site', '00000000-0000-4000-8000-0000000000e6', 'evidence_adapter_qual')
  ON CONFLICT DO NOTHING;
INSERT INTO evidence.producer_grants (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, event_types,
    actor_role)
  VALUES ('evidence-site', '00000000-0000-4000-8000-0000000000e3', 'lab-a', 'project',
          '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a4',
          ARRAY['sandbox.receipt', 'custody.store'], 'sandbox'),
         ('evidence-site', '00000000-0000-4000-8000-0000000000e5', 'lab-a', 'project',
          '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a4',
          ARRAY['model.call', 'custody.store'], 'model'),
         ('evidence-site', '00000000-0000-4000-8000-0000000000e1', 'lab-a', 'project',
          '00000000-0000-4000-8000-0000000000a1', NULL, ARRAY['acceptance.project'], 'broker'),
         ('evidence-site', '00000000-0000-4000-8000-0000000000e3', 'lab-a', 'standalone', NULL, NULL,
          ARRAY['custody.store'], 'sandbox'),
         ('evidence-site', '00000000-0000-4000-8000-0000000000e6', 'lab-a', 'standalone', NULL, NULL,
          ARRAY['custody.store', 'custody.canary'], 'service')
  ON CONFLICT DO NOTHING;
GRANT EXECUTE ON FUNCTION evidence.custody_authorise(text,text,text,text,uuid,uuid),
  evidence.custody_issue_claim(text,text,text,text,uuid,uuid,text,text,bigint,text,text,bigint,text,text,timestamptz,boolean),
  evidence.custody_record_copy(text,uuid,text,text,bigint,text,text,timestamptz,boolean),
  evidence.custody_mark_ready(text,text,uuid,uuid), evidence.custody_claim(text,text,uuid),
  evidence.custody_waiting(text,integer,uuid), evidence.custody_due_copies(text,interval,integer),
  evidence.custody_retention_pending(text,integer), evidence.custody_observe(text,uuid,uuid,text,jsonb),
  evidence.custody_epoch_check(text), evidence.custody_complete_acceptances(text)
  TO evidence_adapter_qual;
-- Lab A's two stores; lab B has none.
SELECT evidence.custody_register_store('evidence-site', 'lab-a', 'primary', 'store-primary');
SELECT evidence.custody_register_store('evidence-site', 'lab-a', 'replica', 'store-replica');
SELECT evidence.custody_register_store('evidence-site', 'lab-a', 'primary', 'store-primary');
SELECT set_config('qual.bucket', evidence.custody_bucket_name('evidence-site', 'lab-a'), false);
\set model_call_schema `cat /workspace/schemas/evidence/model.call.v1.json`
\set receipt_schema `cat /workspace/schemas/evidence/sandbox.receipt.v1.json`
SELECT evidence.register_event_schema('evidence-site', 'model.call', :'model_call_schema'::jsonb);
\set canary_schema `cat /workspace/schemas/evidence/custody.canary.v1.json`
SELECT evidence.register_event_schema('evidence-site', 'custody.canary', :'canary_schema'::jsonb);

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
-- Append in run a0a4 (or another run) as the current session user.
CREATE OR REPLACE FUNCTION pg_temp.put(p_producer text, p_key text, p_payload text, p_event text,
  p_run uuid DEFAULT '00000000-0000-4000-8000-00000000a0a4')
RETURNS jsonb LANGUAGE sql AS $$
  SELECT evidence.append('evidence-site', p_producer, 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1',
    p_run, p_event, p_key, '2026-10-24T12:00:00Z'::timestamptz, p_payload,
    encode(sha256(convert_to(p_payload, 'UTF8')), 'hex'))
$$;
-- A claim as the service makes one: authorise, then record the primary copy it wrote and read back.
CREATE OR REPLACE FUNCTION pg_temp.claim(p_producer text, p_key text, p_text text, p_days integer DEFAULT 91,
  p_run uuid DEFAULT '00000000-0000-4000-8000-00000000a0a4')
RETURNS jsonb LANGUAGE sql AS $$
  SELECT evidence.custody_issue_claim('evidence-site', p_producer, 'lab-a', 'project',
    '00000000-0000-4000-8000-0000000000a1', p_run, p_key, encode(sha256(convert_to(p_text, 'UTF8')), 'hex'),
    octet_length(p_text), 'text/plain', 'v-' || left(encode(sha256(convert_to(p_text, 'UTF8')), 'hex'), 12),
    octet_length(p_text) + 9, encode(sha256(convert_to('zstd:' || p_text, 'UTF8')), 'hex'), 'GOVERNANCE',
    clock_timestamp() + make_interval(days => p_days), false)
$$;
-- The replica copy of a claim's object (same version and stored bytes unless told otherwise), then `ready`.
CREATE OR REPLACE FUNCTION pg_temp.ready(p_producer text, p_claim jsonb, p_days integer DEFAULT 91)
RETURNS jsonb LANGUAGE sql AS $$
  SELECT evidence.custody_mark_ready('evidence-site', p_producer, (p_claim->>'claim_id')::uuid,
    evidence.custody_record_copy('evidence-site', (p_claim->>'object_id')::uuid, 'replica', p_claim->>'primary_version',
      (p_claim->>'stored_size')::bigint, p_claim->>'stored_sha256', 'GOVERNANCE',
      clock_timestamp() + make_interval(days => p_days), false))
$$;

SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
DECLARE a jsonb; c1 jsonb; c2 jsonb; r jsonb; e record;
BEGIN
  -- Authorisation names both stores and the run's 90-day floor, only for a scope granted custody.store.
  a := evidence.custody_authorise('evidence-site', 'openshell/sandbox-qual', 'lab-a', 'project',
    '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a4');
  IF a #>> '{primary,bucket}' <> current_setting('qual.bucket') OR a->>'class' <> 'run'
    OR a #>> '{replica,endpoint}' <> 'store-replica' OR (a->>'floor_until')::timestamptz < clock_timestamp() + interval '89 days' THEN
    RAISE EXCEPTION 'authorise answer wrong: %', a;
  END IF;
  PERFORM pg_temp.refused($q$SELECT evidence.custody_authorise('evidence-site', 'openshell/sandbox-qual', 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a1')$q$,
    'evidence:grant_missing');
  PERFORM pg_temp.refused($q$SELECT evidence.custody_authorise('evidence-site', 'openshell/sandbox-two', 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1', '00000000-0000-4000-8000-00000000a0a2')$q$,
    'evidence:producer_not_caller');
  PERFORM pg_temp.refused($q$SELECT evidence.custody_authorise('evidence-site', 'openshell/sandbox-qual', 'lab-b', 'project', '00000000-0000-4000-8000-0000000000b1', NULL)$q$,
    'evidence:grant_missing');

  -- A claim, replayed, and a conflicting reuse of its key.
  c1 := pg_temp.claim('openshell/sandbox-qual', 'cus-key-0001', 'artifact bytes one');
  IF (c1->>'ready')::boolean OR (c1->>'replayed')::boolean OR c1->>'class' <> 'run' THEN
    RAISE EXCEPTION 'new claim wrong: %', c1;
  END IF;
  r := pg_temp.claim('openshell/sandbox-qual', 'cus-key-0001', 'artifact bytes one');
  IF r->>'claim_id' <> c1->>'claim_id' OR NOT (r->>'replayed')::boolean THEN RAISE EXCEPTION 'replay wrong: %', r; END IF;
  PERFORM pg_temp.refused($q$SELECT pg_temp.claim('openshell/sandbox-qual', 'cus-key-0001', 'other bytes')$q$,
    'evidence:idempotency_conflict');
  IF (SELECT count(*) FROM evidence.custody_waiting('evidence-site', 10) w WHERE w.claim_id = (c1->>'claim_id')::uuid) <> 1 THEN
    RAISE EXCEPTION 'the waiting claim is not listed';
  END IF;

  -- Binding before the replica is verified is refused; so are producer-supplied byte fields.
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0001', '{"lineage":{"version":1,"activity":{"key":"sbx-c1","kind":"sandbox_run"},"entities":[{"key":"out:c1","kind":"artifact","disclosure":"project","claim":"%s"}],"generated":[{"entity":"out:c1","role":"output"}]}}', 'sandbox.receipt')$q$, c1->>'claim_id'),
    'evidence:custody_unverified');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0002', '{"lineage":{"version":1,"activity":{"key":"sbx-c2","kind":"sandbox_run"},"entities":[{"key":"out:c2","kind":"other","sha256":"2222222222222222222222222222222222222222222222222222222222222222","disclosure":"project"}],"generated":[{"entity":"out:c2","role":"output"}]}}', 'sandbox.receipt')$q$,
    'evidence:custody_required');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0003', '{"lineage":{"version":1,"activity":{"key":"sbx-c3","kind":"sandbox_run"},"entities":[{"key":"out:c3","kind":"paper","media_type":"text/plain","disclosure":"project"}],"generated":[{"entity":"out:c3","role":"output"}]}}', 'sandbox.receipt')$q$,
    'evidence:custody_required');

  -- A replica copy that differs from the primary's stored bytes is refused, at the copy or at readiness.
  PERFORM pg_temp.refused(format($q$SELECT evidence.custody_record_copy('evidence-site', %L, 'replica', %L, 27, '%s', 'GOVERNANCE', clock_timestamp() + interval '91 days', false)$q$,
    c1->>'object_id', c1->>'primary_version', repeat('9', 64)), 'evidence:custody_copy_conflict');
  PERFORM pg_temp.refused(format($q$SELECT evidence.custody_mark_ready('evidence-site', 'openshell/sandbox-qual', %L, evidence.custody_record_copy('evidence-site', %L, 'replica', 'v-other', 27, %L, 'GOVERNANCE', clock_timestamp() + interval '91 days', false))$q$,
    c1->>'claim_id', c1->>'object_id', c1->>'stored_sha256'), 'evidence:custody_replica_mismatch');
  -- A replica retained for less than the floor does not make the claim ready.
  c2 := pg_temp.claim('openshell/sandbox-qual', 'cus-key-0002', 'artifact bytes two');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.ready('openshell/sandbox-qual', %L::jsonb, 30)$q$, c2),
    'evidence:custody_retention_short');

  r := pg_temp.ready('openshell/sandbox-qual', c1);
  IF NOT (r->>'ready')::boolean THEN RAISE EXCEPTION 'claim not ready: %', r; END IF;
  r := pg_temp.ready('openshell/sandbox-qual', c1);
  IF NOT (r->>'replayed')::boolean THEN RAISE EXCEPTION 'ready not idempotent: %', r; END IF;

  -- Another producer cannot read or bind the claim; the same producer cannot bind it in another run.
  PERFORM pg_temp.refused(format($q$SELECT evidence.custody_claim('evidence-site', 'gateway/inference-qual', %L)$q$, c1->>'claim_id'),
    'evidence:custody_claim_unknown');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('gateway/inference-qual', 'cus-rec-0004', '{"lineage":{"version":1,"activity":{"key":"gw-c4","kind":"model_call"},"entities":[{"key":"out:c4","kind":"artifact","disclosure":"project","claim":"%s"}],"generated":[{"entity":"out:c4","role":"output"}]}}', 'model.call')$q$, c1->>'claim_id'),
    'evidence:custody_claim_unknown');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0005', '{"lineage":{"version":1,"activity":{"key":"sbx-c5","kind":"sandbox_run"},"entities":[{"key":"out:c5","kind":"artifact","disclosure":"project","claim":"%s"}],"generated":[{"entity":"out:c5","role":"output"}]}}', 'sandbox.receipt', '00000000-0000-4000-8000-00000000a0a1')$q$, c1->>'claim_id'),
    'evidence:custody_scope');

  -- The bind: the server fills digest, size, media type, location and version, and the reference names the entity.
  r := pg_temp.put('openshell/sandbox-qual', 'cus-rec-0006', format('{"lineage":{"version":1,"activity":{"key":"sbx-c6","kind":"sandbox_run"},"entities":[{"key":"out:c6","kind":"artifact","disclosure":"project","claim":"%s"}],"generated":[{"entity":"out:c6","role":"output"}]}}', c1->>'claim_id'), 'sandbox.receipt');
  PERFORM set_config('qual.c1', c1::text, false);
  PERFORM set_config('qual.rec6', r->>'record_id', false);
  -- A claim and producer-supplied location together are refused; an untrusted producer declares no imported entity.
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0007', '{"lineage":{"version":1,"activity":{"key":"sbx-c7","kind":"sandbox_run"},"entities":[{"key":"out:c7","kind":"artifact","disclosure":"project","claim":"%s","uri":"s3://elsewhere/x"}],"generated":[{"entity":"out:c7","role":"output"}]}}', 'sandbox.receipt')$q$, c1->>'claim_id'),
    'evidence:custody_required');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0008', '{"lineage":{"version":1,"activity":{"key":"sbx-c8","kind":"sandbox_run"},"entities":[{"key":"in:c8","kind":"dataset","disclosure":"project","origin":"external"}],"used":[{"entity":"in:c8","role":"input"}]}}', 'sandbox.receipt')$q$,
    'evidence:lineage_entity_origin');
  -- A member named twice is refused, at any depth, whatever the schema; so is lineage text that is not a token, and an
  -- activity time that is not a time.
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0011', '{"a":"1","a":"what the model said"}', 'sandbox.receipt')$q$,
    'evidence:payload_duplicate_member');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0012', '{"b":{"c":1,"c":2}}', 'sandbox.receipt')$q$,
    'evidence:payload_duplicate_member');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0013', '{"lineage":{"version":1,"activity":{"key":"sbx-c13","kind":"sandbox_run"},"agents":[{"kind":"sandbox","identity":"a model said this","role":"runtime"}]}}', 'sandbox.receipt')$q$,
    'evidence:lineage_not_metadata');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0014', '{"lineage":{"version":1,"activity":{"key":"sbx-c14","kind":"sandbox_run","at":"not-a-time"}}}', 'sandbox.receipt')$q$,
    'evidence:lineage_invalid');
  -- Only the inference gateway records model calls.
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0009', '{"lineage":{"version":1,"activity":{"key":"sbx-c9","kind":"sandbox_run"},"agents":[{"kind":"model","identity":"m","version":"1","role":"r"}],"model_call":{"agent":0,"prompt_sha256":"3333333333333333333333333333333333333333333333333333333333333333","completion_sha256":"4444444444444444444444444444444444444444444444444444444444444444","input_tokens":1,"output_tokens":1,"latency_ms":1}}}', 'sandbox.receipt')$q$,
    'evidence:model_call_reserved');
END $$;
RESET SESSION AUTHORIZATION;
DO $$
DECLARE c1 jsonb := current_setting('qual.c1')::jsonb; e record;
BEGIN
  SELECT ent.content_sha256, ent.size_bytes, ent.media_type, ent.object_uri, ent.object_version, ref.role, ref.class,
      ref.entity_id = ent.entity_id AS named
    INTO e FROM evidence.entities ent JOIN evidence.custody_references ref
      ON ref.tenant_id = ent.tenant_id AND ref.record_id = current_setting('qual.rec6')::uuid
    WHERE ent.tenant_id = 'evidence-site' AND ent.entity_key = 'out:c6';
  IF e.content_sha256 <> c1->>'raw_sha256' OR e.size_bytes <> (c1->>'raw_size')::bigint OR e.media_type <> 'text/plain'
    OR e.object_uri <> 's3://' || current_setting('qual.bucket') || '/' || (c1->>'object_key')
    OR e.object_version <> c1->>'primary_version' OR e.role <> 'entity' OR e.class <> 'run' OR NOT e.named THEN
    RAISE EXCEPTION 'bound entity wrong: %', e;
  END IF;
END $$;
SET SESSION AUTHORIZATION evidence_adapter_qual;

-- The inference gateway: prompt, completion and reasoning stored; tool calls not returned.
DO $$
DECLARE p jsonb; c jsonb; rs jsonb; r jsonb; m record; refs bigint;
  call text := '{"lineage":{"version":1,"activity":{"key":"gw-%s","kind":"model_call"},"agents":[{"kind":"model","identity":"open-model","version":"2026-10","role":"generator"}],"model_call":{"agent":0,"decoding":{"temperature":0.2,"max_tokens":512,"reasoning_effort":"high"},%s,"input_tokens":900,"output_tokens":120,"latency_ms":850,"cache":{"hit":true,"cached_tokens":800}}}}';
BEGIN
  p := pg_temp.claim('gateway/inference-qual', 'gw-claim-0001', 'the prompt');
  c := pg_temp.claim('gateway/inference-qual', 'gw-claim-0002', 'the completion');
  rs := pg_temp.claim('gateway/inference-qual', 'gw-claim-0003', 'the reasoning');
  PERFORM pg_temp.ready('gateway/inference-qual', p);
  PERFORM pg_temp.ready('gateway/inference-qual', c);
  PERFORM pg_temp.ready('gateway/inference-qual', rs);
  r := pg_temp.put('gateway/inference-qual', 'gw-rec-0001', format(call, '1', format(
    '"prompt_claim":"%s","completion_claim":"%s","reasoning_claim":"%s","tool_calls_not_returned":true',
    p->>'claim_id', c->>'claim_id', rs->>'claim_id')), 'model.call');
  PERFORM set_config('qual.gw', jsonb_build_object('record', r->>'record_id', 'prompt', p->>'raw_sha256',
    'completion', c->>'raw_sha256', 'reasoning', rs->>'raw_sha256')::text, false);
  -- Digests the gateway names itself, a role neither stored nor declared not returned, text in decoding, and a claim
  -- beside not_returned are each refused.
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('gateway/inference-qual', 'gw-rec-0002', %L, 'model.call')$q$,
    format(call, '2', '"prompt_sha256":"3333333333333333333333333333333333333333333333333333333333333333","completion_claim":"' || (c->>'claim_id') || '","tool_calls_not_returned":true,"reasoning_not_returned":true')),
    'evidence:lineage_model_call_invalid');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('gateway/inference-qual', 'gw-rec-0003', %L, 'model.call')$q$,
    format(call, '3', format('"prompt_claim":"%s","completion_claim":"%s","tool_calls_not_returned":true', p->>'claim_id', c->>'claim_id'))),
    'evidence:lineage_model_call_invalid');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('gateway/inference-qual', 'gw-rec-0004', %L, 'model.call')$q$,
    replace(format(call, '4', format('"prompt_claim":"%s","completion_claim":"%s","tool_calls_not_returned":true,"reasoning_not_returned":true', p->>'claim_id', c->>'claim_id')),
      '"reasoning_effort":"high"', '"system":"You are a helpful assistant"')),
    'evidence:lineage_not_metadata');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('gateway/inference-qual', 'gw-rec-0005', %L, 'model.call')$q$,
    format(call, '5', format('"prompt_claim":"%s","completion_claim":"%s","reasoning_claim":"%s","reasoning_not_returned":true,"tool_calls_not_returned":true', p->>'claim_id', c->>'claim_id', rs->>'claim_id'))),
    'evidence:lineage_model_call_invalid');
END $$;
RESET SESSION AUTHORIZATION;
DO $$
DECLARE g jsonb := current_setting('qual.gw')::jsonb; m record; refs bigint;
BEGIN
  SELECT mc.prompt_sha256, mc.completion_sha256, mc.reasoning_sha256, mc.tool_calls_sha256 INTO m
    FROM evidence.model_calls mc WHERE mc.tenant_id = 'evidence-site' AND mc.record_id = (g->>'record')::uuid;
  SELECT count(*) INTO refs FROM evidence.custody_references ref
    WHERE ref.tenant_id = 'evidence-site' AND ref.record_id = (g->>'record')::uuid;
  IF m.prompt_sha256 <> g->>'prompt' OR m.completion_sha256 <> g->>'completion'
    OR m.reasoning_sha256 <> g->>'reasoning' OR m.tool_calls_sha256 IS NOT NULL OR refs <> 3 THEN
    RAISE EXCEPTION 'model call custody wrong: % refs %', m, refs;
  END IF;
END $$;
SET SESSION AUTHORIZATION evidence_adapter_qual;

-- A copy found missing makes every claim pinned to it unbindable; a claim cannot become ready on it either.
DO $$
DECLARE c jsonb; d jsonb; replica uuid;
BEGIN
  c := pg_temp.claim('openshell/sandbox-qual', 'cus-key-0010', 'bytes that go missing');
  replica := evidence.custody_record_copy('evidence-site', (c->>'object_id')::uuid, 'replica', c->>'primary_version',
    (c->>'stored_size')::bigint, c->>'stored_sha256', 'GOVERNANCE', clock_timestamp() + interval '91 days', false);
  PERFORM evidence.custody_mark_ready('evidence-site', 'openshell/sandbox-qual', (c->>'claim_id')::uuid, replica);
  PERFORM evidence.custody_observe('evidence-site', (c->>'object_id')::uuid, replica, 'copy_missing',
    '{"reason":"read_back_mismatch"}');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0010', '{"lineage":{"version":1,"activity":{"key":"sbx-c10","kind":"sandbox_run"},"entities":[{"key":"out:c10","kind":"artifact","disclosure":"project","claim":"%s"}],"generated":[{"entity":"out:c10","role":"output"}]}}', 'sandbox.receipt')$q$, c->>'claim_id'),
    'evidence:custody_unverified');
  d := pg_temp.claim('openshell/sandbox-qual', 'cus-key-0011', 'bytes that go missing');
  PERFORM pg_temp.refused(format($q$SELECT evidence.custody_mark_ready('evidence-site', 'openshell/sandbox-qual', %L, %L)$q$, d->>'claim_id', replica),
    'evidence:custody_copy_missing');
  -- A failed re-read recorded against a later verification of the same version invalidates the copy a claim pinned.
  c := pg_temp.claim('openshell/sandbox-qual', 'cus-key-0012', 'bytes re-read later');
  replica := evidence.custody_record_copy('evidence-site', (c->>'object_id')::uuid, 'replica', c->>'primary_version',
    (c->>'stored_size')::bigint, c->>'stored_sha256', 'GOVERNANCE', clock_timestamp() + interval '91 days', false);
  PERFORM evidence.custody_mark_ready('evidence-site', 'openshell/sandbox-qual', (c->>'claim_id')::uuid, replica);
  PERFORM evidence.custody_observe('evidence-site', (c->>'object_id')::uuid,
    evidence.custody_record_copy('evidence-site', (c->>'object_id')::uuid, 'replica', c->>'primary_version',
      (c->>'stored_size')::bigint, c->>'stored_sha256', 'GOVERNANCE', clock_timestamp() + interval '91 days', false),
    'copy_missing', '{"read_back":"failed"}');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0015', '{"lineage":{"version":1,"activity":{"key":"sbx-c15","kind":"sandbox_run"},"entities":[{"key":"out:c15","kind":"artifact","disclosure":"project","claim":"%s"}],"generated":[{"entity":"out:c15","role":"output"}]}}', 'sandbox.receipt')$q$, c->>'claim_id'),
    'evidence:custody_unverified');
  -- The service may not write lifecycle events reserved to garbage collection or acceptance.
  PERFORM pg_temp.refused(format($q$SELECT evidence.custody_observe('evidence-site', %L, NULL, 'delete_intent', '{}')$q$, c->>'object_id'),
    'evidence:custody_event_reserved');
  PERFORM pg_temp.refused(format($q$SELECT evidence.custody_observe('evidence-site', %L, NULL, 'acceptance_completed', '{}')$q$, c->>'object_id'),
    'evidence:custody_event_reserved');
  IF (evidence.custody_epoch_check('evidence-site')->>'epoch') !~ '^[0-9A-F]{8}$' THEN
    RAISE EXCEPTION 'epoch not read';
  END IF;
END $$;
-- Custody rows are immutable even for the service.
DO $$ BEGIN
  PERFORM pg_temp.refused($q$UPDATE evidence.custody_claims SET media_type = 'text/html'$q$, 'permission denied');
END $$;
RESET SESSION AUTHORIZATION;
DO $$ BEGIN
  PERFORM pg_temp.refused($q$UPDATE evidence.custody_claims SET media_type = 'text/html'$q$, 'evidence:append_only');
  PERFORM pg_temp.refused($q$DELETE FROM evidence.custody_references$q$, 'evidence:append_only');
  PERFORM pg_temp.refused($q$TRUNCATE evidence.custody_copies CASCADE$q$, 'evidence:append_only');
  PERFORM pg_temp.refused($q$SELECT evidence.custody_register_store('evidence-site', 'lab-a', 'primary', 'store-other')$q$,
    'evidence:custody_store_conflict');
END $$;

-- An expired claim does not bind (its copy of a ready claim is made here with past times, as the owner).
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
DECLARE c jsonb;
BEGIN
  c := pg_temp.claim('openshell/sandbox-qual', 'cus-key-0020', 'bytes for an old claim');
  PERFORM pg_temp.ready('openshell/sandbox-qual', c);
  PERFORM set_config('qual.old_claim', c->>'claim_id', false);
END $$;
RESET SESSION AUTHORIZATION;
DO $$
DECLARE expired uuid := '00000000-0000-4000-8000-00000000c0c1'; old uuid := current_setting('qual.old_claim')::uuid;
  base timestamptz := clock_timestamp();
BEGIN
  INSERT INTO evidence.custody_claims (tenant_id, claim_id, producer_id, group_key, scope_kind, project_id, run_id,
      object_id, media_type, idempotency_key, request_sha256, primary_copy, class, floor_until, issued_at, expires_at,
      epoch)
    SELECT tenant_id, expired, producer_id, group_key, scope_kind, project_id, run_id, object_id, media_type,
        'cus-key-0021', request_sha256, primary_copy, class, floor_until, base - interval '2 days',
        base - interval '1 day' - interval '1 second', epoch
      FROM evidence.custody_claims WHERE tenant_id = 'evidence-site' AND claim_id = old;
  INSERT INTO evidence.custody_claim_events (tenant_id, claim_id, kind, primary_copy, replica_copy, covered_until, epoch)
    SELECT tenant_id, expired, 'ready', primary_copy, replica_copy, covered_until, epoch FROM evidence.custody_claim_events
      WHERE tenant_id = 'evidence-site' AND claim_id = old;
END $$;
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$ BEGIN
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0020', '{"lineage":{"version":1,"activity":{"key":"sbx-c20","kind":"sandbox_run"},"entities":[{"key":"out:c20","kind":"artifact","disclosure":"project","claim":"00000000-0000-4000-8000-00000000c0c1"}],"generated":[{"entity":"out:c20","role":"output"}]}}', 'sandbox.receipt')$q$,
    'evidence:custody_claim_expired');
END $$;
RESET SESSION AUTHORIZATION;

-- Acceptance: needs an approved records class, asks for every reference's retention, and from then on a claim ready
-- for the run's 90 days no longer binds in the project; one covering the records period does.
INSERT INTO evidence.custody_classes (tenant_id, class_id, group_key, period, owner_id, review_cadence, disposition,
    approved_by, epoch)
  VALUES ('evidence-site', '00000000-0000-4000-8000-00000000d0d1', 'lab-a', interval '10 years',
          '00000000-0000-4000-8000-000000000001', interval '1 year', 'Review with the lab lead, then archive or delete.',
          '00000000-0000-4000-8000-000000000001', evidence.database_epoch());
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
DECLARE before_acceptance jsonb;
BEGIN
  before_acceptance := pg_temp.claim('openshell/sandbox-qual', 'cus-key-0030', 'bytes claimed before acceptance');
  PERFORM pg_temp.ready('openshell/sandbox-qual', before_acceptance);
  PERFORM set_config('qual.before_acceptance', before_acceptance->>'claim_id', false);
END $$;
RESET SESSION AUTHORIZATION;
SET SESSION AUTHORIZATION broker_api_qual;
DO $$
DECLARE requested bigint; refs bigint;
BEGIN
  PERFORM pg_temp.refused($q$SELECT evidence.append('evidence-site', 'broker/api', 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1', NULL, 'acceptance.project', 'acc-key-0001', '2026-10-24T12:00:00Z', '{"decision":"accepted"}', encode(sha256(convert_to('{"decision":"accepted"}', 'UTF8')), 'hex'))$q$,
    'evidence:records_class_missing');
  PERFORM evidence.append('evidence-site', 'broker/api', 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1', NULL,
    'acceptance.project', 'acc-key-0002', '2026-10-24T12:00:00Z',
    '{"records_class":"00000000-0000-4000-8000-00000000d0d1"}',
    encode(sha256(convert_to('{"records_class":"00000000-0000-4000-8000-00000000d0d1"}', 'UTF8')), 'hex'));
  PERFORM pg_temp.refused($q$SELECT evidence.append('evidence-site', 'broker/api', 'lab-a', 'project', '00000000-0000-4000-8000-0000000000a1', NULL, 'acceptance.project', 'acc-key-0003', '2026-10-24T12:00:00Z', '{"records_class":"00000000-0000-4000-8000-00000000d0d1"}', encode(sha256(convert_to('{"records_class":"00000000-0000-4000-8000-00000000d0d1"}', 'UTF8')), 'hex'))$q$,
    'evidence:project_already_accepted');
END $$;
RESET SESSION AUTHORIZATION;
DO $$
DECLARE requested bigint; refs bigint;
BEGIN
  SELECT count(*) INTO refs FROM evidence.custody_references
    WHERE tenant_id = 'evidence-site' AND project_id = '00000000-0000-4000-8000-0000000000a1';
  SELECT count(*) INTO requested FROM evidence.custody_events
    WHERE tenant_id = 'evidence-site' AND kind = 'retention_requested';
  IF refs = 0 OR requested <> refs THEN
    RAISE EXCEPTION 'acceptance asked for % retentions over % references', requested, refs;
  END IF;
  IF evidence.custody_acceptance_state('evidence-site', '00000000-0000-4000-8000-0000000000a1') <> 'pending' THEN
    RAISE EXCEPTION 'an acceptance with retention outstanding is not pending';
  END IF;
END $$;
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
DECLARE r record; first_copy uuid; first_object uuid; n integer;
BEGIN
  -- Nothing is complete while copies are uncovered; a failed attempt moves that copy behind the others.
  IF evidence.custody_complete_acceptances('evidence-site') <> 0 THEN RAISE EXCEPTION 'completed too early'; END IF;
  SELECT copy_id, object_id INTO first_copy, first_object FROM evidence.custody_retention_pending('evidence-site', 500) LIMIT 1;
  PERFORM evidence.custody_observe('evidence-site', first_object, first_copy, 'retention_failed', '{}');
  IF (SELECT copy_id FROM evidence.custody_retention_pending('evidence-site', 500) LIMIT 1) = first_copy THEN
    RAISE EXCEPTION 'a failed copy still heads the retention queue';
  END IF;
  n := 0;
  FOR r IN SELECT * FROM evidence.custody_retention_pending('evidence-site', 500) LOOP
    IF n = 0 THEN
      -- One copy covered for less than asked does not complete it.
      PERFORM evidence.custody_observe('evidence-site', r.object_id, r.copy_id, 'retention_applied',
        jsonb_build_object('retain_until', r.until - interval '1 day'));
    END IF;
    n := n + 1;
  END LOOP;
  IF evidence.custody_complete_acceptances('evidence-site') <> 0 THEN RAISE EXCEPTION 'completed with a copy short'; END IF;
  FOR r IN SELECT * FROM evidence.custody_retention_pending('evidence-site', 500) LOOP
    PERFORM evidence.custody_observe('evidence-site', r.object_id, r.copy_id, 'retention_applied',
      jsonb_build_object('retain_until', r.until + interval '1 day'));
  END LOOP;
  IF evidence.custody_complete_acceptances('evidence-site') <> 1 THEN RAISE EXCEPTION 'acceptance not completed'; END IF;
  IF evidence.custody_complete_acceptances('evidence-site') <> 0 THEN RAISE EXCEPTION 'acceptance completed twice'; END IF;
END $$;
RESET SESSION AUTHORIZATION;
DO $$ BEGIN
  IF evidence.custody_acceptance_state('evidence-site', '00000000-0000-4000-8000-0000000000a1') <> 'complete' THEN
    RAISE EXCEPTION 'the acceptance is not complete after every copy was covered';
  END IF;
END $$;
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
DECLARE long_kept jsonb; r jsonb; cls text;
BEGIN
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'cus-rec-0030', '{"lineage":{"version":1,"activity":{"key":"sbx-c30","kind":"sandbox_run"},"entities":[{"key":"out:c30","kind":"artifact","disclosure":"project","claim":"%s"}],"generated":[{"entity":"out:c30","role":"output"}]}}', 'sandbox.receipt')$q$, current_setting('qual.before_acceptance')),
    'evidence:custody_retention_short');
  long_kept := pg_temp.claim('openshell/sandbox-qual', 'cus-key-0031', 'bytes kept for the records period', 3700);
  IF long_kept->>'class' <> 'records' THEN RAISE EXCEPTION 'claim after acceptance not records: %', long_kept; END IF;
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.ready('openshell/sandbox-qual', %L::jsonb, 91)$q$, long_kept),
    'evidence:custody_retention_short');
  PERFORM pg_temp.ready('openshell/sandbox-qual', long_kept, 3700);
  r := pg_temp.put('openshell/sandbox-qual', 'cus-rec-0031', format('{"lineage":{"version":1,"activity":{"key":"sbx-c31","kind":"sandbox_run"},"entities":[{"key":"out:c31","kind":"artifact","disclosure":"project","claim":"%s"}],"generated":[{"entity":"out:c31","role":"output"}]}}', long_kept->>'claim_id'), 'sandbox.receipt');
  PERFORM set_config('qual.rec31', r->>'record_id', false);
END $$;
RESET SESSION AUTHORIZATION;
DO $$
DECLARE cls text;
BEGIN
  SELECT class INTO cls FROM evidence.custody_references
    WHERE tenant_id = 'evidence-site' AND record_id = current_setting('qual.rec31')::uuid;
  IF cls IS DISTINCT FROM 'records' THEN RAISE EXCEPTION 'reference after acceptance not records: %', cls; END IF;
END $$;

-- The real receipt schema: a receipt of the live shape passes; an unknown member, a sentence in a token field, an
-- unlisted explanation and a missing required member are refused. Registering the same schema again keeps its version.
SELECT evidence.register_event_schema('evidence-site', 'sandbox.receipt', :'receipt_schema'::jsonb);
SELECT set_config('qual.receipt_ref', evidence.register_event_schema('evidence-site', 'sandbox.receipt', :'receipt_schema'::jsonb), false);
\set receipt_sample `cat /workspace/tests/fixtures/sandbox_receipt_sample.json`
SELECT set_config('qual.receipt', :'receipt_sample', false);
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
DECLARE good text := current_setting('qual.receipt'); r jsonb;
BEGIN
  r := pg_temp.put('openshell/sandbox-qual', 'rcpt-key-0001', good, 'sandbox.receipt');
  PERFORM set_config('qual.receipt_record', r->>'record_id', false);
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'rcpt-key-0002', %L, 'sandbox.receipt')$q$,
    jsonb_set(good::jsonb, '{prompt}', '"what the model was asked"')::text), 'evidence:payload_not_metadata');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'rcpt-key-0003', %L, 'sandbox.receipt')$q$,
    jsonb_set(good::jsonb, '{gateway}', '"a reasoning sentence with spaces"')::text), 'evidence:payload_not_metadata');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'rcpt-key-0004', %L, 'sandbox.receipt')$q$,
    jsonb_set(good::jsonb, '{not_recorded,start}', '"any other explanation"')::text), 'evidence:payload_not_metadata');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'rcpt-key-0005', %L, 'sandbox.receipt')$q$,
    (good::jsonb - 'written_at')::text), 'evidence:payload_not_metadata');
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'rcpt-key-0006', %L, 'sandbox.receipt')$q$,
    jsonb_set(good::jsonb, '{pods,0,containers,0,note}', '"text"')::text), 'evidence:payload_not_metadata');
  PERFORM pg_temp.refused($q$SELECT pg_temp.put('openshell/sandbox-qual', 'rcpt-key-0007', '{"x":1}', 'sandbox.unregistered')$q$,
    'evidence:grant_missing');
END $$;
RESET SESSION AUTHORIZATION;
DO $$
BEGIN
  IF (SELECT payload_schema FROM evidence.receipts WHERE tenant_id = 'evidence-site'
      AND record_id = current_setting('qual.receipt_record')::uuid) IS DISTINCT FROM current_setting('qual.receipt_ref') THEN
    RAISE EXCEPTION 'the receipt does not name the schema it was checked against';
  END IF;
  IF current_setting('qual.receipt_ref') !~ '^sandbox\.receipt@2:' THEN
    RAISE EXCEPTION 'the real schema is not version 2 after the open one and its re-registration: %', current_setting('qual.receipt_ref');
  END IF;
END $$;

-- An identical retry is answered with its original receipt even after a newer schema would refuse the payload.
SELECT evidence.register_event_schema('evidence-site', 'sandbox.receipt', '{"type":"object","required":["never_sent"],"properties":{"never_sent":{"type":"boolean"}}}');
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
DECLARE r jsonb;
BEGIN
  r := pg_temp.put('openshell/sandbox-qual', 'rcpt-key-0001', current_setting('qual.receipt'), 'sandbox.receipt');
  IF NOT (r->>'replayed')::boolean OR r->>'record_id' <> current_setting('qual.receipt_record') THEN
    RAISE EXCEPTION 'the retry was not answered with its receipt: %', r;
  END IF;
  PERFORM pg_temp.refused(format($q$SELECT pg_temp.put('openshell/sandbox-qual', 'rcpt-key-0008', %L, 'sandbox.receipt')$q$,
    current_setting('qual.receipt')), 'evidence:payload_not_metadata');
END $$;
RESET SESSION AUTHORIZATION;
SELECT evidence.register_event_schema('evidence-site', 'sandbox.receipt', :'receipt_schema'::jsonb);

-- Another tenant's session sees no custody rows.
SET SESSION AUTHORIZATION evidence_rls_probe;
DO $$ BEGIN
  PERFORM pg_temp.refused($q$SELECT count(*) FROM evidence.custody_claims$q$, 'permission denied');
END $$;
RESET SESSION AUTHORIZATION;
SELECT 'evidence custody qualification passed' AS result;
