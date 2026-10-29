-- Where an analysis came from. Who asked for a task is recorded in the transaction that
-- creates it: a person through the API, or a research run's committed turn. The lineage
-- read assembles the broker's own records for one task and names every part they do not
-- hold. Nothing here infers an actor from a record that is missing.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_origins (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  origin text NOT NULL CHECK (origin IN ('person','research_run','unrecorded')),
  -- submission: written with the task. turn_record: an earlier run task, read from the
  -- turn that submitted it. standalone_task: an earlier task outside any project, which
  -- only its principal could submit. before_origin_records: an earlier project task no
  -- turn lists; the account is known, who asked is not.
  basis text NOT NULL CHECK (basis IN ('submission','turn_record','standalone_task','before_origin_records')),
  principal_id uuid NOT NULL,
  run_id uuid,
  turn integer CHECK (turn IS NULL OR turn > 0),
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals,
  FOREIGN KEY (tenant_id, run_id) REFERENCES enrichedlab_agent.broker_research_runs,
  CHECK ((origin = 'research_run') = (run_id IS NOT NULL AND turn IS NOT NULL)),
  CHECK ((origin = 'unrecorded') = (basis = 'before_origin_records')),
  CHECK (basis <> 'turn_record' OR origin = 'research_run'),
  CHECK (basis <> 'standalone_task' OR origin = 'person')
);

DO $$ BEGIN
  ALTER TABLE enrichedlab_agent.broker_task_origins ENABLE ROW LEVEL SECURITY;
  REVOKE ALL ON enrichedlab_agent.broker_task_origins FROM PUBLIC;
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_task_origins;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_task_origins
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
END $$;

-- A research run's tasks: the turn row and its submissions commit together, so the
-- origin is written by the same transaction. A replayed action keeps its first origin.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_turn_task_origins()
RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE actor uuid;
BEGIN
  SELECT r.actor_id INTO actor FROM enrichedlab_agent.broker_research_runs r
    WHERE r.tenant_id=NEW.tenant_id AND r.run_id=NEW.run_id;
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id,task_id,origin,basis,principal_id,run_id,turn)
    SELECT NEW.tenant_id,(s->>'task_id')::uuid,'research_run','submission',actor,NEW.run_id,NEW.turn
    FROM jsonb_array_elements(coalesce(NEW.record->'submitted_tasks','[]'::jsonb)) s
  ON CONFLICT (tenant_id,task_id) DO NOTHING;
  RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS task_origins ON enrichedlab_agent.broker_research_turns;
CREATE TRIGGER task_origins AFTER INSERT ON enrichedlab_agent.broker_research_turns
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_record_turn_task_origins();

-- A person's submissions through the API. The API role executes these instead of the
-- underlying functions, so it cannot create a task without its origin.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_submit_person_task(p_tenant text,p_actor uuid,p_group text,p_profile text,p_objective text,p_key text,p_sha text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE task uuid;
BEGIN
  task:=enrichedlab_agent.broker_submit_task(p_tenant,p_actor,p_group,p_profile,p_objective,p_key,p_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id,task_id,origin,basis,principal_id)
    VALUES(p_tenant,task,'person','submission',p_actor) ON CONFLICT (tenant_id,task_id) DO NOTHING;
  RETURN task;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_submit_person_project_task(p_tenant text,p_actor uuid,p_project uuid,p_action text,p_parent uuid,p_group text,p_profile text,p_objective text,p_key text,p_sha text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE task uuid;
BEGIN
  task:=enrichedlab_agent.broker_submit_project_task(p_tenant,p_actor,p_project,p_action,p_parent,p_group,p_profile,p_objective,p_key,p_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id,task_id,origin,basis,principal_id)
    VALUES(p_tenant,task,'person','submission',p_actor) ON CONFLICT (tenant_id,task_id) DO NOTHING;
  RETURN task;
END $$;

-- Earlier tasks, once: a turn record names its run tasks; a task outside any project had
-- one writer, its principal; any other project task keeps its account and no origin.
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=16) THEN
    INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id,task_id,origin,basis,principal_id,run_id,turn,recorded_at)
      SELECT DISTINCT ON (t.tenant_id,(s->>'task_id')::uuid) t.tenant_id,(s->>'task_id')::uuid,'research_run','turn_record',
             r.actor_id,t.run_id,t.turn,t.created_at
      FROM enrichedlab_agent.broker_research_turns t
      JOIN enrichedlab_agent.broker_research_runs r ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      CROSS JOIN LATERAL jsonb_array_elements(coalesce(t.record->'submitted_tasks','[]'::jsonb)) s
      WHERE EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks k WHERE k.tenant_id=t.tenant_id AND k.task_id=(s->>'task_id')::uuid)
      ORDER BY t.tenant_id,(s->>'task_id')::uuid,t.turn
    ON CONFLICT (tenant_id,task_id) DO NOTHING;
    INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id,task_id,origin,basis,principal_id,recorded_at)
      SELECT k.tenant_id,k.task_id,
             CASE WHEN pt.task_id IS NULL THEN 'person' ELSE 'unrecorded' END,
             CASE WHEN pt.task_id IS NULL THEN 'standalone_task' ELSE 'before_origin_records' END,
             k.principal_id,k.submitted_at
      FROM enrichedlab_agent.broker_tasks k
      LEFT JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id=k.tenant_id AND pt.task_id=k.task_id
      WHERE NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_task_origins o WHERE o.tenant_id=k.tenant_id AND o.task_id=k.task_id)
    ON CONFLICT (tenant_id,task_id) DO NOTHING;
  END IF;
END $$;

-- One task's lineage for a reader of that task: who asked, the plan step, the inputs
-- the project selected, the method and its class, the admitted code, the execution
-- record, the outputs with their classes, the review state, and every gap.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_lineage(p_tenant text,p_actor uuid,p_task uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE t enrichedlab_agent.broker_tasks%ROWTYPE; project uuid; action text;
  o enrichedlab_agent.broker_task_origins%ROWTYPE; g enrichedlab_agent.broker_task_generated_code%ROWTYPE;
  step jsonb; workspace jsonb; workspace_revision bigint; turn_revision bigint; tool jsonb;
  tool_class text; output_class text; gaps jsonb:='[]'; outputs jsonb; profile_name text; has_template boolean;
  account_name text; account_kind text;
BEGIN
  SELECT * INTO t FROM enrichedlab_agent.broker_tasks WHERE tenant_id=p_tenant AND task_id=p_task;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_task_can(p_tenant,p_actor,p_task,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:task_not_found'; END IF;
  SELECT pt.project_id,pt.action_key INTO project,action FROM enrichedlab_agent.broker_project_tasks pt
    WHERE pt.tenant_id=p_tenant AND pt.task_id=p_task;
  SELECT * INTO o FROM enrichedlab_agent.broker_task_origins WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO g FROM enrichedlab_agent.broker_task_generated_code WHERE tenant_id=p_tenant AND task_id=p_task;
  -- The display name only: a task reader may not hold the capability that shows emails.
  SELECT a.display_name,CASE WHEN a.issuer='kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END
    INTO account_name,account_kind FROM enrichedlab_agent.broker_principals a
    WHERE a.tenant_id=p_tenant AND a.principal_id=coalesce(o.principal_id,t.principal_id);
  SELECT p.name INTO profile_name FROM enrichedlab_agent.broker_agent_profiles p WHERE p.tenant_id=p_tenant AND p.profile_key=t.profile_key;
  SELECT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_profile_templates pt WHERE pt.tenant_id=p_tenant AND pt.profile_key=t.profile_key)
    INTO has_template;
  SELECT jsonb_build_object('tool_id',j.tool_id,'catalog_digest',j.catalog_digest,'input_sha256',j.input_sha256)
    INTO tool FROM enrichedlab_agent.broker_task_platform_tool_jobs j
    WHERE j.tenant_id=p_tenant AND j.task_id=p_task ORDER BY j.attempt DESC,j.lease_generation DESC LIMIT 1;

  IF o.task_id IS NULL THEN gaps:=gaps||'"origin_not_recorded"'::jsonb;
  ELSIF o.origin='unrecorded' THEN gaps:=gaps||'"origin_before_records"'::jsonb; END IF;

  IF project IS NULL THEN
    gaps:=gaps||'"not_in_project"'::jsonb;
  ELSE
    SELECT s INTO step FROM enrichedlab_agent.broker_project_checkpoints c
      CROSS JOIN LATERAL jsonb_array_elements(coalesce(c.state->'plan','[]'::jsonb)) s
      WHERE c.tenant_id=p_tenant AND c.project_id=project AND s->>'task_id'=p_task::text
      ORDER BY c.revision DESC LIMIT 1;
    IF step IS NULL THEN gaps:=gaps||'"plan_step_not_recorded"'::jsonb; END IF;
    IF o.origin='research_run' THEN
      SELECT u.checkpoint_revision INTO turn_revision FROM enrichedlab_agent.broker_research_turns u
        WHERE u.tenant_id=p_tenant AND u.run_id=o.run_id AND u.turn=o.turn;
    END IF;
    SELECT c.state->'workspace',c.revision INTO workspace,workspace_revision FROM enrichedlab_agent.broker_project_checkpoints c
      WHERE c.tenant_id=p_tenant AND c.project_id=project
        AND (CASE WHEN turn_revision IS NOT NULL THEN c.revision=turn_revision ELSE c.created_at<=t.submitted_at END)
      ORDER BY c.revision DESC LIMIT 1;
    IF workspace IS NULL OR jsonb_typeof(workspace)<>'object' THEN
      workspace:=NULL; gaps:=gaps||'"inputs_not_recorded"'::jsonb;
    END IF;
  END IF;
  IF workspace IS NOT NULL OR tool IS NOT NULL THEN gaps:=gaps||'"input_class_not_recorded"'::jsonb; END IF;

  tool_class:=CASE WHEN g.task_id IS NOT NULL THEN 'agent_built' WHEN tool IS NOT NULL THEN 'vetted_service'
    WHEN has_template THEN 'configured' ELSE 'agent_session' END;
  output_class:=CASE tool_class WHEN 'agent_built' THEN 'agent_built_tool_output' WHEN 'vetted_service' THEN 'vetted_tool_output'
    WHEN 'configured' THEN 'configured_tool_output' ELSE 'agent_session_output' END;
  IF coalesce(t.profile_snapshot->>'image_ref','') !~ '@sha256:[0-9a-f]{64}$' THEN
    gaps:=gaps||'"image_not_digest_pinned"'::jsonb; END IF;

  SELECT coalesce(jsonb_agg(jsonb_build_object('artifact_id',a.artifact_id,'role',a.role,'label',a.label,
      'relative_path',a.relative_path,'media_type',a.media_type,'size_bytes',a.size_bytes,'sha256',a.sha256,
      'created_at',a.created_at,'provenance_class',output_class) ORDER BY a.created_at,a.artifact_id),'[]'::jsonb)
    INTO outputs FROM enrichedlab_agent.broker_artifacts a
    WHERE a.tenant_id=p_tenant AND a.task_id=p_task AND a.visibility='researcher';
  IF jsonb_array_length(outputs)=0 AND t.status IN ('completed','failed','cancelled','review_required') THEN
    gaps:=gaps||'"no_outputs_recorded"'::jsonb; END IF;

  RETURN jsonb_build_object(
    'schema','enrichedlab.task-lineage.v1',
    'task',jsonb_build_object('task_id',t.task_id,'project_id',project,'action_key',action,'objective',t.objective,
      'status',t.status,'result_class',t.result_class,'error_code',t.error_code,'attempt',t.attempt,
      'submitted_at',t.submitted_at,'started_at',t.started_at,'finished_at',t.finished_at),
    'origin',CASE WHEN o.task_id IS NULL THEN jsonb_build_object('origin','unrecorded','basis',NULL,'principal_id',t.principal_id)
      ELSE jsonb_build_object('origin',o.origin,'basis',o.basis,'principal_id',o.principal_id,'run_id',o.run_id,
        'turn',o.turn,'recorded_at',o.recorded_at) END
      ||jsonb_build_object('principal_name',account_name,'principal_kind',account_kind),
    'plan_step',CASE WHEN step IS NULL THEN NULL ELSE jsonb_build_object('action_key',step->>'action_key',
      'title',step->>'title','status',step->>'status','rationale',step->>'rationale') END,
    'inputs',jsonb_build_object('workspace',workspace,'checkpoint_revision',workspace_revision,'platform_tool',tool),
    'method',jsonb_build_object('profile',t.profile_key,'profile_name',profile_name,'tool_class',tool_class,
      'template_ref',g.template_ref,'template_sha256',g.template_sha256,'harness',t.profile_snapshot->'harness',
      'role',t.profile_snapshot->'role','tool_bundle',t.profile_snapshot->'tool_bundle'),
    'code',CASE WHEN g.task_id IS NULL THEN NULL ELSE jsonb_build_object('path',g.path,'sha256',g.sha256,
      'size_bytes',g.size_bytes,'admission',g.admission,'run_id',g.run_id,'turn',g.turn,'created_at',g.created_at) END,
    'execution',jsonb_build_object('image_ref',t.profile_snapshot->'image_ref','cpu_limit',t.profile_snapshot->'cpu_limit',
      'memory_limit',t.profile_snapshot->'memory_limit','max_run_seconds',t.profile_snapshot->'max_run_seconds',
      'attempts',t.attempt,'scientific_history',(SELECT count(*) FROM enrichedlab_agent.broker_task_scientific_history h
        WHERE h.tenant_id=p_tenant AND h.task_id=p_task)),
    'outputs',outputs,
    'review',jsonb_build_object('state',CASE WHEN tool_class IN ('agent_built','agent_session') THEN 'unvalidated' ELSE 'not_reviewed' END),
    'gaps',gaps);
END $$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE n.nspname='enrichedlab_agent' AND p.proname IN ('broker_record_turn_task_origins','broker_submit_person_task',
      'broker_submit_person_project_task','broker_task_lineage')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (16) ON CONFLICT DO NOTHING;
COMMIT;
