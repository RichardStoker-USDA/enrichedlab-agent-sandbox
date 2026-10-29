BEGIN;

-- The coordinator is a trusted, function-only executor. The model receives
-- neither this role nor a credential. Each write rechecks the human delegation.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_research_runs (
  tenant_id text NOT NULL,
  run_id uuid NOT NULL,
  project_id uuid NOT NULL,
  actor_id uuid NOT NULL,
  initial_revision bigint NOT NULL,
  allowed_profiles text[] NOT NULL,
  max_turns integer NOT NULL CHECK (max_turns BETWEEN 1 AND 128),
  max_parallel integer NOT NULL CHECK (max_parallel BETWEEN 1 AND 8),
  status text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued','running','waiting','paused','review_required','failed')),
  turn integer NOT NULL DEFAULT 0,
  generation bigint NOT NULL DEFAULT 0,
  lease_owner text,
  lease_until timestamptz,
  available_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  failures integer NOT NULL DEFAULT 0,
  code text NOT NULL DEFAULT 'requested',
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id,run_id),
  FOREIGN KEY (tenant_id,project_id) REFERENCES enrichedlab_agent.broker_projects,
  FOREIGN KEY (tenant_id,actor_id) REFERENCES enrichedlab_agent.broker_principals
);
CREATE UNIQUE INDEX IF NOT EXISTS broker_one_active_research_run
  ON enrichedlab_agent.broker_research_runs(tenant_id,project_id)
  WHERE status IN ('queued','running','waiting');
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_research_turns (
  tenant_id text NOT NULL,
  run_id uuid NOT NULL,
  turn integer NOT NULL,
  generation bigint NOT NULL,
  context_revision bigint NOT NULL,
  checkpoint_revision bigint NOT NULL,
  record jsonb NOT NULL CHECK (jsonb_typeof(record)='object' AND octet_length(record::text)<=131072),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id,run_id,turn),
  FOREIGN KEY (tenant_id,run_id) REFERENCES enrichedlab_agent.broker_research_runs
);
ALTER TABLE enrichedlab_agent.broker_research_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_research_turns ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_research_runs,enrichedlab_agent.broker_research_turns FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_research_turns;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_research_turns
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_authorized(p_tenant text,p_actor uuid,p_project uuid,p_profiles text[])
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'steer')
    AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'contribute')
    AND cardinality(p_profiles) BETWEEN 1 AND 16
    AND NOT EXISTS (
      SELECT 1 FROM unnest(p_profiles) k WHERE k IS NULL OR NOT EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_projects p
        JOIN enrichedlab_agent.broker_group_profile_grants g USING(tenant_id,group_key)
        JOIN enrichedlab_agent.broker_agent_profiles f USING(tenant_id,profile_key)
        WHERE p.tenant_id=p_tenant AND p.project_id=p_project AND f.profile_key=k AND f.enabled
      )
    )
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_start_research_run(p_tenant text,p_actor uuid,p_project uuid,p_run uuid,p_revision bigint,p_profiles text[],p_turns integer,p_parallel integer)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE prior enrichedlab_agent.broker_research_runs%ROWTYPE; rev bigint;
BEGIN
  SELECT revision INTO rev FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=p_project FOR UPDATE;
  IF NOT enrichedlab_agent.broker_research_authorized(p_tenant,p_actor,p_project,p_profiles) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden'; END IF;
  SELECT * INTO prior FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run;
  IF FOUND THEN
    IF prior.project_id<>p_project OR prior.actor_id<>p_actor OR prior.initial_revision<>p_revision OR prior.allowed_profiles<>p_profiles OR prior.max_turns<>p_turns OR prior.max_parallel<>p_parallel THEN
      RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:idempotency_conflict'; END IF;
    RETURN p_run;
  END IF;
  IF rev IS DISTINCT FROM p_revision OR rev<1 THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  INSERT INTO enrichedlab_agent.broker_research_runs(tenant_id,run_id,project_id,actor_id,initial_revision,allowed_profiles,max_turns,max_parallel)
    VALUES(p_tenant,p_run,p_project,p_actor,p_revision,p_profiles,p_turns,p_parallel);
  INSERT INTO enrichedlab_agent.broker_project_audit(tenant_id,project_id,actor_id,kind,detail)
    VALUES(p_tenant,p_project,p_actor,'research_started',jsonb_build_object('run_id',p_run,'allowed_profiles',p_profiles,'max_turns',p_turns,'max_parallel',p_parallel));
  RETURN p_run;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_research_runs(p_tenant text,p_actor uuid,p_project uuid)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  RETURN QUERY SELECT to_jsonb(r)-'tenant_id'-'lease_owner'-'lease_until' FROM enrichedlab_agent.broker_research_runs r
    WHERE tenant_id=p_tenant AND project_id=p_project ORDER BY created_at DESC LIMIT 100;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_turn_history(p_tenant text,p_actor uuid,p_run uuid,p_after integer,p_limit integer)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_research_runs r WHERE tenant_id=p_tenant AND run_id=p_run
    AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,r.project_id,'read')) THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  IF p_after<0 OR p_limit NOT BETWEEN 1 AND 100 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:list_limit_invalid'; END IF;
  RETURN QUERY SELECT to_jsonb(t)-'tenant_id' FROM enrichedlab_agent.broker_research_turns t
    WHERE tenant_id=p_tenant AND run_id=p_run AND turn>p_after ORDER BY turn LIMIT p_limit;
END $$;

-- Pause fences the lead immediately. Already submitted tools retain their task
-- lifecycle and evidence. Cancelling a tool is the separate task capability.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_pause_research_run(p_tenant text,p_actor uuid,p_run uuid)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,r.project_id,'steer') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  UPDATE enrichedlab_agent.broker_research_runs SET status='paused',code='user_paused',generation=generation+1,
    lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp() WHERE tenant_id=p_tenant AND run_id=p_run;
  INSERT INTO enrichedlab_agent.broker_project_audit(tenant_id,project_id,actor_id,kind,detail)
    VALUES(p_tenant,r.project_id,p_actor,'research_paused',jsonb_build_object('run_id',p_run));
END $$;

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
  SELECT coalesce(jsonb_agg(jsonb_build_object('key',f.profile_key,'name',f.name,'description',f.description,'tool_bundle',f.tool_bundle)),'[]')
    INTO profiles FROM enrichedlab_agent.broker_agent_profiles f WHERE tenant_id=p_tenant AND profile_key=ANY(r.allowed_profiles) AND enabled;
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

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_renew_research_run(p_tenant text,p_run uuid,p_worker text,p_generation bigint,p_seconds integer)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF p_seconds NOT BETWEEN 30 AND 600 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:lease_invalid'; END IF;
  UPDATE enrichedlab_agent.broker_research_runs SET lease_until=clock_timestamp()+make_interval(secs=>p_seconds)
    WHERE tenant_id=p_tenant AND run_id=p_run AND status='running' AND lease_owner=p_worker AND generation=p_generation
      AND lease_until>clock_timestamp() AND enrichedlab_agent.broker_research_authorized(tenant_id,actor_id,project_id,allowed_profiles);
  RETURN FOUND;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_yield_research_run(p_tenant text,p_run uuid,p_worker text,p_generation bigint,p_code text)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF p_code NOT IN ('waiting_for_tools','model_unavailable','proposal_invalid','checkpoint_changed','turn_limit','review_required') THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  UPDATE enrichedlab_agent.broker_research_runs SET
    failures=failures+CASE WHEN p_code IN ('model_unavailable','proposal_invalid') THEN 1 ELSE 0 END,
    status=CASE WHEN p_code IN ('turn_limit','review_required') THEN 'review_required'
      WHEN p_code IN ('model_unavailable','proposal_invalid') AND failures>=2 THEN 'paused' ELSE 'waiting' END,
    code=p_code,available_at=clock_timestamp()+CASE WHEN p_code='waiting_for_tools' THEN interval '5 seconds' ELSE interval '15 seconds' END,
    lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=p_run AND status='running' AND lease_owner=p_worker AND generation=p_generation AND lease_until>clock_timestamp();
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_reconcile_research_run(p_tenant text,p_run uuid,p_worker text,p_generation bigint,p_expected bigint,p_state jsonb)
RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; prior jsonb;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run FOR UPDATE;
  IF NOT FOUND OR r.status<>'running' OR r.lease_owner IS DISTINCT FROM p_worker OR r.generation<>p_generation OR r.lease_until<=clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  IF NOT enrichedlab_agent.broker_research_authorized(p_tenant,r.actor_id,r.project_id,r.allowed_profiles) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden'; END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=r.project_id AND revision=p_expected FOR UPDATE;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  SELECT state INTO prior FROM enrichedlab_agent.broker_project_checkpoints WHERE tenant_id=p_tenant AND project_id=r.project_id AND revision=p_expected;
  IF prior=p_state THEN RETURN p_expected; END IF;
  -- Machine reconciliation may update task statuses/evidence only. It does not
  -- rewrite the question, methods, proposed lessons or chosen next action.
  IF prior-'plan' IS DISTINCT FROM p_state-'plan' OR jsonb_array_length(prior->'plan') IS DISTINCT FROM jsonb_array_length(p_state->'plan')
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(prior->'plan') WITH ORDINALITY p(v,n)
      FULL JOIN jsonb_array_elements(p_state->'plan') WITH ORDINALITY q(v,n) USING(n)
      WHERE p.v-'status'-'evidence_refs' IS DISTINCT FROM q.v-'status'-'evidence_refs') THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:checkpoint_invalid'; END IF;
  RETURN enrichedlab_agent.broker_checkpoint_project(p_tenant,r.actor_id,r.project_id,p_expected,gen_random_uuid(),p_state);
END $$;

-- One transaction records the decision, task submissions and next checkpoint.
-- Replaying a committed turn cannot enqueue another action. A stale generation
-- cannot commit, and a scientist's intervening checkpoint always wins.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_commit_research_turn(p_tenant text,p_run uuid,p_worker text,p_generation bigint,p_expected bigint,p_state jsonb,p_actions jsonb,p_record jsonb,p_outcome text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project enrichedlab_agent.broker_projects%ROWTYPE;
  a jsonb; task uuid; tasks jsonb:='[]'; plan jsonb; revision bigint; pending integer;
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
    task:=enrichedlab_agent.broker_submit_project_task(p_tenant,r.actor_id,r.project_id,a->>'action_key',NULL,project.group_key,a->>'profile',a->>'objective',
      'research-'||p_run::text||'-'||(r.turn+1)::text||'-'||(a->>'action_key'),a->>'request_sha256');
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

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE n.nspname='enrichedlab_agent' AND p.proname IN ('broker_research_authorized','broker_start_research_run','broker_list_research_runs',
      'broker_research_turn_history','broker_pause_research_run','broker_claim_research_run','broker_renew_research_run','broker_yield_research_run','broker_commit_research_turn','broker_reconcile_research_run')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (11) ON CONFLICT DO NOTHING;
COMMIT;
