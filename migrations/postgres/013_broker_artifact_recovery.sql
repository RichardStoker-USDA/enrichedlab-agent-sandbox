-- Operator recovery registers verified retained bytes without reviving a lease.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_artifact_recoveries (
  tenant_id text NOT NULL,
  recovery_id uuid NOT NULL,
  task_id uuid NOT NULL,
  request jsonb NOT NULL,
  receipt jsonb NOT NULL,
  actor text NOT NULL DEFAULT session_user,
  recovered_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, recovery_id),
  FOREIGN KEY (tenant_id, task_id)
    REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id)
);
ALTER TABLE enrichedlab_agent.broker_artifact_recoveries ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_artifact_recoveries FROM PUBLIC;
DROP TRIGGER IF EXISTS broker_artifact_recoveries_immutable ON enrichedlab_agent.broker_artifact_recoveries;
CREATE TRIGGER broker_artifact_recoveries_immutable BEFORE UPDATE OR DELETE
  ON enrichedlab_agent.broker_artifact_recoveries FOR EACH ROW
  EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_recover_artifacts(
  p_tenant text, p_recovery uuid, p_operator_ref text, p_generation bigint,
  p_terminal_text text, p_custody jsonb, p_observed jsonb
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY INVOKER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_terminal jsonb := p_terminal_text::jsonb;
  v_task enrichedlab_agent.broker_tasks%ROWTYPE;
  v_request jsonb;
  v_prior enrichedlab_agent.broker_artifact_recoveries%ROWTYPE;
  v_hash text := encode(sha256(convert_to(p_terminal_text, 'UTF8')), 'hex');
  v_file jsonb;
  v_id uuid;
  v_path text;
  v_artifacts jsonb := '[]'::jsonb;
  v_receipt jsonb;
BEGIN
  IF p_recovery IS NULL OR p_generation IS NULL OR p_operator_ref IS NULL
     OR length(p_operator_ref) NOT BETWEEN 1 AND 256
     OR octet_length(p_terminal_text) > 1048576
     OR v_terminal->>'schemaVersion' IS DISTINCT FROM 'sandbox.enrichedlab.org/terminal-export/v1'
     OR p_custody->>'schemaVersion' IS DISTINCT FROM 'sandbox.enrichedlab.org/output-custodian-receipt/v1'
     OR p_custody->>'terminalSha256' IS DISTINCT FROM v_hash
     OR p_observed->>'terminalSha256' IS DISTINCT FROM v_hash
     OR coalesce(length(p_observed->>'pvcUid'), 0) NOT BETWEEN 1 AND 128
     OR coalesce(length(p_observed->>'exporterPodUid'), 0) NOT BETWEEN 1 AND 128
     OR jsonb_typeof(v_terminal->'files') IS DISTINCT FROM 'array'
     OR v_terminal->'files' IS DISTINCT FROM p_observed->'files'
  THEN RAISE EXCEPTION USING ERRCODE='22023', MESSAGE='broker:invalid_recovery_evidence'; END IF;
  IF jsonb_array_length(v_terminal->'files') NOT BETWEEN 1 AND 64
     OR (p_custody->>'fileCount')::integer IS DISTINCT FROM jsonb_array_length(v_terminal->'files')
     OR (p_custody->>'sizeBytes')::bigint IS DISTINCT FROM (
       SELECT sum((f->>'sizeBytes')::bigint) FROM jsonb_array_elements(v_terminal->'files') f)
     OR EXISTS (SELECT 1 FROM jsonb_array_elements(v_terminal->'files') f
       GROUP BY f->>'relativePath' HAVING count(*) > 1)
  THEN RAISE EXCEPTION USING ERRCODE='22023', MESSAGE='broker:invalid_recovery_files'; END IF;

  SELECT * INTO STRICT v_task FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id=p_tenant AND task_id=(v_terminal->>'taskId')::uuid FOR UPDATE;
  v_request := jsonb_build_object('operator_ref',p_operator_ref,'generation',p_generation,
    'terminal_sha256',v_hash,'custody',p_custody,'observed',p_observed);
  SELECT * INTO v_prior FROM enrichedlab_agent.broker_artifact_recoveries
    WHERE tenant_id=p_tenant AND recovery_id=p_recovery;
  IF FOUND THEN
    IF v_prior.task_id <> v_task.task_id OR v_prior.request <> v_request THEN
      RAISE EXCEPTION USING ERRCODE='23505', MESSAGE='broker:recovery_replay_changed';
    END IF;
    RETURN v_prior.receipt;
  END IF;
  IF v_task.status NOT IN ('failed','cancelled') OR v_task.lease_owner IS NOT NULL
     OR v_task.lease_generation <> p_generation
     OR v_terminal->>'principalId' IS DISTINCT FROM v_task.principal_id::text
     OR v_terminal->>'groupKey' IS DISTINCT FROM v_task.group_key
     OR v_terminal->>'profileKey' IS DISTINCT FROM v_task.profile_key
     OR (v_terminal->>'attempt')::integer NOT BETWEEN 1 AND v_task.attempt
     OR (v_terminal->>'leaseGeneration')::bigint NOT BETWEEN 1 AND v_task.lease_generation
     OR p_custody->'taskId' IS DISTINCT FROM v_terminal->'taskId'
     OR p_custody->'attempt' IS DISTINCT FROM v_terminal->'attempt'
     OR p_custody->'leaseGeneration' IS DISTINCT FROM v_terminal->'leaseGeneration'
     OR NOT EXISTS (
       SELECT 1 FROM enrichedlab_agent.broker_task_events e
       WHERE e.tenant_id=p_tenant AND e.task_id=v_task.task_id
         AND e.kind='runtime' AND e.code='sandbox_terminal_observed'
         AND e.metadata @> jsonb_build_object(
           'attempt',v_terminal->'attempt','lease_generation',v_terminal->'leaseGeneration',
           'namespace',v_terminal->'namespace','sandbox_name',v_terminal->'sandboxName',
           'sandbox_uid',v_terminal->'sandboxUid','sandbox_generation',v_terminal->'sandboxGeneration',
           'pod_name',v_terminal->'podName','pod_uid',v_terminal->'podUid',
           'runtime_class_name',v_terminal->'runtimeClassName','exit_code',v_terminal->'exitCode'))
  THEN RAISE EXCEPTION USING ERRCODE='22023', MESSAGE='broker:recovery_scope_mismatch'; END IF;

  FOR v_file IN SELECT value FROM jsonb_array_elements(v_terminal->'files') LOOP
    IF coalesce(v_file->>'relativePath','') !~ '^[A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)*$'
       OR v_file->>'relativePath' ~ '(^|/)\.{1,2}(/|$)'
    THEN RAISE EXCEPTION USING ERRCODE='22023', MESSAGE='broker:invalid_recovery_path'; END IF;
    v_path := 'tasks/' || v_task.task_id || '/recovered/' || p_recovery || '/' || (v_file->>'relativePath');
    -- Create only. A conflict rolls back the whole recovery; no artifact is overwritten.
    INSERT INTO enrichedlab_agent.broker_artifacts (
      tenant_id,task_id,visibility,role,label,relative_path,media_type,size_bytes,sha256
    ) VALUES (p_tenant,v_task.task_id,v_file->>'visibility',v_file->>'role',
      v_file->>'label',v_path,v_file->>'mediaType',(v_file->>'sizeBytes')::bigint,v_file->>'sha256')
      RETURNING artifact_id INTO v_id;
    v_artifacts := v_artifacts || jsonb_build_array(jsonb_build_object(
      'artifact_id',v_id,'relative_path',v_path,'sha256',v_file->>'sha256','size_bytes',v_file->'sizeBytes'));
  END LOOP;
  v_receipt := jsonb_build_object('recovery_id',p_recovery,'task_id',v_task.task_id,
    'original_status',v_task.status,'attempt',v_terminal->'attempt',
    'terminal_sha256',v_hash,'operator_ref',p_operator_ref,'artifacts',v_artifacts);
  INSERT INTO enrichedlab_agent.broker_artifact_recoveries(tenant_id,recovery_id,task_id,request,receipt)
    VALUES(p_tenant,p_recovery,v_task.task_id,v_request,v_receipt);
  UPDATE enrichedlab_agent.broker_tasks SET event_sequence=event_sequence+1,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND task_id=v_task.task_id RETURNING * INTO v_task;
  INSERT INTO enrichedlab_agent.broker_task_events(tenant_id,task_id,sequence,kind,state,code,metadata)
    VALUES(p_tenant,v_task.task_id,v_task.event_sequence,'artifact_recovery',v_task.status,
      'recovered_artifacts',v_receipt);
  RETURN v_receipt;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_recover_artifacts(text,uuid,text,bigint,text,jsonb,jsonb) FROM PUBLIC;
-- Deliberately absent from every runtime role grant. Operator/schema owner only.
COMMIT;
