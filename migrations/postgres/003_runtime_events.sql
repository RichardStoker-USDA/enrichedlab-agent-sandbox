BEGIN;

-- Runtime facts share the task's append-only sequence and owner-scoped read path.
CREATE UNIQUE INDEX IF NOT EXISTS broker_runtime_event_once
ON enrichedlab_agent.broker_task_events
  (tenant_id, task_id, (metadata->>'lease_generation'), code)
WHERE kind = 'runtime';

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_runtime_event(
  p_tenant text, p_task uuid, p_worker text, p_generation bigint,
  p_code text, p_metadata jsonb
) RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_task enrichedlab_agent.broker_tasks%ROWTYPE;
  v_existing jsonb;
  v_created jsonb;
  v_pod jsonb;
  v_handle jsonb;
  v_keys text[] := ARRAY['schema_version', 'attempt', 'lease_generation'];
  v_key text;
BEGIN
  IF p_code IS NULL OR p_code NOT IN (
    'sandbox_created', 'sandbox_pod_observed',
    'sandbox_terminal_observed', 'sandbox_recovery_needed'
  ) OR p_metadata IS NULL OR jsonb_typeof(p_metadata) <> 'object'
    OR octet_length(p_metadata::text) > 4096 THEN
    RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
  END IF;

  -- Serialize sequence allocation with lifecycle transitions and new claimants.
  SELECT * INTO v_task FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND task_id = p_task
    AND lease_owner = p_worker AND lease_generation = p_generation
    AND status IN ('leased', 'starting', 'running', 'cancelling')
    AND lease_expires_at > clock_timestamp()
  FOR UPDATE;
  IF NOT FOUND OR v_task.lease_expires_at <= clock_timestamp() THEN RETURN false; END IF;

  IF p_metadata->'schema_version' IS DISTINCT FROM '1'::jsonb
    OR p_metadata->'attempt' IS DISTINCT FROM to_jsonb(v_task.attempt)
    OR p_metadata->'lease_generation' IS DISTINCT FROM to_jsonb(p_generation)
    OR p_metadata->>'schema_version' IS DISTINCT FROM '1'
    OR p_metadata->>'attempt' IS DISTINCT FROM v_task.attempt::text
    OR p_metadata->>'lease_generation' IS DISTINCT FROM p_generation::text THEN
    RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
  END IF;
  IF p_code <> 'sandbox_recovery_needed' OR p_metadata ? 'sandbox_uid' THEN
    v_keys := v_keys || ARRAY['namespace', 'sandbox_name', 'sandbox_uid', 'sandbox_generation'];
    FOREACH v_key IN ARRAY ARRAY['namespace', 'sandbox_name', 'sandbox_uid'] LOOP
      IF jsonb_typeof(p_metadata->v_key) IS DISTINCT FROM 'string'
        OR (p_metadata->>v_key) !~ '^[A-Za-z0-9_.-]{1,253}$' THEN
        RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
      END IF;
    END LOOP;
    IF jsonb_typeof(p_metadata->'sandbox_generation') IS DISTINCT FROM 'number'
      OR (p_metadata->>'sandbox_generation') !~ '^[1-9][0-9]{0,18}$' THEN
      RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
    END IF;
    IF (p_metadata->>'sandbox_generation')::numeric > 9223372036854775807 THEN
      RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
    END IF;
  END IF;
  IF p_code IN ('sandbox_pod_observed', 'sandbox_terminal_observed') THEN
    v_keys := v_keys || ARRAY['pod_name', 'pod_uid', 'runtime_class_name'];
    FOREACH v_key IN ARRAY ARRAY['pod_name', 'pod_uid', 'runtime_class_name'] LOOP
      IF jsonb_typeof(p_metadata->v_key) IS DISTINCT FROM 'string'
        OR (p_metadata->>v_key) !~ '^[A-Za-z0-9_.-]{1,253}$' THEN
        RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
      END IF;
    END LOOP;
  END IF;
  IF p_code = 'sandbox_terminal_observed' THEN
    v_keys := v_keys || ARRAY['exit_code'];
    IF jsonb_typeof(p_metadata->'exit_code') IS DISTINCT FROM 'number'
      OR (p_metadata->>'exit_code') !~ '^-?[0-9]{1,10}$' THEN
      RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
    END IF;
    IF (p_metadata->>'exit_code')::numeric NOT BETWEEN -2147483648 AND 2147483647 THEN
      RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
    END IF;
  END IF;
  IF p_code = 'sandbox_recovery_needed' THEN
    v_keys := v_keys || ARRAY['reason'];
    IF jsonb_typeof(p_metadata->'reason') IS DISTINCT FROM 'string'
      OR (p_metadata->>'reason') NOT IN (
        'cancellation_requested', 'shutdown_requested', 'deadline_elapsed',
        'lease_lost', 'lease_check_failed', 'start_failed', 'observation_failed',
        'retainer_preparation_failed', 'finalization_failed', 'runtime_event_failed'
      ) THEN
      RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
    END IF;
  END IF;
  IF NOT (p_metadata ?& v_keys) OR EXISTS (
    SELECT 1 FROM jsonb_object_keys(p_metadata) AS fields(key)
    WHERE NOT (key = ANY(v_keys))
  ) THEN
    RAISE EXCEPTION 'broker:runtime_event_invalid' USING ERRCODE = '22023';
  END IF;

  SELECT metadata INTO v_existing FROM enrichedlab_agent.broker_task_events
  WHERE tenant_id = p_tenant AND task_id = p_task AND kind = 'runtime'
    AND code = p_code AND metadata->>'lease_generation' = p_generation::text;
  IF FOUND THEN
    IF v_existing IS DISTINCT FROM p_metadata THEN
      RAISE EXCEPTION 'broker:runtime_event_conflict' USING ERRCODE = '23505';
    END IF;
    RETURN true;
  END IF;

  SELECT metadata INTO v_created FROM enrichedlab_agent.broker_task_events
  WHERE tenant_id = p_tenant AND task_id = p_task AND kind = 'runtime'
    AND code = 'sandbox_created' AND metadata->>'lease_generation' = p_generation::text;
  v_handle := p_metadata - ARRAY['pod_name', 'pod_uid', 'runtime_class_name', 'exit_code', 'reason'];
  IF p_code IN ('sandbox_pod_observed', 'sandbox_terminal_observed')
    AND v_created IS DISTINCT FROM v_handle THEN
    RAISE EXCEPTION 'broker:runtime_event_conflict' USING ERRCODE = '23505';
  END IF;
  IF p_code = 'sandbox_recovery_needed' AND p_metadata ? 'sandbox_uid'
    AND v_created IS NOT NULL AND v_created IS DISTINCT FROM v_handle THEN
    RAISE EXCEPTION 'broker:runtime_event_conflict' USING ERRCODE = '23505';
  END IF;
  IF p_code = 'sandbox_terminal_observed' THEN
    SELECT metadata INTO v_pod FROM enrichedlab_agent.broker_task_events
    WHERE tenant_id = p_tenant AND task_id = p_task AND kind = 'runtime'
      AND code = 'sandbox_pod_observed' AND metadata->>'lease_generation' = p_generation::text;
    IF v_pod IS DISTINCT FROM (p_metadata - 'exit_code') THEN
      RAISE EXCEPTION 'broker:runtime_event_conflict' USING ERRCODE = '23505';
    END IF;
  END IF;

  UPDATE enrichedlab_agent.broker_tasks
  SET event_sequence = event_sequence + 1, updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND task_id = p_task
  RETURNING * INTO v_task;
  INSERT INTO enrichedlab_agent.broker_task_events
    (tenant_id, task_id, sequence, kind, state, code, metadata)
  VALUES (p_tenant, p_task, v_task.event_sequence, 'runtime', v_task.status, p_code, p_metadata);
  RETURN true;
END;
$$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_runtime_event(text,uuid,text,bigint,text,jsonb)
FROM PUBLIC;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (3) ON CONFLICT DO NOTHING;
COMMIT;
