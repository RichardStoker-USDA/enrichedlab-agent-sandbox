-- Plan pages and each analysis's method. A project's working plan holds at most 128 steps.
-- Finished steps leave it for immutable archive pages through one fenced writer, so a long project keeps going while
-- every step stays recorded: in its page, in the plan ledger under the new label `plan_archive`, and in its analysis.
-- Every archived action key stays reserved for its project: the database refuses an analysis under one, so no
-- coordinator, an older one after a rollback included, can plan it again. The claim carries the archived keys and
-- analyses, counts by status and by method, the newest archived steps with their titles, and each planned analysis's
-- method, from which the coordinator builds the lead's tool card. Everything else in the claim is migration 046's.
-- Rollback: an older coordinator ignores the added keys and never archives; the pages, reserved keys and ledger rows
-- stay as records.
BEGIN;

-- Checkpoint writers take this table's rows FOR UPDATE; they wait here until the archive writer exists.
LOCK TABLE enrichedlab_agent.broker_projects IN EXCLUSIVE MODE;

-- The plan ledger names the archive writer. Migration 036's two checks on the writer label are replaced by named ones
-- that also admit `plan_archive`, which records its run and no turn or principal.
DO $$
DECLARE c record;
BEGIN
  FOR c IN SELECT conname, pg_get_constraintdef(oid) AS def FROM pg_constraint
           WHERE conrelid = 'enrichedlab_agent.broker_plan_revisions'::regclass AND contype = 'c'
             AND conname NOT IN ('broker_plan_revisions_source_v2', 'broker_plan_revisions_run_v2') LOOP
    IF c.def LIKE '%recorded_before_ledger%' OR c.def LIKE '%(run_id IS NOT NULL)%' THEN
      EXECUTE format('ALTER TABLE enrichedlab_agent.broker_plan_revisions DROP CONSTRAINT %I', c.conname);
    END IF;
  END LOOP;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'enrichedlab_agent.broker_plan_revisions'::regclass
                 AND conname = 'broker_plan_revisions_source_v2') THEN
    ALTER TABLE enrichedlab_agent.broker_plan_revisions ADD CONSTRAINT broker_plan_revisions_source_v2 CHECK (source IN
      ('lead_turn', 'analysis_record', 'plan_archive', 'person', 'service_account', 'recorded_before_ledger'));
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'enrichedlab_agent.broker_plan_revisions'::regclass
                 AND conname = 'broker_plan_revisions_run_v2') THEN
    ALTER TABLE enrichedlab_agent.broker_plan_revisions ADD CONSTRAINT broker_plan_revisions_run_v2
      CHECK ((source IN ('lead_turn', 'analysis_record', 'plan_archive')) = (run_id IS NOT NULL));
  END IF;
END $$;

-- One row per archive page: the steps it took out of the working plan, in plan order, as the checkpoint held them.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_plan_pages (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  page integer NOT NULL CHECK (page BETWEEN 1 AND 4096),
  revision bigint NOT NULL,
  run_id uuid NOT NULL,
  steps jsonb NOT NULL CHECK (jsonb_typeof(steps) = 'array' AND jsonb_array_length(steps) BETWEEN 1 AND 128),
  steps_sha256 text NOT NULL CHECK (steps_sha256 ~ '^sha256:[0-9a-f]{64}$'),
  archived_at timestamptz NOT NULL,
  PRIMARY KEY (tenant_id, project_id, page),
  UNIQUE (tenant_id, project_id, revision),
  FOREIGN KEY (tenant_id, project_id, revision) REFERENCES enrichedlab_agent.broker_plan_revisions
);
ALTER TABLE enrichedlab_agent.broker_plan_pages ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_plan_pages FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_plan_pages;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_plan_pages
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- One row per archived step: its key stays reserved for the project, with its analysis, the analysis's submission time
-- and its place in its page. The index gives a claim the project's newest archived analyses without reading the rest.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_plan_archived_keys (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  action_key text NOT NULL CHECK (action_key ~ '^[a-zA-Z0-9_-]{1,100}$'),
  page integer NOT NULL,
  ordinal integer NOT NULL CHECK (ordinal BETWEEN 1 AND 128),
  task_id uuid,
  submitted_at timestamptz,
  status text NOT NULL CHECK (status IN ('completed', 'failed', 'skipped')),
  PRIMARY KEY (tenant_id, project_id, action_key),
  UNIQUE (tenant_id, project_id, page, ordinal),
  CHECK ((task_id IS NULL) = (submitted_at IS NULL)),
  FOREIGN KEY (tenant_id, project_id, page) REFERENCES enrichedlab_agent.broker_plan_pages
);
CREATE INDEX IF NOT EXISTS broker_plan_archived_keys_newest ON enrichedlab_agent.broker_plan_archived_keys
  (tenant_id, project_id, submitted_at DESC, task_id DESC) WHERE task_id IS NOT NULL;
ALTER TABLE enrichedlab_agent.broker_plan_archived_keys ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_plan_archived_keys FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_plan_archived_keys;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_plan_archived_keys
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- No analysis is ever bound to an archived key of its project, whichever writer tries.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_refuse_archived_task_key()
RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_plan_archived_keys k
             WHERE k.tenant_id=NEW.tenant_id AND k.project_id=NEW.project_id AND k.action_key=NEW.action_key) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid';
  END IF;
  RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_refuse_archived_task_key() FROM PUBLIC;
DROP TRIGGER IF EXISTS refuse_archived_key ON enrichedlab_agent.broker_project_tasks;
CREATE TRIGGER refuse_archived_key BEFORE INSERT OR UPDATE OF action_key ON enrichedlab_agent.broker_project_tasks
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_refuse_archived_task_key();

-- Migration 036's recorder, unchanged except that it accepts the archive writer's label.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_plan_record(p_tenant text, p_project uuid, p_revision bigint,
  p_source text, p_run uuid, p_turn integer, p_principal uuid)
RETURNS integer
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF p_source NOT IN ('lead_turn', 'analysis_record', 'plan_archive', 'person', 'service_account') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:plan_ledger_source_invalid';
  END IF;
  PERFORM enrichedlab_agent.broker_plan_replay(p_tenant, p_project, p_revision);
  RETURN enrichedlab_agent.broker_plan_record_one(p_tenant, p_project, p_revision, p_source, p_run, p_turn, p_principal);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_plan_record(text, uuid, bigint, text, uuid, integer, uuid) FROM PUBLIC;

-- The archive writer. Under the run's lease and the project's expected revision, it moves the named steps out of the
-- working plan into the next page and saves the rest as a new checkpoint, all in one transaction. Only finished steps
-- move: completed, failed or skipped, each with its analysis finished (never one waiting for a person's review). The
-- database takes the steps from the checkpoint it holds, so the caller names keys and never supplies a plan.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_archive_research_plan(p_tenant text, p_run uuid, p_worker text,
  p_generation bigint, p_expected bigint, p_keys text[])
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; prior jsonb; kept jsonb; moved jsonb; v_revision bigint;
  v_page integer;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run FOR UPDATE;
  -- Every comparison is NULL-safe: a missing worker, generation or revision must refuse, not pass.
  IF NOT FOUND OR r.status IS DISTINCT FROM 'running' OR r.lease_owner IS DISTINCT FROM p_worker
    OR r.generation IS DISTINCT FROM p_generation OR (r.lease_until > clock_timestamp()) IS NOT TRUE THEN
    RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  IF NOT enrichedlab_agent.broker_research_authorized(p_tenant,r.actor_id,r.project_id,r.allowed_profiles) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden'; END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=r.project_id AND revision=p_expected FOR UPDATE;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  SELECT state INTO prior FROM enrichedlab_agent.broker_project_checkpoints WHERE tenant_id=p_tenant AND project_id=r.project_id AND revision=p_expected;
  IF p_keys IS NULL OR cardinality(p_keys) NOT BETWEEN 1 AND 128
    OR cardinality(p_keys) <> (SELECT count(DISTINCT k) FROM unnest(p_keys) k)
    OR EXISTS (SELECT 1 FROM unnest(p_keys) k WHERE k IS NULL OR (SELECT count(*) FROM jsonb_array_elements(prior->'plan') s
      WHERE s->>'action_key'=k)<>1) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:plan_archive_invalid'; END IF;
  IF EXISTS (SELECT 1 FROM jsonb_array_elements(prior->'plan') s WHERE s->>'action_key'=ANY(p_keys) AND (
      (s->>'status' IN ('completed','failed','skipped')) IS NOT TRUE
      OR (s->>'task_id' IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
        WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND pt.task_id=(s->>'task_id')::uuid
          AND t.status IN ('completed','failed','cancelled'))))) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:plan_archive_unfinished'; END IF;
  SELECT coalesce(jsonb_agg(e.s ORDER BY e.n) FILTER (WHERE NOT (e.s->>'action_key'=ANY(p_keys))),'[]'::jsonb),
         coalesce(jsonb_agg(e.s ORDER BY e.n) FILTER (WHERE e.s->>'action_key'=ANY(p_keys)),'[]'::jsonb)
    INTO kept, moved FROM jsonb_array_elements(prior->'plan') WITH ORDINALITY AS e(s,n);
  v_revision:=enrichedlab_agent.broker_write_checkpoint(p_tenant,r.actor_id,r.project_id,p_expected,gen_random_uuid(),
    jsonb_set(prior,'{plan}',kept),'plan_archive',p_run,NULL,NULL);
  SELECT coalesce(max(page),0)+1 INTO v_page FROM enrichedlab_agent.broker_plan_pages
    WHERE tenant_id=p_tenant AND project_id=r.project_id;
  INSERT INTO enrichedlab_agent.broker_plan_pages VALUES (p_tenant,r.project_id,v_page,v_revision,p_run,moved,
    'sha256:'||encode(sha256(convert_to(moved::text,'UTF8')),'hex'),clock_timestamp());
  INSERT INTO enrichedlab_agent.broker_plan_archived_keys
    SELECT p_tenant,r.project_id,e.s->>'action_key',v_page,e.n::integer,(e.s->>'task_id')::uuid,t.submitted_at,e.s->>'status'
    FROM jsonb_array_elements(moved) WITH ORDINALITY AS e(s,n)
    LEFT JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=p_tenant AND t.task_id=(e.s->>'task_id')::uuid;
  RETURN jsonb_build_object('revision',v_revision,'page',v_page,'steps',jsonb_array_length(moved));
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_archive_research_plan(text,uuid,text,bigint,bigint,text[]) FROM PUBLIC;

-- Migration 046's claim, with each analysis's method in its facts and the archive summary.

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_claim_research_run(p_tenant text,p_worker text,p_seconds integer)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project jsonb; profiles jsonb; facts jsonb; pages jsonb;
  newest jsonb;
BEGIN
  IF p_seconds NOT BETWEEN 30 AND 600 OR length(p_worker) NOT BETWEEN 1 AND 100 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:lease_invalid'; END IF;
  IF NOT pg_try_advisory_xact_lock(hashtextextended('broker-research-claim:' || p_tenant, 0)) THEN RETURN NULL; END IF;
  -- A revoked delegation never becomes stuck as apparently active work.
  UPDATE enrichedlab_agent.broker_research_runs SET status='paused',code='delegation_revoked',
    generation=generation+1,lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND status IN ('queued','running','waiting')
      AND NOT enrichedlab_agent.broker_research_authorized(tenant_id,actor_id,project_id,allowed_profiles);
  WITH held AS (
    SELECT p.group_key, h.actor_id FROM enrichedlab_agent.broker_research_runs h
      JOIN enrichedlab_agent.broker_projects p ON p.tenant_id=h.tenant_id AND p.project_id=h.project_id
      WHERE h.tenant_id=p_tenant AND h.status='running' AND h.lease_until>clock_timestamp()),
  by_group AS (SELECT group_key, count(*) AS n FROM held GROUP BY group_key),
  by_actor AS (SELECT actor_id, count(*) AS n FROM held GROUP BY actor_id)
  SELECT q.* INTO r FROM enrichedlab_agent.broker_research_runs q
    JOIN enrichedlab_agent.broker_projects g ON g.tenant_id=q.tenant_id AND g.project_id=q.project_id
    LEFT JOIN by_group ON by_group.group_key=g.group_key
    LEFT JOIN by_actor ON by_actor.actor_id=q.actor_id
    WHERE q.tenant_id=p_tenant AND q.status IN ('queued','waiting','running') AND q.available_at<=clock_timestamp()
      AND (q.lease_until IS NULL OR q.lease_until<=clock_timestamp())
    ORDER BY coalesce(by_group.n,0)-floor(extract(epoch FROM clock_timestamp()-q.available_at)/120),
      coalesce(by_actor.n,0), q.available_at, q.created_at
    FOR UPDATE OF q SKIP LOCKED LIMIT 1;
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
  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id',t.task_id,'action_key',pt.action_key,'profile',t.profile_key,'status',t.status,
    'result_class',t.result_class,'error_code',t.error_code,
    'evidence_refs',(SELECT coalesce(jsonb_agg('artifact://tasks/'||t.task_id::text||'/'||a.artifact_id::text),'[]') FROM (
      SELECT artifact_id FROM enrichedlab_agent.broker_artifacts WHERE tenant_id=p_tenant AND task_id=t.task_id AND visibility='researcher' ORDER BY artifact_id LIMIT 64
    ) a))),'[]') INTO facts
    FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND EXISTS (
      SELECT 1 FROM jsonb_array_elements(project->'checkpoint'->'state'->'plan') s WHERE s->>'task_id'=t.task_id::text
    );
  -- The project's archived steps: every archived action key, so none is planned again, their analyses
  -- (still this project's own work), the counts by status and by method, and the newest sixteen with their titles.
  WITH archived AS (
    SELECT pp.page, s.n, s.v FROM enrichedlab_agent.broker_plan_pages pp
      CROSS JOIN LATERAL jsonb_array_elements(pp.steps) WITH ORDINALITY AS s(v, n)
      WHERE pp.tenant_id=p_tenant AND pp.project_id=r.project_id),
  methods AS (
    SELECT t.profile_key AS method, a.v->>'status' AS status, count(*) AS n FROM archived a
      JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=p_tenant AND t.task_id=(a.v->>'task_id')::uuid
      GROUP BY 1, 2)
  SELECT jsonb_build_object(
      'pages', (SELECT count(DISTINCT page) FROM archived),
      'steps', (SELECT count(*) FROM archived),
      'keys', coalesce((SELECT jsonb_agg(k.action_key ORDER BY k.page, k.ordinal)
        FROM enrichedlab_agent.broker_plan_archived_keys k WHERE k.tenant_id=p_tenant AND k.project_id=r.project_id), '[]'::jsonb),
      'tasks', coalesce((SELECT jsonb_agg(k.task_id ORDER BY k.page, k.ordinal)
        FROM enrichedlab_agent.broker_plan_archived_keys k WHERE k.tenant_id=p_tenant AND k.project_id=r.project_id
          AND k.task_id IS NOT NULL), '[]'::jsonb),
      'by_status', coalesce((SELECT jsonb_object_agg(status, c ORDER BY status) FROM (
        SELECT v->>'status' AS status, count(*) AS c FROM archived GROUP BY 1) x), '{}'::jsonb),
      'by_method', coalesce((SELECT jsonb_object_agg(method, statuses ORDER BY method) FROM (
        SELECT method, jsonb_object_agg(status, n ORDER BY status) AS statuses FROM methods GROUP BY method) y), '{}'::jsonb),
      'newest', coalesce((SELECT jsonb_agg(jsonb_build_object('action_key', v->>'action_key', 'title', v->>'title',
        'status', v->>'status') ORDER BY page DESC, n DESC) FROM (
        SELECT * FROM archived ORDER BY page DESC, n DESC LIMIT 16) z), '[]'::jsonb))
    INTO pages;
  -- The project's 64 newest analyses by submission, in the working plan or archived, oldest first: the coordinator reads
  -- their queries so none is proposed again. Archival keeps steps an earlier finding reads, so plan order is not age
  -- order. The archive's newest 64 come from its index and the plan holds at most
  -- 128, so the cost does not grow with the project's history.
  SELECT coalesce(jsonb_agg(x.task_id ORDER BY x.submitted_at, x.task_id), '[]'::jsonb) INTO newest FROM (
    SELECT u.task_id, u.submitted_at FROM (
        (SELECT k.task_id, k.submitted_at FROM enrichedlab_agent.broker_plan_archived_keys k
          WHERE k.tenant_id=p_tenant AND k.project_id=r.project_id AND k.task_id IS NOT NULL
          ORDER BY k.submitted_at DESC, k.task_id DESC LIMIT 64)
        UNION
        SELECT t.task_id, t.submitted_at FROM jsonb_array_elements(project->'checkpoint'->'state'->'plan') s
          JOIN enrichedlab_agent.broker_project_tasks pt
            ON pt.tenant_id=p_tenant AND pt.task_id=(s->>'task_id')::uuid AND pt.project_id=r.project_id
          JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=p_tenant AND t.task_id=pt.task_id
          WHERE s->>'task_id' IS NOT NULL) u
      ORDER BY u.submitted_at DESC, u.task_id DESC LIMIT 64) x;
  RETURN jsonb_build_object('run',to_jsonb(r)-'tenant_id','project',project,'profiles',profiles,'task_facts',facts,
    'plan_pages',pages,'newest_tasks',newest);
END $$;


REVOKE ALL ON FUNCTION enrichedlab_agent.broker_claim_research_run(text,text,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (48) ON CONFLICT DO NOTHING;
COMMIT;
