-- The lineage of a task on a reviewed-template profile that has no admitted block: the method is
-- still model-written code under that template, and the missing block is a stated gap. Migration
-- 016 had called such a task a configured tool, which no record supports.
BEGIN;

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

  -- What ran decides the class first: admitted code, then a reviewed platform tool's job. A template
  -- profile with neither ran no model-written code, which is a gap, not a configured tool.
  -- `configured` waits for the catalog to declare one.
  tool_class:=CASE WHEN g.task_id IS NOT NULL THEN 'agent_built' WHEN tool IS NOT NULL THEN 'vetted_service'
    WHEN has_template THEN 'agent_built' ELSE 'agent_session' END;
  IF has_template AND g.task_id IS NULL AND tool IS NULL THEN gaps:=gaps||'"code_not_recorded"'::jsonb; END IF;
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
    WHERE n.nspname='enrichedlab_agent' AND p.proname='broker_task_lineage'
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (17) ON CONFLICT DO NOTHING;
COMMIT;
