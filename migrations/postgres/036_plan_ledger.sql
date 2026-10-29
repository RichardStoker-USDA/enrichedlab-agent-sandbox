-- The plan ledger. One row per checkpoint revision, and one per plan step that revision changed, written
-- in the checkpoint's own transaction by the function that wrote it: the lead's turn (`lead_turn`), reconciliation from
-- analysis records (`analysis_record`), or a direct checkpoint by a person or a service account. One pure comparison
-- produces the step rows, live and in the replay of earlier checkpoints at the end of this file. The writers keep their
-- signatures and grants, and the recorder has no grant, so no caller chooses the label.
BEGIN;

-- Checkpoint writers take this table's rows FOR UPDATE; they wait here until the recording writers replace them.
LOCK TABLE enrichedlab_agent.broker_projects IN EXCLUSIVE MODE;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_plan_revisions (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  revision bigint NOT NULL,
  source text NOT NULL
    CHECK (source IN ('lead_turn', 'analysis_record', 'person', 'service_account', 'recorded_before_ledger')),
  run_id uuid,
  turn integer CHECK (turn >= 1),
  principal_id uuid,
  step_changes integer NOT NULL CHECK (step_changes BETWEEN 0 AND 2048),
  recorded_at timestamptz NOT NULL,
  PRIMARY KEY (tenant_id, project_id, revision),
  FOREIGN KEY (tenant_id, project_id, revision) REFERENCES enrichedlab_agent.broker_project_checkpoints,
  CHECK ((source = 'lead_turn') = (turn IS NOT NULL)),
  CHECK ((source IN ('lead_turn', 'analysis_record')) = (run_id IS NOT NULL)),
  CHECK ((source IN ('person', 'service_account')) = (principal_id IS NOT NULL))
);
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_plan_events (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  revision bigint NOT NULL,
  action_key text NOT NULL CHECK (action_key ~ '^[a-zA-Z0-9_-]{1,100}$'),
  step_number integer NOT NULL CHECK (step_number BETWEEN 1 AND 1024),
  changes text[] NOT NULL CHECK (cardinality(changes) BETWEEN 1 AND 4
    AND changes <@ ARRAY['planned', 'removed', 'text', 'task', 'status', 'results']::text[]),
  status_before text CHECK (status_before IN ('pending', 'active', 'completed', 'failed', 'uncertain', 'skipped')),
  status_after text CHECK (status_after IN ('pending', 'active', 'completed', 'failed', 'uncertain', 'skipped')),
  task_id uuid,
  result_count integer NOT NULL CHECK (result_count BETWEEN 0 AND 64),
  title text CHECK (octet_length(title) <= 2048),
  rationale text CHECK (octet_length(rationale) <= 8192),
  task_status text,
  error_code text,
  PRIMARY KEY (tenant_id, project_id, revision, action_key),
  FOREIGN KEY (tenant_id, project_id, revision) REFERENCES enrichedlab_agent.broker_plan_revisions,
  CHECK (('planned' = ANY (changes)) = (status_before IS NULL)),
  CHECK (('removed' = ANY (changes)) = (status_after IS NULL))
);
ALTER TABLE enrichedlab_agent.broker_plan_revisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_plan_events ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_plan_revisions, enrichedlab_agent.broker_plan_events FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_plan_revisions;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_plan_revisions
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_plan_events;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_plan_events
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- Compare two plans by action key. The live recorder and the replay both use this, so the same checkpoints always give
-- the same rows. A step's title and reason are kept when it is planned or when either changed.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_plan_changes(p_old jsonb, p_new jsonb)
RETURNS TABLE (action_key text, step_number integer, changes text[], status_before text, status_after text, task_id uuid,
               result_count integer, title text, rationale text)
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  WITH o AS (SELECT e.s->>'action_key' AS k, e.n::integer AS pos, e.s
               FROM jsonb_array_elements(COALESCE(p_old->'plan', '[]'::jsonb)) WITH ORDINALITY AS e(s, n)),
       w AS (SELECT e.s->>'action_key' AS k, e.n::integer AS pos, e.s
               FROM jsonb_array_elements(COALESCE(p_new->'plan', '[]'::jsonb)) WITH ORDINALITY AS e(s, n)),
       j AS (SELECT COALESCE(w.k, o.k) AS k, COALESCE(w.pos, o.pos) AS pos, o.s AS os, w.s AS ns
               FROM o FULL JOIN w ON o.k = w.k),
       c AS (SELECT j.k, j.pos, j.os, j.ns, CASE
               WHEN j.os IS NULL THEN ARRAY['planned']
               WHEN j.ns IS NULL THEN ARRAY['removed']
               ELSE array_remove(ARRAY[
                 CASE WHEN j.os->'title' IS DISTINCT FROM j.ns->'title'
                        OR j.os->'rationale' IS DISTINCT FROM j.ns->'rationale' THEN 'text' END,
                 CASE WHEN j.os->'task_id' IS DISTINCT FROM j.ns->'task_id' THEN 'task' END,
                 CASE WHEN j.os->'status' IS DISTINCT FROM j.ns->'status' THEN 'status' END,
                 CASE WHEN j.os->'evidence_refs' IS DISTINCT FROM j.ns->'evidence_refs' THEN 'results' END], NULL)
             END AS changes
             FROM j)
  SELECT c.k, c.pos, c.changes, c.os->>'status', c.ns->>'status', (COALESCE(c.ns, c.os)->>'task_id')::uuid,
         CASE WHEN jsonb_typeof(COALESCE(c.ns, c.os)->'evidence_refs') = 'array'
              THEN jsonb_array_length(COALESCE(c.ns, c.os)->'evidence_refs') ELSE 0 END,
         CASE WHEN c.changes && ARRAY['planned', 'text'] THEN c.ns->>'title' END,
         CASE WHEN c.changes && ARRAY['planned', 'text'] THEN c.ns->>'rationale' END
    FROM c
   WHERE cardinality(c.changes) > 0
   ORDER BY c.pos, c.k;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_plan_changes(jsonb, jsonb) FROM PUBLIC;

-- Record one checkpoint revision against the one before it. An analysis record's status and error code are kept only
-- when reconciliation moved a step to a finished status from a finished analysis, which cannot change afterwards.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_plan_record_one(p_tenant text, p_project uuid, p_revision bigint,
  p_source text, p_run uuid, p_turn integer, p_principal uuid)
RETURNS integer
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_new enrichedlab_agent.broker_project_checkpoints%ROWTYPE;
  v_old jsonb;
  v_changes integer;
BEGIN
  SELECT * INTO v_new FROM enrichedlab_agent.broker_project_checkpoints
  WHERE tenant_id = p_tenant AND project_id = p_project AND revision = p_revision;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:plan_ledger_revision_missing';
  END IF;
  SELECT state INTO v_old FROM enrichedlab_agent.broker_project_checkpoints
  WHERE tenant_id = p_tenant AND project_id = p_project AND revision = p_revision - 1;
  SELECT count(*) INTO v_changes FROM enrichedlab_agent.broker_plan_changes(v_old, v_new.state);
  INSERT INTO enrichedlab_agent.broker_plan_revisions (
    tenant_id, project_id, revision, source, run_id, turn, principal_id, step_changes, recorded_at
  ) VALUES (p_tenant, p_project, p_revision, p_source, p_run, p_turn, p_principal, v_changes, v_new.created_at);
  INSERT INTO enrichedlab_agent.broker_plan_events (
    tenant_id, project_id, revision, action_key, step_number, changes, status_before, status_after, task_id, result_count,
    title, rationale, task_status, error_code
  )
  SELECT p_tenant, p_project, p_revision, c.action_key, c.step_number, c.changes, c.status_before, c.status_after, c.task_id,
         c.result_count, c.title, c.rationale,
         CASE WHEN f.finished THEN t.status END, CASE WHEN f.finished THEN t.error_code END
  FROM enrichedlab_agent.broker_plan_changes(v_old, v_new.state) c
  LEFT JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = p_tenant AND t.task_id = c.task_id
  CROSS JOIN LATERAL (
    SELECT p_source = 'analysis_record' AND c.status_after IN ('completed', 'failed', 'uncertain')
           AND t.status IN ('completed', 'review_required', 'failed', 'cancelled') AS finished
  ) f;
  RETURN v_changes;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_plan_record_one(text, uuid, bigint, text, uuid, integer, uuid) FROM PUBLIC;

-- Record every checkpoint of a project before p_before (all when NULL) that has no ledger row, oldest first. A revision
-- that a recorded turn produced is the lead's; any other left no mark of its writer.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_plan_replay(p_tenant text, p_project uuid, p_before bigint)
RETURNS integer
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v record;
  v_count integer := 0;
BEGIN
  FOR v IN
    SELECT c.revision, t.run_id, t.turn
    FROM enrichedlab_agent.broker_project_checkpoints c
    LEFT JOIN LATERAL (
      SELECT rt.run_id, rt.turn FROM enrichedlab_agent.broker_research_turns rt
      JOIN enrichedlab_agent.broker_research_runs rr ON rr.tenant_id = rt.tenant_id AND rr.run_id = rt.run_id
      WHERE rt.tenant_id = c.tenant_id AND rr.project_id = c.project_id AND rt.checkpoint_revision = c.revision
    ) t ON true
    WHERE c.tenant_id = p_tenant AND c.project_id = p_project AND (p_before IS NULL OR c.revision < p_before)
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_plan_revisions r
                      WHERE r.tenant_id = c.tenant_id AND r.project_id = c.project_id AND r.revision = c.revision)
    ORDER BY c.revision
  LOOP
    PERFORM enrichedlab_agent.broker_plan_record_one(p_tenant, p_project, v.revision,
      CASE WHEN v.turn IS NULL THEN 'recorded_before_ledger' ELSE 'lead_turn' END, v.run_id, v.turn, NULL);
    v_count := v_count + 1;
  END LOOP;
  RETURN v_count;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_plan_replay(text, uuid, bigint) FROM PUBLIC;

-- The recorder the writers call. It first records any earlier revision an older writer left unrecorded.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_plan_record(p_tenant text, p_project uuid, p_revision bigint,
  p_source text, p_run uuid, p_turn integer, p_principal uuid)
RETURNS integer
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF p_source NOT IN ('lead_turn', 'analysis_record', 'person', 'service_account') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:plan_ledger_source_invalid';
  END IF;
  PERFORM enrichedlab_agent.broker_plan_replay(p_tenant, p_project, p_revision);
  RETURN enrichedlab_agent.broker_plan_record_one(p_tenant, p_project, p_revision, p_source, p_run, p_turn, p_principal);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_plan_record(text, uuid, bigint, text, uuid, integer, uuid) FROM PUBLIC;

-- The checkpoint body of migration 010, unchanged except that it takes the writer's label and records the plan changes
-- after the new revision exists. An idempotent repeat returns before writing and records nothing again.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_write_checkpoint(p_tenant text,p_actor uuid,p_id uuid,p_expected bigint,p_request uuid,p_state jsonb,p_source text,p_run uuid,p_turn integer,p_principal uuid)
RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE current_revision bigint; old enrichedlab_agent.broker_project_checkpoints%ROWTYPE; step jsonb;
BEGIN
  SELECT revision INTO current_revision FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=p_id FOR UPDATE;
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_id,'steer') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  SELECT * INTO old FROM enrichedlab_agent.broker_project_checkpoints WHERE tenant_id=p_tenant AND project_id=p_id AND request_id=p_request;
  IF FOUND THEN
    IF old.author_id<>p_actor OR old.state<>p_state OR old.revision<>p_expected+1 THEN
      RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:idempotency_conflict';
    END IF;
    RETURN old.revision;
  END IF;
  IF current_revision<>p_expected THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  IF p_state->>'schema_version' IS DISTINCT FROM '1' OR jsonb_typeof(p_state->'plan') IS DISTINCT FROM 'array' THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:checkpoint_invalid'; END IF;
  FOR step IN SELECT value FROM jsonb_array_elements(p_state->'plan') LOOP
    IF (step->>'status'='completed' AND step->>'task_id' IS NULL) OR (step->>'task_id' IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
      WHERE pt.tenant_id=p_tenant AND pt.project_id=p_id AND pt.task_id=(step->>'task_id')::uuid
        AND pt.action_key=step->>'action_key'
        AND (step->>'status'<>'completed' OR t.status IN ('completed','review_required'))
    )) THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:checkpoint_invalid'; END IF;
  END LOOP;
  INSERT INTO enrichedlab_agent.broker_project_checkpoints VALUES(p_tenant,p_id,current_revision+1,p_actor,p_request,p_state,clock_timestamp());
  UPDATE enrichedlab_agent.broker_projects SET revision=revision+1,updated_at=clock_timestamp() WHERE tenant_id=p_tenant AND project_id=p_id;
  PERFORM enrichedlab_agent.broker_plan_record(p_tenant,p_id,current_revision+1,p_source,p_run,p_turn,p_principal);
  RETURN current_revision+1;
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_write_checkpoint(text, uuid, uuid, bigint, uuid, jsonb, text, uuid, integer, uuid)
  FROM PUBLIC;

-- A direct checkpoint through the API: its author's principal class names the writer.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_checkpoint_project(p_tenant text,p_actor uuid,p_id uuid,p_expected bigint,p_request uuid,p_state jsonb)
RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE v_class text;
BEGIN
  SELECT principal_class INTO v_class FROM enrichedlab_agent.broker_principals WHERE tenant_id=p_tenant AND principal_id=p_actor;
  RETURN enrichedlab_agent.broker_write_checkpoint(p_tenant,p_actor,p_id,p_expected,p_request,p_state,
    CASE WHEN v_class='person' THEN 'person' ELSE 'service_account' END,NULL,NULL,p_actor);
END $$;

-- Migration 011's reconciliation, unchanged except that its checkpoint is labeled with its run.
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
  RETURN enrichedlab_agent.broker_write_checkpoint(p_tenant,r.actor_id,r.project_id,p_expected,gen_random_uuid(),p_state,'analysis_record',p_run,NULL,NULL);
END $$;

-- Migration 022's turn commit, unchanged except that its checkpoint is labeled with its run and turn.
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
  revision:=enrichedlab_agent.broker_write_checkpoint(p_tenant,r.actor_id,r.project_id,p_expected,gen_random_uuid(),p_state,'lead_turn',p_run,r.turn+1,NULL);
  INSERT INTO enrichedlab_agent.broker_research_turns VALUES(p_tenant,p_run,r.turn+1,p_generation,p_expected,revision,
    p_record||jsonb_build_object('submitted_tasks',tasks,'actor_id',r.actor_id,'outcome',p_outcome),clock_timestamp());
  UPDATE enrichedlab_agent.broker_research_runs SET turn=turn+1,failures=0,
    status=CASE WHEN p_outcome='delegate' THEN 'waiting' ELSE p_outcome END,
    code=CASE WHEN p_outcome='delegate' THEN 'waiting_for_tools' ELSE p_outcome END,
    available_at=clock_timestamp()+interval '5 seconds',lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=p_run;
  RETURN jsonb_build_object('revision',revision,'tasks',tasks,'turn',r.turn+1);
END $$;

-- Replay every existing checkpoint once. Applying this file again records nothing new.
DO $$
DECLARE
  v record;
BEGIN
  FOR v IN SELECT tenant_id, project_id FROM enrichedlab_agent.broker_projects ORDER BY tenant_id, project_id LOOP
    PERFORM enrichedlab_agent.broker_plan_replay(v.tenant_id, v.project_id, NULL);
  END LOOP;
END $$;

-- A project reader's view of its ledger: the newest 500 revisions, oldest first, each with its step rows, and the count
-- of checkpoints not yet recorded.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_plan_ledger(p_tenant text, p_actor uuid, p_project uuid)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_revisions jsonb;
  v_recorded bigint;
  v_unrecorded bigint;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  SELECT count(*) INTO v_recorded FROM enrichedlab_agent.broker_plan_revisions
  WHERE tenant_id = p_tenant AND project_id = p_project;
  SELECT count(*) INTO v_unrecorded FROM enrichedlab_agent.broker_project_checkpoints c
  WHERE c.tenant_id = p_tenant AND c.project_id = p_project
    AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_plan_revisions r
                    WHERE r.tenant_id = c.tenant_id AND r.project_id = c.project_id AND r.revision = c.revision);
  SELECT COALESCE(jsonb_agg(s.item ORDER BY s.revision), '[]'::jsonb) INTO v_revisions
  FROM (
    SELECT r.revision, jsonb_build_object(
      'revision', r.revision, 'recorded_at', r.recorded_at, 'source', r.source, 'run_id', r.run_id, 'turn', r.turn,
      'principal_id', r.principal_id,
      'steps', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
          'action_key', e.action_key, 'step_number', e.step_number, 'changes', to_jsonb(e.changes),
          'status_before', e.status_before, 'status_after', e.status_after, 'task_id', e.task_id,
          'result_count', e.result_count, 'title', e.title, 'rationale', e.rationale,
          'task_status', e.task_status, 'error_code', e.error_code) ORDER BY e.step_number, e.action_key)
        FROM enrichedlab_agent.broker_plan_events e
        WHERE e.tenant_id = r.tenant_id AND e.project_id = r.project_id AND e.revision = r.revision), '[]'::jsonb)
    ) AS item
    FROM enrichedlab_agent.broker_plan_revisions r
    WHERE r.tenant_id = p_tenant AND r.project_id = p_project
    ORDER BY r.revision DESC
    LIMIT 500
  ) s;
  RETURN jsonb_build_object('revisions', v_revisions, 'unrecorded_revisions', v_unrecorded,
                            'truncated', v_recorded > 500);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_plan_ledger(text, uuid, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (36) ON CONFLICT DO NOTHING;
COMMIT;
