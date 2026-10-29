-- Bounded model-written code: a reviewed template with one replaceable block. The
-- block is admitted when the coordinator commits its task, stays byte-exact here,
-- and reaches the sandbox only as a sealed input the custodian reads for the lease.
BEGIN;

-- Operator configuration: profiles that run a reviewed template. No runtime role
-- writes it; a new template version is a new profile.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_profile_templates (
  tenant_id text NOT NULL,
  profile_key text NOT NULL,
  template_ref text NOT NULL CHECK (template_ref ~ '^template://[a-z0-9][a-z0-9._/-]{0,127}$' AND template_ref !~ '\.\.'),
  template_sha256 text NOT NULL CHECK (template_sha256 ~ '^sha256:[0-9a-f]{64}$'),
  block_path text NOT NULL DEFAULT 'generated/block.R' CHECK (block_path = 'generated/block.R'),
  contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object' AND octet_length(contract::text) <= 16384),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, profile_key),
  FOREIGN KEY (tenant_id, profile_key) REFERENCES enrichedlab_agent.broker_agent_profiles
);

-- The exact block admitted with one task. The task's request digest binds its SHA-256.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_generated_code (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  profile_key text NOT NULL,
  template_ref text NOT NULL,
  template_sha256 text NOT NULL,
  path text NOT NULL CHECK (path = 'generated/block.R'),
  content text NOT NULL CHECK (octet_length(content) BETWEEN 1 AND 16384),
  size_bytes integer NOT NULL,
  sha256 text NOT NULL,
  admission jsonb NOT NULL CHECK (jsonb_typeof(admission) = 'object' AND octet_length(admission::text) <= 32768),
  run_id uuid NOT NULL,
  turn integer NOT NULL CHECK (turn > 0),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, run_id) REFERENCES enrichedlab_agent.broker_research_runs,
  CHECK (size_bytes = octet_length(content)),
  CHECK (sha256 = 'sha256:' || encode(sha256(convert_to(content, 'UTF8')), 'hex'))
);

DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_profile_templates','broker_task_generated_code'] LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I ENABLE ROW LEVEL SECURITY',t);
    EXECUTE format('REVOKE ALL ON enrichedlab_agent.%I FROM PUBLIC',t);
    EXECUTE format('DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.%I',t);
    EXECUTE format('CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.%I FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation()',t);
  END LOOP;
END $$;

-- 011 claim, plus each allowed template's reference and contract for the lead.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_claim_research_run(p_tenant text,p_worker text,p_seconds integer)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project jsonb; profiles jsonb; facts jsonb;
BEGIN
  IF p_seconds NOT BETWEEN 30 AND 600 OR length(p_worker) NOT BETWEEN 1 AND 100 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:lease_invalid'; END IF;
  -- A revoked delegation never becomes stuck as apparently active work.
  UPDATE enrichedlab_agent.broker_research_runs SET status='paused',code='delegation_revoked',
    generation=generation+1,lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND status IN ('queued','running','waiting')
      AND NOT enrichedlab_agent.broker_research_authorized(tenant_id,actor_id,project_id,allowed_profiles);
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs
    WHERE tenant_id=p_tenant AND status IN ('queued','waiting','running') AND available_at<=clock_timestamp()
      AND (lease_until IS NULL OR lease_until<=clock_timestamp())
    ORDER BY available_at,created_at FOR UPDATE SKIP LOCKED LIMIT 1;
  IF NOT FOUND THEN RETURN NULL; END IF;
  UPDATE enrichedlab_agent.broker_research_runs SET status='running',generation=generation+1,lease_owner=p_worker,
    lease_until=clock_timestamp()+make_interval(secs=>p_seconds),updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=r.run_id RETURNING * INTO r;
  project:=enrichedlab_agent.broker_get_project(p_tenant,r.actor_id,r.project_id);
  SELECT coalesce(jsonb_agg(jsonb_build_object('key',f.profile_key,'name',f.name,'description',f.description,'tool_bundle',f.tool_bundle)
      || CASE WHEN t.profile_key IS NULL THEN '{}'::jsonb ELSE jsonb_build_object('template',
        jsonb_build_object('template_ref',t.template_ref,'block_path',t.block_path,'contract',t.contract)) END),'[]')
    INTO profiles FROM enrichedlab_agent.broker_agent_profiles f
    LEFT JOIN enrichedlab_agent.broker_profile_templates t ON t.tenant_id=f.tenant_id AND t.profile_key=f.profile_key
    WHERE f.tenant_id=p_tenant AND f.profile_key=ANY(r.allowed_profiles) AND f.enabled;
  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id',t.task_id,'action_key',pt.action_key,'status',t.status,
    'result_class',t.result_class,'error_code',t.error_code,
    'evidence_refs',(SELECT coalesce(jsonb_agg('artifact://tasks/'||t.task_id::text||'/'||a.artifact_id::text),'[]') FROM (
      SELECT artifact_id FROM enrichedlab_agent.broker_artifacts WHERE tenant_id=p_tenant AND task_id=t.task_id AND visibility='researcher' ORDER BY artifact_id LIMIT 64
    ) a))),'[]') INTO facts
    FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND EXISTS (
      SELECT 1 FROM jsonb_array_elements(project->'checkpoint'->'state'->'plan') s WHERE s->>'task_id'=t.task_id::text
    );
  RETURN jsonb_build_object('run',to_jsonb(r)-'tenant_id','project',project,'profiles',profiles,'task_facts',facts);
END $$;

-- 011 commit, plus: a template profile requires one admitted block, stored with the
-- task in the same transaction; any other profile refuses a block.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_commit_research_turn(p_tenant text,p_run uuid,p_worker text,p_generation bigint,p_expected bigint,p_state jsonb,p_actions jsonb,p_record jsonb,p_outcome text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project enrichedlab_agent.broker_projects%ROWTYPE;
  tmpl enrichedlab_agent.broker_profile_templates%ROWTYPE;
  a jsonb; code jsonb; task uuid; tasks jsonb:='[]'; plan jsonb; revision bigint; pending integer;
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
    code:=a->'generated_code';
    SELECT * INTO tmpl FROM enrichedlab_agent.broker_profile_templates WHERE tenant_id=p_tenant AND profile_key=a->>'profile';
    IF FOUND THEN
      -- Every comparison is NULL-safe: a missing field must refuse, not pass.
      IF jsonb_typeof(code) IS DISTINCT FROM 'object'
        OR EXISTS (SELECT 1 FROM jsonb_object_keys(code) k WHERE k NOT IN ('template_ref','path','content','sha256','admission'))
        OR (code->>'template_ref') IS DISTINCT FROM tmpl.template_ref
        OR (code->>'path') IS DISTINCT FROM tmpl.block_path
        OR jsonb_typeof(code->'content') IS DISTINCT FROM 'string'
        OR (octet_length(code->>'content') BETWEEN 1 AND 16384) IS NOT TRUE
        OR (code->>'sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(code->>'content','UTF8')),'hex')
        OR jsonb_typeof(code->'admission') IS DISTINCT FROM 'object'
        OR (code->'admission'->>'result') IS DISTINCT FROM 'accepted'
        OR (code->'admission'->>'block_sha256') IS DISTINCT FROM (code->>'sha256') THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
    ELSIF code IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid';
    END IF;
    task:=enrichedlab_agent.broker_submit_project_task(p_tenant,r.actor_id,r.project_id,a->>'action_key',NULL,project.group_key,a->>'profile',a->>'objective',
      'research-'||p_run::text||'-'||(r.turn+1)::text||'-'||(a->>'action_key'),a->>'request_sha256');
    IF code IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_task_generated_code(tenant_id,task_id,profile_key,template_ref,template_sha256,path,
        content,size_bytes,sha256,admission,run_id,turn)
        VALUES(p_tenant,task,a->>'profile',tmpl.template_ref,tmpl.template_sha256,tmpl.block_path,code->>'content',
          octet_length(code->>'content'),code->>'sha256',code->'admission',p_run,r.turn+1);
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

-- Worker: a digest-only reference for its own current lease, so the preparation
-- notice can name the block without carrying it.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_admitted_code(p_tenant text,p_task uuid,p_worker text,p_attempt integer,p_generation bigint)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT jsonb_build_object('templateRef',g.template_ref,'path',g.path,'sizeBytes',g.size_bytes,'sha256',g.sha256)
  FROM enrichedlab_agent.broker_tasks t JOIN enrichedlab_agent.broker_task_generated_code g USING (tenant_id,task_id)
  WHERE t.tenant_id=p_tenant AND t.task_id=p_task AND t.lease_owner=p_worker AND t.attempt=p_attempt
    AND t.lease_generation=p_generation AND t.status IN ('leased','starting','running') AND t.desired_status='run'
    AND t.lease_expires_at>CURRENT_TIMESTAMP
$$;

-- Input custodian: the exact bytes, for the same current lease only.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_custody_admitted_code(p_tenant text,p_task uuid,p_worker text,p_attempt integer,p_generation bigint)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT jsonb_build_object('templateRef',g.template_ref,'path',g.path,'sizeBytes',g.size_bytes,'sha256',g.sha256,'content',g.content)
  FROM enrichedlab_agent.broker_tasks t JOIN enrichedlab_agent.broker_task_generated_code g USING (tenant_id,task_id)
  WHERE t.tenant_id=p_tenant AND t.task_id=p_task AND t.lease_owner=p_worker AND t.attempt=p_attempt
    AND t.lease_generation=p_generation AND t.status IN ('leased','starting','running') AND t.desired_status='run'
    AND t.lease_expires_at>CURRENT_TIMESTAMP
$$;

-- API: the block, template and admission record for a task the caller can read.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_generated_code(p_tenant text,p_actor uuid,p_task uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks WHERE tenant_id=p_tenant AND task_id=p_task
      AND enrichedlab_agent.broker_task_can(p_tenant,p_actor,p_task,'read')) THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:task_not_found'; END IF;
  RETURN (SELECT jsonb_build_object('task_id',g.task_id,'profile',g.profile_key,'template_ref',g.template_ref,
      'template_sha256',g.template_sha256,'path',g.path,'content',g.content,'size_bytes',g.size_bytes,'sha256',g.sha256,
      'admission',g.admission,'run_id',g.run_id,'turn',g.turn,'created_at',g.created_at,'validation','unvalidated')
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
      'admission',g.admission,'run_id',g.run_id,'turn',g.turn,'created_at',g.created_at,'validation','unvalidated')
      ORDER BY g.created_at,g.task_id),'[]'::jsonb)
    FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_task_generated_code g USING (tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project);
END $$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE n.nspname='enrichedlab_agent' AND p.proname IN ('broker_claim_research_run','broker_commit_research_turn',
      'broker_task_admitted_code','broker_custody_admitted_code','broker_task_generated_code','broker_project_generated_code')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (15) ON CONFLICT DO NOTHING;
COMMIT;
