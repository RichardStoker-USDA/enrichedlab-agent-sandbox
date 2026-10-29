-- Figure version lineage. A version the render service refused, or whose R code failed, drew
-- nothing and has no files by design; its receipt says why. Migration 040's lineage named two gaps for it: an image
-- not pinned to an exact build (a refusal returns before anything runs, so its receipt names no image) and no result
-- files. The lineage below is 040's with those two rules limited to a drawn version. Every other task's lineage is
-- unchanged.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_lineage(p_tenant text, p_actor uuid, p_task uuid)
 RETURNS jsonb
 LANGUAGE plpgsql
 STABLE SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'enrichedlab_agent'
AS $function$
DECLARE t enrichedlab_agent.broker_tasks%ROWTYPE; project uuid; action text;
  o enrichedlab_agent.broker_task_origins%ROWTYPE; g enrichedlab_agent.broker_task_generated_code%ROWTYPE;
  m enrichedlab_agent.broker_task_methods%ROWTYPE;
  step jsonb; step_revision bigint; step_now jsonb; step_now_revision bigint; plan jsonb;
  workspace jsonb; workspace_revision bigint; turn_revision bigint; tool jsonb;
  tool_class text; output_class text; gaps jsonb:='[]'; outputs jsonb; has_template boolean;
  q enrichedlab_agent.broker_task_queries%ROWTYPE; src enrichedlab_agent.broker_sql_sources%ROWTYPE;
  classes jsonb:='[]'; classes_by text;
  fr enrichedlab_agent.broker_figure_requests%ROWTYPE; fx enrichedlab_agent.broker_figure_receipts%ROWTYPE;
  account_name text; account_kind text; unpinned boolean;
BEGIN
  SELECT * INTO t FROM enrichedlab_agent.broker_tasks WHERE tenant_id=p_tenant AND task_id=p_task;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_task_can(p_tenant,p_actor,p_task,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:task_not_found'; END IF;
  SELECT pt.project_id,pt.action_key INTO project,action FROM enrichedlab_agent.broker_project_tasks pt
    WHERE pt.tenant_id=p_tenant AND pt.task_id=p_task;
  SELECT * INTO o FROM enrichedlab_agent.broker_task_origins WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO g FROM enrichedlab_agent.broker_task_generated_code WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO m FROM enrichedlab_agent.broker_task_methods WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO q FROM enrichedlab_agent.broker_task_queries WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO fr FROM enrichedlab_agent.broker_figure_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF fr.task_id IS NOT NULL THEN
    SELECT * INTO fx FROM enrichedlab_agent.broker_figure_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
  END IF;
  IF q.task_id IS NOT NULL THEN
    SELECT * INTO src FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id=p_tenant AND source_key=q.source_key;
  END IF;
  -- The display name only: a task reader may not hold the capability that shows emails.
  SELECT a.display_name,CASE WHEN a.principal_class <> 'person' THEN 'service_account' ELSE 'person' END
    INTO account_name,account_kind FROM enrichedlab_agent.broker_principals a
    WHERE a.tenant_id=p_tenant AND a.principal_id=coalesce(o.principal_id,t.principal_id);
  has_template:=m.template_ref IS NOT NULL;
  SELECT jsonb_build_object('tool_id',j.tool_id,'catalog_digest',j.catalog_digest,'input_sha256',j.input_sha256)
    INTO tool FROM enrichedlab_agent.broker_task_platform_tool_jobs j
    WHERE j.tenant_id=p_tenant AND j.task_id=p_task ORDER BY j.attempt DESC,j.lease_generation DESC LIMIT 1;

  IF o.task_id IS NULL THEN gaps:=gaps||'"origin_not_recorded"'::jsonb;
  ELSIF o.origin='unrecorded' THEN gaps:=gaps||'"origin_before_records"'::jsonb; END IF;
  IF m.task_id IS NULL THEN gaps:=gaps||'"method_not_recorded"'::jsonb;
  ELSIF m.basis='recorded_later' THEN gaps:=gaps||'"method_recorded_later"'::jsonb; END IF;

  IF project IS NULL THEN
    gaps:=gaps||'"not_in_project"'::jsonb;
  ELSIF fr.task_id IS NOT NULL THEN
    NULL;
  ELSE
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
    -- The step as first recorded for this task, and the newest revision that names it.
    SELECT s,c.revision INTO step,step_revision FROM enrichedlab_agent.broker_project_checkpoints c
      CROSS JOIN LATERAL jsonb_array_elements(coalesce(c.state->'plan','[]'::jsonb)) s
      WHERE c.tenant_id=p_tenant AND c.project_id=project AND s->>'task_id'=p_task::text
      ORDER BY c.revision ASC LIMIT 1;
    SELECT s,c.revision INTO step_now,step_now_revision FROM enrichedlab_agent.broker_project_checkpoints c
      CROSS JOIN LATERAL jsonb_array_elements(coalesce(c.state->'plan','[]'::jsonb)) s
      WHERE c.tenant_id=p_tenant AND c.project_id=project AND s->>'task_id'=p_task::text
      ORDER BY c.revision DESC LIMIT 1;
    IF step IS NULL THEN gaps:=gaps||'"plan_step_not_recorded"'::jsonb; END IF;
  END IF;
  -- What kind of data the task read: a query's source as registered, otherwise the classes the input custodian recorded
  -- for the latest attempt's sealed copies. With neither, a task that had inputs names the gap.
  IF fr.task_id IS NOT NULL THEN
    classes:='["prior_output"]'::jsonb; classes_by:='render_request';
  ELSIF src.source_key IS NOT NULL THEN
    classes:=jsonb_build_array(src.data_class); classes_by:='query_source';
  ELSE
    SELECT coalesce(jsonb_agg(c ORDER BY c),'[]'::jsonb) INTO classes FROM (SELECT DISTINCT i.input_class c
      FROM enrichedlab_agent.broker_attempt_inputs i WHERE i.tenant_id=p_tenant AND i.task_id=p_task AND i.attempt=t.attempt) x;
    IF jsonb_array_length(classes)>0 THEN classes_by:='input_custodian'; END IF;
  END IF;
  IF (workspace IS NOT NULL OR tool IS NOT NULL) AND jsonb_array_length(classes)=0 THEN
    gaps:=gaps||'"input_class_not_recorded"'::jsonb; END IF;

  -- What ran decides the class first: a block rendered from a configured method's settings,
  -- admitted model-written code, then a reviewed platform tool's job. A template the task was
  -- submitted under, with none of these, ran no model-written code, which is a gap.
  tool_class:=CASE WHEN fr.task_id IS NOT NULL THEN 'vetted_service' WHEN q.task_id IS NOT NULL THEN 'guarded_query'
    WHEN g.task_id IS NOT NULL AND g.configuration IS NOT NULL THEN 'configured'
    WHEN g.task_id IS NOT NULL THEN 'agent_built' WHEN tool IS NOT NULL THEN 'vetted_service'
    WHEN has_template THEN 'agent_built' ELSE 'agent_session' END;
  IF has_template AND g.task_id IS NULL AND tool IS NULL THEN gaps:=gaps||'"code_not_recorded"'::jsonb; END IF;
  output_class:=CASE tool_class WHEN 'agent_built' THEN 'agent_built_tool_output' WHEN 'vetted_service' THEN 'vetted_tool_output'
    WHEN 'configured' THEN 'configured_tool_output' WHEN 'guarded_query' THEN 'query_output' ELSE 'agent_session_output' END;
  -- A figure version ran in the render service, so the image it names is the one its newest receipt records, and only a
  -- drawn version's receipt must name one (a refusal comes back before anything runs); any other task ran in the image
  -- its profile named.
  unpinned:=CASE WHEN fr.task_id IS NOT NULL THEN fx.status='completed' AND fx.image_digest IS NULL
    ELSE coalesce(t.profile_snapshot->>'image_ref','') !~ '@sha256:[0-9a-f]{64}$' END;
  IF unpinned THEN
    gaps:=gaps||'"image_not_digest_pinned"'::jsonb; END IF;

  SELECT coalesce(jsonb_agg(jsonb_build_object('artifact_id',a.artifact_id,'role',a.role,'label',a.label,
      'relative_path',a.relative_path,'media_type',a.media_type,'size_bytes',a.size_bytes,'sha256',a.sha256,
      'created_at',a.created_at,'provenance_class',output_class) ORDER BY a.created_at,a.artifact_id),'[]'::jsonb)
    INTO outputs FROM enrichedlab_agent.broker_artifacts a
    WHERE a.tenant_id=p_tenant AND a.task_id=p_task AND a.visibility='researcher';
  -- A figure version that was not drawn has no files by design: the service refused its settings or R failed, and its
  -- receipt says so. Only a drawn version without its files is a gap.
  IF jsonb_array_length(outputs)=0 AND (CASE WHEN fr.task_id IS NOT NULL THEN t.status='completed'
      ELSE t.status IN ('completed','failed','cancelled','review_required') END) THEN
    gaps:=gaps||'"no_outputs_recorded"'::jsonb; END IF;

  IF step IS NOT NULL THEN
    plan:=jsonb_build_object('action_key',step->>'action_key','title',step->>'title','rationale',step->>'rationale',
      'status',step_now->>'status','revision',step_revision,
      -- A step first recorded after the checkpoint the task was submitted against was added to the plan later.
      'added_after_submission',workspace_revision IS NOT NULL AND step_revision>workspace_revision,
      'current',CASE WHEN step_now_revision<>step_revision AND (step_now->>'title' IS DISTINCT FROM step->>'title'
          OR step_now->>'rationale' IS DISTINCT FROM step->>'rationale')
        THEN jsonb_build_object('title',step_now->>'title','rationale',step_now->>'rationale','revision',step_now_revision) END);
  END IF;

  RETURN jsonb_build_object(
    'schema','enrichedlab.task-lineage.v1',
    'task',jsonb_build_object('task_id',t.task_id,'project_id',project,'action_key',action,'objective',t.objective,
      'status',t.status,'result_class',t.result_class,'error_code',t.error_code,'attempt',t.attempt,
      'submitted_at',t.submitted_at,'started_at',t.started_at,'finished_at',t.finished_at),
    'origin',CASE WHEN o.task_id IS NULL THEN jsonb_build_object('origin','unrecorded','basis',NULL,'principal_id',t.principal_id)
      ELSE jsonb_build_object('origin',o.origin,'basis',o.basis,'principal_id',o.principal_id,'run_id',o.run_id,
        'turn',o.turn,'recorded_at',o.recorded_at) END
      ||jsonb_build_object('principal_name',account_name,'principal_kind',account_kind),
    'plan_step',plan,
    'inputs',jsonb_build_object('workspace',workspace,'checkpoint_revision',workspace_revision,'platform_tool',tool,
      'classes',classes,'classes_recorded_by',classes_by),
    'method',jsonb_build_object('profile',t.profile_key,'profile_name',m.profile_name,'tool_class',tool_class,
      'template_ref',coalesce(g.template_ref,m.template_ref),'template_sha256',coalesce(g.template_sha256,m.template_sha256),
      'recorded',m.basis,'harness',t.profile_snapshot->'harness',
      'role',t.profile_snapshot->'role','tool_bundle',t.profile_snapshot->'tool_bundle'),
    'code',CASE WHEN g.task_id IS NULL THEN NULL ELSE jsonb_build_object('path',g.path,'sha256',g.sha256,
      'size_bytes',g.size_bytes,'admission',g.admission,'configuration',g.configuration,'run_id',g.run_id,'turn',g.turn,
      'created_at',g.created_at) END,
    'query',CASE WHEN q.task_id IS NULL THEN NULL ELSE jsonb_build_object('source',q.source_key,'sha256',q.sql_sha256,
      'guard',q.guard,'relations',to_jsonb(q.relations),'row_limit',q.row_limit,'run_id',q.run_id,'turn',q.turn,
      'created_at',q.created_at,'title',src.title,'release',src.release,
      'snapshot_sha256',src.snapshot->>'manifest_sha256','data_class',src.data_class) END,
    'render',CASE WHEN fr.task_id IS NULL THEN NULL ELSE jsonb_build_object('source_task',fr.source_task_id,
      'table_path',fr.table_path,'table_sha256',fr.table_sha256,'source_package_sha256',fr.source_package_sha256,
      'feature',fr.feature,'controls',fr.controls,'title',fr.title,'previous',fr.previous_task_id,
      'request_sha256',fr.request_sha256,'created_at',fr.created_at,'service_version',fx.service_version,
      'image_digest',fx.image_digest) END,
    'execution',jsonb_build_object('image_ref',t.profile_snapshot->'image_ref','cpu_limit',t.profile_snapshot->'cpu_limit',
      'memory_limit',t.profile_snapshot->'memory_limit','max_run_seconds',t.profile_snapshot->'max_run_seconds',
      'attempts',t.attempt,'scientific_history',(SELECT count(*) FROM enrichedlab_agent.broker_task_scientific_history h
        WHERE h.tenant_id=p_tenant AND h.task_id=p_task)),
    'outputs',outputs,
    'review',jsonb_build_object('state',CASE WHEN tool_class IN ('agent_built','agent_session','configured','guarded_query') THEN 'unvalidated' ELSE 'not_reviewed' END),
    'gaps',gaps);
END $function$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (41) ON CONFLICT DO NOTHING;
COMMIT;
