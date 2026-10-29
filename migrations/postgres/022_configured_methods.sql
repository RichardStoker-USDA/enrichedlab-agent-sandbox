-- Configured methods. A configured method's template contract carries a block template
-- with slots and named settings, each a closed list of choices. The research lead picks settings and
-- writes no code; the coordinator renders and admits the block. The turn commit renders it again here
-- from the template row and the chosen settings and refuses any difference, so a configured task can
-- only hold the rendered recipe. Lineage classes such a task `configured`. Additive: older images keep
-- working, and a code template's commit is unchanged except that it now refuses a configuration.
BEGIN;

ALTER TABLE enrichedlab_agent.broker_task_generated_code
  ADD COLUMN IF NOT EXISTS configuration jsonb
  CHECK (configuration IS NULL OR (jsonb_typeof(configuration) = 'object' AND octet_length(configuration::text) <= 4096));

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_commit_research_turn(p_tenant text,p_run uuid,p_worker text,p_generation bigint,p_expected bigint,p_state jsonb,p_actions jsonb,p_record jsonb,p_outcome text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project enrichedlab_agent.broker_projects%ROWTYPE;
  tmpl enrichedlab_agent.broker_profile_templates%ROWTYPE;
  a jsonb; code jsonb; task uuid; tasks jsonb:='[]'; plan jsonb; revision bigint; pending integer;
  cfg jsonb; chosen jsonb; rendered text; pick record; fill record; described jsonb;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run FOR UPDATE;
  IF NOT FOUND OR r.status<>'running' OR r.lease_owner IS DISTINCT FROM p_worker OR r.generation<>p_generation OR r.lease_until<=clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  SELECT * INTO project FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=r.project_id FOR UPDATE;
  IF NOT enrichedlab_agent.broker_research_authorized(p_tenant,r.actor_id,r.project_id,r.allowed_profiles) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden'; END IF;
  IF project.revision<>p_expected THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  IF r.turn>=r.max_turns OR p_outcome NOT IN ('delegate','review_required','paused')
    OR jsonb_typeof(p_actions) IS DISTINCT FROM 'array' OR jsonb_array_length(p_actions)>r.max_parallel
    OR (p_outcome='delegate') IS DISTINCT FROM (jsonb_array_length(p_actions)>0)
    OR jsonb_typeof(p_record) IS DISTINCT FROM 'object' OR octet_length(p_record::text)>131072 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  SELECT count(*) INTO pending FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND t.status IN ('queued','leased','starting','running','cancelling');
  IF pending+jsonb_array_length(p_actions)>r.max_parallel THEN RAISE EXCEPTION USING ERRCODE='53300',MESSAGE='broker:group_queue_full'; END IF;
  FOR a IN SELECT value FROM jsonb_array_elements(p_actions) LOOP
    IF (a->>'profile'=ANY(r.allowed_profiles)) IS NOT TRUE
      OR (a->>'action_key' ~ '^[a-zA-Z0-9_-]{1,100}$') IS NOT TRUE
      OR (a->>'request_sha256' ~ '^[0-9a-f]{64}$') IS NOT TRUE OR NOT EXISTS (
        SELECT 1 FROM jsonb_array_elements(p_state->'plan') s WHERE s->>'action_key'=a->>'action_key' AND s->>'status'='pending' AND s->>'task_id' IS NULL
      ) OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=r.project_id AND action_key=a->>'action_key') THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
    code:=a->'generated_code'; cfg:=NULL; described:=NULL;
    SELECT * INTO tmpl FROM enrichedlab_agent.broker_profile_templates WHERE tenant_id=p_tenant AND profile_key=a->>'profile';
    IF FOUND THEN
      -- Every comparison is NULL-safe: a missing field must refuse, not pass.
      IF jsonb_typeof(code) IS DISTINCT FROM 'object'
        OR EXISTS (SELECT 1 FROM jsonb_object_keys(code) k WHERE k NOT IN ('template_ref','path','content','sha256','admission','configuration'))
        OR (code->>'template_ref') IS DISTINCT FROM tmpl.template_ref
        OR (code->>'path') IS DISTINCT FROM tmpl.block_path
        OR jsonb_typeof(code->'content') IS DISTINCT FROM 'string'
        OR (octet_length(code->>'content') BETWEEN 1 AND 16384) IS NOT TRUE
        OR (code->>'sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(code->>'content','UTF8')),'hex')
        OR jsonb_typeof(code->'admission') IS DISTINCT FROM 'object'
        OR (code->'admission'->>'result') IS DISTINCT FROM 'accepted'
        OR (code->'admission'->>'block_sha256') IS DISTINCT FROM (code->>'sha256') THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
      -- A configured method carries settings, never code: render its block again from the
      -- template row and refuse any difference. A code template carries no configuration.
      cfg:=tmpl.contract->'configuration';
      IF cfg IS NULL THEN
        IF code ? 'configuration' THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
      ELSE
        chosen:=code->'configuration'->'settings';
        IF jsonb_typeof(code->'configuration') IS DISTINCT FROM 'object'
          OR EXISTS (SELECT 1 FROM jsonb_object_keys(code->'configuration') k WHERE k NOT IN ('schema','block_sha256','settings'))
          OR (code->'configuration'->>'schema') IS DISTINCT FROM 'enrichedlab.configured-block.v1'
          OR (cfg->>'schema') IS DISTINCT FROM 'enrichedlab.configured-block.v1'
          OR jsonb_typeof(cfg->'block') IS DISTINCT FROM 'string'
          OR (cfg->>'block_sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(cfg->>'block','UTF8')),'hex')
          OR (code->'configuration'->>'block_sha256') IS DISTINCT FROM (cfg->>'block_sha256')
          OR jsonb_typeof(chosen) IS DISTINCT FROM 'object'
          OR jsonb_typeof(cfg->'settings') IS DISTINCT FROM 'object'
          OR (SELECT array_agg(k ORDER BY k) FROM jsonb_object_keys(chosen) k)
            IS DISTINCT FROM (SELECT array_agg(k ORDER BY k) FROM jsonb_object_keys(cfg->'settings') k) THEN
          RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
        rendered:=cfg->>'block'; described:='[]'::jsonb;
        FOR pick IN SELECT key,value FROM jsonb_each(chosen) ORDER BY key LOOP
          IF jsonb_typeof(pick.value) IS DISTINCT FROM 'string'
            OR jsonb_typeof(cfg->'settings'->pick.key->'choices'->(pick.value#>>'{}')->'slots') IS DISTINCT FROM 'object' THEN
            RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
          FOR fill IN SELECT key,value FROM jsonb_each_text(cfg->'settings'->pick.key->'choices'->(pick.value#>>'{}')->'slots') LOOP
            rendered:=replace(rendered,'{{'||fill.key||'}}',fill.value);
          END LOOP;
          -- The words the workspace shows come from the template row, never from the proposal.
          described:=described||jsonb_build_array(jsonb_build_object('setting',pick.key,
            'label',cfg->'settings'->pick.key->>'label','choice',pick.value#>>'{}',
            'choice_label',cfg->'settings'->pick.key->'choices'->(pick.value#>>'{}')->>'label'));
        END LOOP;
        IF strpos(rendered,'{{')>0 OR rendered IS DISTINCT FROM (code->>'content') THEN
          RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
      END IF;
    ELSIF code IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid';
    END IF;
    task:=enrichedlab_agent.broker_submit_project_task(p_tenant,r.actor_id,r.project_id,a->>'action_key',NULL,project.group_key,a->>'profile',a->>'objective',
      'research-'||p_run::text||'-'||(r.turn+1)::text||'-'||(a->>'action_key'),a->>'request_sha256');
    IF code IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_task_generated_code(tenant_id,task_id,profile_key,template_ref,template_sha256,path,
        content,size_bytes,sha256,admission,run_id,turn,configuration)
        VALUES(p_tenant,task,a->>'profile',tmpl.template_ref,tmpl.template_sha256,tmpl.block_path,code->>'content',
          octet_length(code->>'content'),code->>'sha256',code->'admission',p_run,r.turn+1,
          CASE WHEN cfg IS NULL THEN NULL ELSE (code->'configuration')||jsonb_build_object('described',described) END);
    END IF;
    tasks:=tasks||jsonb_build_array(jsonb_build_object('task_id',task,'action_key',a->>'action_key'));
    SELECT jsonb_agg(CASE WHEN s->>'action_key'=a->>'action_key' THEN s||jsonb_build_object('task_id',task,'status','active') ELSE s END ORDER BY ord)
      INTO plan FROM jsonb_array_elements(p_state->'plan') WITH ORDINALITY AS e(s,ord);
    p_state:=jsonb_set(p_state,'{plan}',plan);
  END LOOP;
  revision:=enrichedlab_agent.broker_checkpoint_project(p_tenant,r.actor_id,r.project_id,p_expected,gen_random_uuid(),p_state);
  INSERT INTO enrichedlab_agent.broker_research_turns VALUES(p_tenant,p_run,r.turn+1,p_generation,p_expected,revision,
    p_record||jsonb_build_object('submitted_tasks',tasks,'actor_id',r.actor_id,'outcome',p_outcome),clock_timestamp());
  UPDATE enrichedlab_agent.broker_research_runs SET turn=turn+1,failures=0,
    status=CASE WHEN p_outcome='delegate' THEN 'waiting' ELSE p_outcome END,
    code=CASE WHEN p_outcome='delegate' THEN 'waiting_for_tools' ELSE p_outcome END,
    available_at=clock_timestamp()+interval '5 seconds',lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=p_run;
  RETURN jsonb_build_object('revision',revision,'tasks',tasks,'turn',r.turn+1);
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_lineage(p_tenant text,p_actor uuid,p_task uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE t enrichedlab_agent.broker_tasks%ROWTYPE; project uuid; action text;
  o enrichedlab_agent.broker_task_origins%ROWTYPE; g enrichedlab_agent.broker_task_generated_code%ROWTYPE;
  m enrichedlab_agent.broker_task_methods%ROWTYPE;
  step jsonb; step_revision bigint; step_now jsonb; step_now_revision bigint; plan jsonb;
  workspace jsonb; workspace_revision bigint; turn_revision bigint; tool jsonb;
  tool_class text; output_class text; gaps jsonb:='[]'; outputs jsonb; has_template boolean;
  account_name text; account_kind text;
BEGIN
  SELECT * INTO t FROM enrichedlab_agent.broker_tasks WHERE tenant_id=p_tenant AND task_id=p_task;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_task_can(p_tenant,p_actor,p_task,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:task_not_found'; END IF;
  SELECT pt.project_id,pt.action_key INTO project,action FROM enrichedlab_agent.broker_project_tasks pt
    WHERE pt.tenant_id=p_tenant AND pt.task_id=p_task;
  SELECT * INTO o FROM enrichedlab_agent.broker_task_origins WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO g FROM enrichedlab_agent.broker_task_generated_code WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO m FROM enrichedlab_agent.broker_task_methods WHERE tenant_id=p_tenant AND task_id=p_task;
  -- The display name only: a task reader may not hold the capability that shows emails.
  SELECT a.display_name,CASE WHEN a.issuer='kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END
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
  IF workspace IS NOT NULL OR tool IS NOT NULL THEN gaps:=gaps||'"input_class_not_recorded"'::jsonb; END IF;

  -- What ran decides the class first: a block rendered from a configured method's settings,
  -- admitted model-written code, then a reviewed platform tool's job. A template the task was
  -- submitted under, with none of these, ran no model-written code, which is a gap.
  tool_class:=CASE WHEN g.task_id IS NOT NULL AND g.configuration IS NOT NULL THEN 'configured'
    WHEN g.task_id IS NOT NULL THEN 'agent_built' WHEN tool IS NOT NULL THEN 'vetted_service'
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
    'inputs',jsonb_build_object('workspace',workspace,'checkpoint_revision',workspace_revision,'platform_tool',tool),
    'method',jsonb_build_object('profile',t.profile_key,'profile_name',m.profile_name,'tool_class',tool_class,
      'template_ref',coalesce(g.template_ref,m.template_ref),'template_sha256',coalesce(g.template_sha256,m.template_sha256),
      'recorded',m.basis,'harness',t.profile_snapshot->'harness',
      'role',t.profile_snapshot->'role','tool_bundle',t.profile_snapshot->'tool_bundle'),
    'code',CASE WHEN g.task_id IS NULL THEN NULL ELSE jsonb_build_object('path',g.path,'sha256',g.sha256,
      'size_bytes',g.size_bytes,'admission',g.admission,'configuration',g.configuration,'run_id',g.run_id,'turn',g.turn,
      'created_at',g.created_at) END,
    'execution',jsonb_build_object('image_ref',t.profile_snapshot->'image_ref','cpu_limit',t.profile_snapshot->'cpu_limit',
      'memory_limit',t.profile_snapshot->'memory_limit','max_run_seconds',t.profile_snapshot->'max_run_seconds',
      'attempts',t.attempt,'scientific_history',(SELECT count(*) FROM enrichedlab_agent.broker_task_scientific_history h
        WHERE h.tenant_id=p_tenant AND h.task_id=p_task)),
    'outputs',outputs,
    'review',jsonb_build_object('state',CASE WHEN tool_class IN ('agent_built','agent_session','configured') THEN 'unvalidated' ELSE 'not_reviewed' END),
    'gaps',gaps);
END $$;

-- API: the block, template and admission record for a task the caller can read.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_generated_code(p_tenant text,p_actor uuid,p_task uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks WHERE tenant_id=p_tenant AND task_id=p_task
      AND enrichedlab_agent.broker_task_can(p_tenant,p_actor,p_task,'read')) THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:task_not_found'; END IF;
  RETURN (SELECT jsonb_build_object('task_id',g.task_id,'profile',g.profile_key,'template_ref',g.template_ref,
      'template_sha256',g.template_sha256,'path',g.path,'content',g.content,'size_bytes',g.size_bytes,'sha256',g.sha256,
      'admission',g.admission,'configuration',g.configuration,'run_id',g.run_id,'turn',g.turn,'created_at',g.created_at,
      'validation','unvalidated')
    FROM enrichedlab_agent.broker_task_generated_code g WHERE g.tenant_id=p_tenant AND g.task_id=p_task);
END $$;

-- Package: every admitted block in a project the caller can read. The API reads it in
-- the same repeatable-read transaction as broker_project_package_snapshot.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_generated_code(p_tenant text,p_actor uuid,p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  RETURN (SELECT coalesce(jsonb_agg(jsonb_build_object('task_id',g.task_id,'profile',g.profile_key,'template_ref',g.template_ref,
      'template_sha256',g.template_sha256,'path',g.path,'content',g.content,'size_bytes',g.size_bytes,'sha256',g.sha256,
      'admission',g.admission,'configuration',g.configuration,'run_id',g.run_id,'turn',g.turn,'created_at',g.created_at,
      'validation','unvalidated')
      ORDER BY g.created_at,g.task_id),'[]'::jsonb)
    FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_task_generated_code g USING (tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project);
END $$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE n.nspname='enrichedlab_agent' AND p.proname IN ('broker_commit_research_turn','broker_task_lineage',
      'broker_task_generated_code','broker_project_generated_code')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (22) ON CONFLICT DO NOTHING;
COMMIT;
