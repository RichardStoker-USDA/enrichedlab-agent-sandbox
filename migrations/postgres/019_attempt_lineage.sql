-- Per-attempt lineage (step 1). A task's lineage said which inputs its workspace
-- declared and which outputs the task holds now, not what each attempt consumed, ran or produced,
-- and a retry that registered an output path an earlier attempt had used replaced that attempt's
-- recorded digest in place. Output registration now also records, immutably, the attempt and lease
-- that registered each digest. The inputs an attempt's custodian sealed and the execution its worker
-- bound get lease-fenced writers here; their callers ship with the worker and custodian releases.
-- The per-attempt read names every part that was not recorded instead of filling it from the task.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_attempt_outputs (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  relative_path text NOT NULL,
  sha256 text NOT NULL,
  artifact_id uuid NOT NULL,
  role text NOT NULL,
  size_bytes bigint NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  -- Every distinct digest an attempt registered for a path is kept, not only the last.
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation, relative_path, sha256),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, artifact_id) REFERENCES enrichedlab_agent.broker_artifacts (tenant_id, artifact_id),
  CHECK (attempt >= 0),
  CHECK (lease_generation > 0),
  CHECK (sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (size_bytes >= 0)
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_attempt_inputs (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  input_ref text NOT NULL,
  -- Where the custodian resolved the input from, never inferred from its name.
  input_class text NOT NULL,
  sealed_sha256 text NOT NULL,
  size_bytes bigint NOT NULL,
  custody_transaction text NOT NULL,
  source jsonb NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation, input_ref),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  CHECK (attempt >= 0),
  CHECK (lease_generation > 0),
  CHECK (input_ref ~ '^[a-z][a-z0-9+.-]*://[^[:space:]]+$' AND length(input_ref) <= 512),
  CHECK (input_class IN ('lab_database', 'object_data', 'paper', 'public_reference', 'prior_output')),
  CHECK (sealed_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (size_bytes >= 0),
  CHECK (custody_transaction ~ '^[A-Za-z0-9._:-]{1,128}$'),
  CHECK (jsonb_typeof(source) = 'object' AND length(source::text) <= 4096),
  -- A database extract names its query and the snapshot it read; an object names its source version.
  CHECK (input_class <> 'lab_database' OR (source ? 'query_sha256' AND source ? 'snapshot' AND source ? 'wal_position')),
  CHECK (input_class <> 'object_data' OR (source ? 'source_version' AND source ? 'source_sha256'))
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_attempt_executions (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  image_digest text NOT NULL,
  template_ref text,
  template_sha256 text,
  tool_ref text,
  parameters_sha256 text NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  CHECK (attempt >= 0),
  CHECK (lease_generation > 0),
  CHECK (image_digest ~ '^sha256:[0-9a-f]{64}$'),
  CHECK ((template_ref IS NULL) = (template_sha256 IS NULL)),
  CHECK (template_sha256 IS NULL OR template_sha256 ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (tool_ref IS NULL OR length(tool_ref) BETWEEN 1 AND 256),
  CHECK (parameters_sha256 ~ '^sha256:[0-9a-f]{64}$')
);

DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_attempt_outputs','broker_attempt_inputs','broker_attempt_executions'] LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON enrichedlab_agent.%I FROM PUBLIC', t);
    EXECUTE format('DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.%I
      FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation()', t);
  END LOOP;
END $$;

-- Same signature, grant and fence as before. The task row is held for the registration so the
-- attempt recorded is the one this lease belongs to.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_register_artifact(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint,
  p_visibility text,
  p_role text,
  p_label text,
  p_relative_path text,
  p_media_type text,
  p_size_bytes bigint,
  p_sha256 text
)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_artifact uuid;
  v_attempt integer;
BEGIN
  SELECT attempt INTO v_attempt FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant
    AND task_id = p_task
    AND lease_owner = p_worker
    AND lease_generation = p_generation
  FOR SHARE;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '40001', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  INSERT INTO enrichedlab_agent.broker_artifacts (
    tenant_id, task_id, visibility, role, label, relative_path, media_type, size_bytes, sha256
  ) VALUES (
    p_tenant, p_task, p_visibility, p_role, p_label, p_relative_path, p_media_type, p_size_bytes, p_sha256
  )
  ON CONFLICT (tenant_id, task_id, relative_path)
  DO UPDATE SET visibility = EXCLUDED.visibility, role = EXCLUDED.role, label = EXCLUDED.label,
                media_type = EXCLUDED.media_type, size_bytes = EXCLUDED.size_bytes,
                sha256 = EXCLUDED.sha256
  RETURNING artifact_id INTO v_artifact;
  INSERT INTO enrichedlab_agent.broker_attempt_outputs (
    tenant_id, task_id, attempt, lease_generation, relative_path, sha256, artifact_id, role, size_bytes
  ) VALUES (
    p_tenant, p_task, v_attempt, p_generation, p_relative_path, p_sha256, v_artifact, p_role, p_size_bytes
  )
  ON CONFLICT DO NOTHING;
  RETURN v_artifact;
END;
$$;

-- The worker records the image, template or tool and parameters it bound for its own lease, once.
-- A second call with the same values is accepted; different values are refused.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_attempt_execution(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint,
  p_image_digest text,
  p_template_ref text,
  p_template_sha256 text,
  p_tool_ref text,
  p_parameters_sha256 text
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_attempt integer;
  e enrichedlab_agent.broker_attempt_executions%ROWTYPE;
BEGIN
  SELECT attempt INTO v_attempt FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND task_id = p_task AND lease_owner = p_worker AND lease_generation = p_generation
    AND status IN ('leased', 'starting', 'running') AND desired_status = 'run'
  FOR SHARE;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '40001', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  INSERT INTO enrichedlab_agent.broker_attempt_executions (
    tenant_id, task_id, attempt, lease_generation, image_digest, template_ref, template_sha256, tool_ref, parameters_sha256
  ) VALUES (
    p_tenant, p_task, v_attempt, p_generation, p_image_digest, p_template_ref, p_template_sha256, p_tool_ref, p_parameters_sha256
  )
  ON CONFLICT DO NOTHING;
  SELECT * INTO e FROM enrichedlab_agent.broker_attempt_executions
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = v_attempt AND lease_generation = p_generation;
  IF (e.image_digest, e.template_ref, e.template_sha256, e.tool_ref, e.parameters_sha256)
     IS DISTINCT FROM (p_image_digest, p_template_ref, p_template_sha256, p_tool_ref, p_parameters_sha256) THEN
    RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:attempt_execution_conflict';
  END IF;
END;
$$;

-- The trusted input custodian records each copy it sealed for the attempt it is preparing, before
-- compute is admitted. The same rules as the execution record apply.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_attempt_input(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_attempt integer,
  p_generation bigint,
  p_input_ref text,
  p_input_class text,
  p_sealed_sha256 text,
  p_size_bytes bigint,
  p_custody_transaction text,
  p_source jsonb
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  i enrichedlab_agent.broker_attempt_inputs%ROWTYPE;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND task_id = p_task AND lease_owner = p_worker AND attempt = p_attempt
    AND lease_generation = p_generation AND status IN ('leased', 'starting', 'running') AND desired_status = 'run'
    AND lease_expires_at > clock_timestamp()
  FOR SHARE;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '40001', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  INSERT INTO enrichedlab_agent.broker_attempt_inputs (
    tenant_id, task_id, attempt, lease_generation, input_ref, input_class, sealed_sha256, size_bytes,
    custody_transaction, source
  ) VALUES (
    p_tenant, p_task, p_attempt, p_generation, p_input_ref, p_input_class, p_sealed_sha256, p_size_bytes,
    p_custody_transaction, p_source
  )
  ON CONFLICT DO NOTHING;
  SELECT * INTO i FROM enrichedlab_agent.broker_attempt_inputs
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = p_attempt AND lease_generation = p_generation
    AND input_ref = p_input_ref;
  IF (i.input_class, i.sealed_sha256, i.size_bytes, i.custody_transaction, i.source)
     IS DISTINCT FROM (p_input_class, p_sealed_sha256, p_size_bytes, p_custody_transaction, p_source) THEN
    RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:attempt_input_conflict';
  END IF;
END;
$$;

-- Version 2 of a task's lineage: version 1 for the same reader, plus one entry per attempt with
-- what that attempt's custodian sealed, what its worker bound and which digests it registered.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_lineage_v2(p_tenant text, p_actor uuid, p_task uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE
  base jsonb; t enrichedlab_agent.broker_tasks%ROWTYPE; attempts jsonb:='[]'; a integer;
  inputs jsonb; execution jsonb; outputs jsonb; gaps jsonb; declared boolean; has_outputs boolean;
  classed boolean:=false; top_gaps jsonb;
BEGIN
  -- Version 1 decides who may read the task and raises for anyone else.
  base:=enrichedlab_agent.broker_task_lineage(p_tenant,p_actor,p_task);
  SELECT * INTO t FROM enrichedlab_agent.broker_tasks WHERE tenant_id=p_tenant AND task_id=p_task;
  declared:=jsonb_array_length(coalesce(base->'inputs'->'workspace'->'input_refs','[]'::jsonb))>0;
  has_outputs:=jsonb_array_length(coalesce(base->'outputs','[]'::jsonb))>0;
  FOR a IN 1..greatest(t.attempt,0) LOOP
    SELECT coalesce(jsonb_agg(jsonb_build_object('input_ref',i.input_ref,'class',i.input_class,
        'sha256',i.sealed_sha256,'size_bytes',i.size_bytes,'custody_transaction',i.custody_transaction,
        'source',i.source,'lease_generation',i.lease_generation,'recorded_at',i.recorded_at)
        ORDER BY i.lease_generation,i.input_ref),'[]'::jsonb)
      INTO inputs FROM enrichedlab_agent.broker_attempt_inputs i
      WHERE i.tenant_id=p_tenant AND i.task_id=p_task AND i.attempt=a;
    SELECT jsonb_build_object('image_digest',e.image_digest,'template_ref',e.template_ref,
        'template_sha256',e.template_sha256,'tool_ref',e.tool_ref,'parameters_sha256',e.parameters_sha256,
        'lease_generation',e.lease_generation,'recorded_at',e.recorded_at)
      INTO execution FROM enrichedlab_agent.broker_attempt_executions e
      WHERE e.tenant_id=p_tenant AND e.task_id=p_task AND e.attempt=a
      ORDER BY e.lease_generation DESC LIMIT 1;
    -- `current` is false when a later registration replaced this digest in the task's artifact record.
    SELECT coalesce(jsonb_agg(jsonb_build_object('artifact_id',o.artifact_id,'relative_path',o.relative_path,
        'role',o.role,'sha256',o.sha256,'size_bytes',o.size_bytes,'lease_generation',o.lease_generation,
        'recorded_at',o.recorded_at,'current',ar.sha256=o.sha256) ORDER BY o.recorded_at,o.relative_path),'[]'::jsonb)
      INTO outputs FROM enrichedlab_agent.broker_attempt_outputs o
      JOIN enrichedlab_agent.broker_artifacts ar ON ar.tenant_id=o.tenant_id AND ar.artifact_id=o.artifact_id
      WHERE o.tenant_id=p_tenant AND o.task_id=p_task AND o.attempt=a
        AND ar.visibility='researcher';
    gaps:='[]';
    IF declared AND jsonb_array_length(inputs)=0 THEN gaps:=gaps||'"attempt_inputs_not_recorded"'::jsonb; END IF;
    IF execution IS NULL THEN gaps:=gaps||'"attempt_execution_not_recorded"'::jsonb; END IF;
    IF a=t.attempt AND has_outputs AND jsonb_array_length(outputs)=0 THEN
      gaps:=gaps||'"attempt_outputs_not_linked"'::jsonb; END IF;
    IF a=t.attempt AND jsonb_array_length(inputs)>0 THEN classed:=true; END IF;
    attempts:=attempts||jsonb_build_array(jsonb_build_object('attempt',a,'inputs',inputs,'execution',execution,
      'outputs',outputs,'validation',jsonb_build_object('state',base->'review'->>'state'),'gaps',gaps));
  END LOOP;
  -- An input class is known only when the latest attempt's custodian recorded its inputs.
  top_gaps:=coalesce(base->'gaps','[]'::jsonb);
  IF classed THEN
    SELECT coalesce(jsonb_agg(g),'[]'::jsonb) INTO top_gaps FROM jsonb_array_elements(top_gaps) g
      WHERE g<>'"input_class_not_recorded"'::jsonb;
  END IF;
  RETURN base||jsonb_build_object('schema','enrichedlab.task-lineage.v2','attempts',attempts,'gaps',top_gaps);
END $$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE n.nspname='enrichedlab_agent' AND p.proname IN ('broker_register_artifact','broker_record_attempt_execution',
      'broker_record_attempt_input','broker_task_lineage_v2')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (19) ON CONFLICT DO NOTHING;
COMMIT;
