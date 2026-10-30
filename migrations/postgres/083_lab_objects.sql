-- Lab objects: a lab's files in its object bucket become analysis inputs. A research run may list
-- the lab's objects method (route `server-objects`). Its step names one object collection the coordinator showed (a
-- knowledge record of the lab's bucket: the bucket, the collection's key prefix, the copy generation and the SHA-256 of
-- its manifest) and 1 to 20 of its files, and nothing else: no code, no query, no analysis. The turn commit checks the
-- shape and records the request (`broker_object_requests`); the worker reads it under its lease, refuses a bucket that
-- is not its configured one for the method, reads the pinned manifest, gets each file by version, checks size and
-- SHA-256, keeps a package and records one receipt per attempt (`broker_object_receipts`). A person's generic
-- submission cannot create such a task: the route is request-only. Everything else in the commit is migration 072's.
-- Rollback: an older coordinator never sends `objects`, an older worker never claims the route (its dispatch has no
-- such route), and this migration's tables and functions can stay.
BEGIN;

-- Routes whose tasks only a person's request or a research run creates (migration 072), now with lab objects.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_only_route(p_route text)
RETURNS boolean LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT p_route IN ('server-render', 'server-analysis', 'server-dataset', 'server-document', 'server-objects')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_request_only_route(text) FROM PUBLIC;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_object_requests (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  profile_key text NOT NULL,
  collection_id uuid NOT NULL,
  bucket text NOT NULL CHECK (bucket ~ '^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$'),
  prefix text NOT NULL CHECK (prefix ~ '^[a-z0-9_-]{1,64}/[a-z0-9_-]{1,64}/[a-z0-9_-]{1,64}/$'),
  generation text NOT NULL CHECK (generation ~ '^[a-z0-9_-]{1,64}$'),
  manifest_sha256 text NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
  files text[] NOT NULL CHECK (cardinality(files) BETWEEN 1 AND 20),
  request jsonb NOT NULL CHECK (jsonb_typeof(request) = 'object' AND octet_length(request::text) <= 65536),
  request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  run_id uuid,
  turn integer,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, profile_key) REFERENCES enrichedlab_agent.broker_agent_profiles (tenant_id, profile_key)
);
ALTER TABLE enrichedlab_agent.broker_object_requests ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_object_requests FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_object_requests;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_object_requests
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- One receipt per attempt: what the worker read and kept, or why it stopped.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_object_receipts (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  status text NOT NULL CHECK (status IN ('completed', 'refused', 'failed')),
  error_code text CHECK (error_code IS NULL OR error_code ~ '^[a-z0-9_:]{1,80}$'),
  files integer CHECK (files IS NULL OR files BETWEEN 1 AND 20),
  bytes bigint CHECK (bytes IS NULL OR bytes >= 0),
  tables jsonb CHECK (tables IS NULL OR (jsonb_typeof(tables) = 'array' AND octet_length(tables::text) <= 65536)),
  package_sha256 text CHECK (package_sha256 IS NULL OR package_sha256 ~ '^[0-9a-f]{64}$'),
  started_at timestamptz NOT NULL,
  finished_at timestamptz NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_object_requests (tenant_id, task_id),
  CHECK ((status = 'completed') = (files IS NOT NULL AND bytes IS NOT NULL AND tables IS NOT NULL AND package_sha256 IS NOT NULL)),
  CHECK ((status = 'completed') = (error_code IS NULL))
);
ALTER TABLE enrichedlab_agent.broker_object_receipts ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_object_receipts FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_object_receipts;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_object_receipts
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- Worker: the request of its current, unexpired lease; null for any other task.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_objects(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object('profile', o.profile_key, 'collection', o.collection_id, 'bucket', o.bucket,
    'prefix', o.prefix, 'generation', o.generation, 'manifest_sha256', o.manifest_sha256, 'files', to_jsonb(o.files),
    'request_sha256', o.request_sha256)
  FROM enrichedlab_agent.broker_tasks t
  JOIN enrichedlab_agent.broker_object_requests o USING (tenant_id, task_id)
  WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
    AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running') AND t.desired_status = 'run'
    AND t.lease_expires_at > CURRENT_TIMESTAMP
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_objects(text, uuid, text, integer, bigint) FROM PUBLIC;

-- Worker: one receipt per attempt under its current lease; the same receipt again is accepted, a different one refused.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_object_receipt(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint, p_status text, p_error text, p_files integer, p_bytes bigint, p_tables jsonb,
  p_package_sha256 text, p_started timestamptz, p_finished timestamptz)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v enrichedlab_agent.broker_object_receipts%ROWTYPE;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks t
      JOIN enrichedlab_agent.broker_object_requests o USING (tenant_id, task_id)
      WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
        AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running')
        AND t.lease_expires_at > CURRENT_TIMESTAMP) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_finished < p_started OR p_started > clock_timestamp() + interval '1 minute' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:object_receipt_invalid';
  END IF;
  SELECT * INTO v FROM enrichedlab_agent.broker_object_receipts
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = p_attempt AND lease_generation = p_generation;
  IF FOUND THEN
    IF v.status IS DISTINCT FROM p_status OR v.error_code IS DISTINCT FROM p_error OR v.files IS DISTINCT FROM p_files
      OR v.bytes IS DISTINCT FROM p_bytes OR v.tables IS DISTINCT FROM p_tables
      OR v.package_sha256 IS DISTINCT FROM p_package_sha256 OR v.started_at IS DISTINCT FROM p_started
      OR v.finished_at IS DISTINCT FROM p_finished THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
  ELSE
    INSERT INTO enrichedlab_agent.broker_object_receipts (tenant_id, task_id, attempt, lease_generation, status,
      error_code, files, bytes, tables, package_sha256, started_at, finished_at)
    VALUES (p_tenant, p_task, p_attempt, p_generation, p_status, p_error, p_files, p_bytes, p_tables, p_package_sha256,
      p_started, p_finished)
    RETURNING * INTO v;
  END IF;
  RETURN jsonb_build_object('status', v.status, 'recorded_at', v.recorded_at);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_object_receipt(text, uuid, text, integer, bigint, text, text,
  integer, bigint, jsonb, text, timestamptz, timestamptz) FROM PUBLIC;

-- Coordinator: the run's lab objects methods (enabled, allowed, route `server-objects`), under the run's lease.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_object_methods(p_tenant text, p_run uuid,
  p_worker text, p_generation bigint)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  r enrichedlab_agent.broker_research_runs%ROWTYPE;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  RETURN coalesce((
    SELECT jsonb_agg(f.profile_key ORDER BY f.profile_key)
    FROM enrichedlab_agent.broker_agent_profiles f
    WHERE f.tenant_id = p_tenant AND f.profile_key = ANY (r.allowed_profiles) AND f.enabled
      AND f.route = 'server-objects'), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_object_methods(text, uuid, text, bigint) FROM PUBLIC;

-- The turn commit (migration 072) with the objects branch and its record.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_commit_research_turn(p_tenant text, p_run uuid, p_worker text, p_generation bigint, p_expected bigint, p_state jsonb, p_actions jsonb, p_record jsonb, p_outcome text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'enrichedlab_agent'
AS $function$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project enrichedlab_agent.broker_projects%ROWTYPE;
  tmpl enrichedlab_agent.broker_profile_templates%ROWTYPE;
  a jsonb; code jsonb; task uuid; tasks jsonb:='[]'; plan jsonb; revision bigint; pending integer;
  cfg jsonb; chosen jsonb; rendered text; pick record; fill record; described jsonb;
  src enrichedlab_agent.broker_sql_sources%ROWTYPE; q jsonb;
  ta jsonb; ac jsonb; route text; parent uuid; request jsonb; ob jsonb;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run FOR UPDATE;
  IF NOT FOUND OR r.status<>'running' OR r.lease_owner IS DISTINCT FROM p_worker OR r.generation<>p_generation OR r.lease_until<=clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  SELECT * INTO project FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=r.project_id FOR UPDATE;
  IF NOT enrichedlab_agent.broker_research_authorized(p_tenant,r.actor_id,r.project_id,r.allowed_profiles) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden'; END IF;
  IF project.revision<>p_expected THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  IF r.turn>=r.max_turns OR p_outcome NOT IN ('delegate','review_required','paused','revise')
    OR jsonb_typeof(p_actions) IS DISTINCT FROM 'array' OR jsonb_array_length(p_actions)>r.max_parallel
    OR (p_outcome='delegate') IS DISTINCT FROM (jsonb_array_length(p_actions)>0)
    OR jsonb_typeof(p_record) IS DISTINCT FROM 'object' OR octet_length(p_record::text)>131072 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  -- A revision keeps the proposal it sent back as `revised_proposal`, never as `proposal`, so no
  -- reader of answers takes it for one; a run is sent back at most twice.
  IF p_outcome='revise' AND (p_record ? 'proposal' OR jsonb_typeof(p_record->'revised_proposal') IS DISTINCT FROM 'object') THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  IF p_outcome='revise' AND (SELECT count(*) FROM enrichedlab_agent.broker_research_turns t
      WHERE t.tenant_id=p_tenant AND t.run_id=p_run AND t.record->>'outcome'='revise') >= 2 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:revision_budget_spent'; END IF;
  SELECT count(*) INTO pending FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND t.status IN ('queued','leased','starting','running','cancelling')
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr WHERE fr.tenant_id=pt.tenant_id AND fr.task_id=pt.task_id)
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_dataset_requests dr WHERE dr.tenant_id=pt.tenant_id AND dr.task_id=pt.task_id)
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_document_requests dq WHERE dq.tenant_id=pt.tenant_id AND dq.task_id=pt.task_id);
  IF pending+jsonb_array_length(p_actions)>r.max_parallel THEN RAISE EXCEPTION USING ERRCODE='53300',MESSAGE='broker:group_queue_full'; END IF;
  FOR a IN SELECT value FROM jsonb_array_elements(p_actions) LOOP
    IF (a->>'profile'=ANY(r.allowed_profiles)) IS NOT TRUE
      OR (a->>'action_key' ~ '^[a-zA-Z0-9_-]{1,100}$') IS NOT TRUE
      OR (a->>'request_sha256' ~ '^[0-9a-f]{64}$') IS NOT TRUE OR NOT EXISTS (
        SELECT 1 FROM jsonb_array_elements(p_state->'plan') s WHERE s->>'action_key'=a->>'action_key' AND s->>'status'='pending' AND s->>'task_id' IS NULL
      ) OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=r.project_id AND action_key=a->>'action_key') THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
    code:=a->'generated_code'; cfg:=NULL; described:=NULL; q:=a->'query';
    ta:=a->'template_analysis'; ac:=NULL; parent:=NULL; ob:=a->'objects';
    SELECT f.route INTO route FROM enrichedlab_agent.broker_agent_profiles f WHERE f.tenant_id=p_tenant AND f.profile_key=a->>'profile';
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
    -- A source's method carries the query its guard admitted, and nothing else runs it: no code, and
    -- no query for any other method. The guard ran in the coordinator; the row keeps its text and digest.
    SELECT * INTO src FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id=p_tenant AND profile_key=a->>'profile';
    IF src.source_key IS NOT NULL THEN
      IF code IS NOT NULL OR jsonb_typeof(q) IS DISTINCT FROM 'object'
        OR EXISTS (SELECT 1 FROM jsonb_object_keys(q) k WHERE k NOT IN ('source','proposed_sql','admitted_sql','sha256','guard','relations','row_limit','reviewed_query'))
        OR (q->>'source') IS DISTINCT FROM src.source_key
        OR jsonb_typeof(q->'proposed_sql') IS DISTINCT FROM 'string'
        OR (octet_length(q->>'proposed_sql') BETWEEN 1 AND 8192) IS NOT TRUE
        OR jsonb_typeof(q->'admitted_sql') IS DISTINCT FROM 'string'
        OR (octet_length(q->>'admitted_sql') BETWEEN 1 AND 8300) IS NOT TRUE
        OR (q->>'sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(q->>'admitted_sql','UTF8')),'hex')
        OR (q->>'guard') IS DISTINCT FROM 'guarded-sql/v1'
        OR jsonb_typeof(q->'relations') IS DISTINCT FROM 'array' OR jsonb_array_length(q->'relations')=0
        OR EXISTS (SELECT 1 FROM jsonb_array_elements(q->'relations') x WHERE jsonb_typeof(x) IS DISTINCT FROM 'string'
          OR NOT EXISTS (SELECT 1 FROM jsonb_array_elements(src.relations) s WHERE s->>'name'=x#>>'{}'))
        OR (CASE WHEN (q->>'row_limit') ~ '^[0-9]{1,6}$' THEN (q->>'row_limit')::integer BETWEEN 0 AND src.row_cap ELSE false END) IS NOT TRUE THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:query_invalid'; END IF;
    ELSIF q IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:query_invalid';
    END IF;
    -- The lab's template-analysis method carries the analysis the lead chose and nothing else. The database
    -- runs the checks a person's request runs, as the run's actor, so the lead can name only a completed analysis of this
    -- project, a table its newest summarized digest lists, a feature of the newest catalog and settings it declares.
    IF route='server-analysis' THEN
      -- Three steps fix the order: the key and field tests run only on an object, the cast only on a well-formed id.
      IF code IS NOT NULL OR q IS NOT NULL OR jsonb_typeof(ta) IS DISTINCT FROM 'object' THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:template_analysis_invalid'; END IF;
      IF EXISTS (SELECT 1 FROM jsonb_object_keys(ta) k WHERE k NOT IN ('source_task','artifact','table_path','feature',
          'settings','title','catalog_sha256'))
        OR jsonb_typeof(ta->'source_task') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'artifact') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'table_path') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'feature') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'settings') IS DISTINCT FROM 'object'
        OR jsonb_typeof(ta->'title') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'catalog_sha256') IS DISTINCT FROM 'string' THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:template_analysis_invalid'; END IF;
      IF (ta->>'source_task') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
        OR (ta->>'artifact') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
        OR (ta->>'catalog_sha256') !~ '^[0-9a-f]{64}$' THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:template_analysis_invalid'; END IF;
      -- The table of the result the lead was shown and the catalog it chose from, or the commit is refused.
      ac:=enrichedlab_agent.broker_check_analysis_request(p_tenant,r.actor_id,r.project_id,(ta->>'source_task')::uuid,
        ta->>'table_path',ta->>'feature',ta->'settings',ta->>'title',(ta->>'artifact')::uuid,ta->>'catalog_sha256');
      IF (ac->>'profile') IS DISTINCT FROM (a->>'profile') THEN
        RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:analysis_method_unavailable'; END IF;
      parent:=(ta->>'source_task')::uuid;
    ELSIF ta IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:template_analysis_invalid';
    END IF;
    -- A lab objects method (migration 083) carries the files its step reads from one collection of the lab's bucket and
    -- nothing else: no code, no query, no analysis. The coordinator admitted them against the collection it showed;
    -- here every field is checked for shape, and the worker checks the bucket against its own configuration and every
    -- file against the pinned manifest.
    IF route='server-objects' THEN
      IF code IS NOT NULL OR q IS NOT NULL OR ta IS NOT NULL OR jsonb_typeof(ob) IS DISTINCT FROM 'object' THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:objects_invalid'; END IF;
      IF EXISTS (SELECT 1 FROM jsonb_object_keys(ob) k WHERE k NOT IN ('collection','bucket','prefix','generation',
          'manifest_sha256','files'))
        OR NOT (ob ?& ARRAY['collection','bucket','prefix','generation','manifest_sha256','files'])
        OR EXISTS (SELECT 1 FROM unnest(ARRAY['collection','bucket','prefix','generation','manifest_sha256']) k
          WHERE jsonb_typeof(ob->k) IS DISTINCT FROM 'string')
        OR (ob->>'collection') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
        OR (ob->>'bucket') !~ '^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$'
        OR (ob->>'prefix') !~ '^[a-z0-9_-]{1,64}/[a-z0-9_-]{1,64}/[a-z0-9_-]{1,64}/$'
        OR (ob->>'generation') !~ '^[a-z0-9_-]{1,64}$'
        OR (ob->>'manifest_sha256') !~ '^[0-9a-f]{64}$'
        OR jsonb_typeof(ob->'files') IS DISTINCT FROM 'array'
        OR (jsonb_array_length(ob->'files') BETWEEN 1 AND 20) IS NOT TRUE
        OR EXISTS (SELECT 1 FROM jsonb_array_elements(ob->'files') f WHERE jsonb_typeof(f) IS DISTINCT FROM 'string'
          OR (octet_length(f#>>'{}') BETWEEN 1 AND 1024) IS NOT TRUE OR (f#>>'{}') ~ '[[:cntrl:]]'
          OR (f#>>'{}') ~ '(^|/)\.{1,2}(/|$)' OR (f#>>'{}') ~ '^/' OR (f#>>'{}') ~ '//')
        OR (SELECT count(DISTINCT f#>>'{}') FROM jsonb_array_elements(ob->'files') f) <> jsonb_array_length(ob->'files') THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:objects_invalid'; END IF;
    ELSIF ob IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:objects_invalid';
    END IF;
    task:=enrichedlab_agent.broker_submit_project_task(p_tenant,r.actor_id,r.project_id,a->>'action_key',parent,project.group_key,a->>'profile',a->>'objective',
      'research-'||p_run::text||'-'||(r.turn+1)::text||'-'||(a->>'action_key'),a->>'request_sha256');
    IF src.source_key IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_task_queries(tenant_id,task_id,source_key,profile_key,proposed_sql,admitted_sql,
        sql_sha256,guard,relations,row_limit,run_id,turn,reviewed_query)
        VALUES(p_tenant,task,src.source_key,src.profile_key,q->>'proposed_sql',q->>'admitted_sql',q->>'sha256',q->>'guard',
          ARRAY(SELECT jsonb_array_elements_text(q->'relations')),(q->>'row_limit')::integer,p_run,r.turn+1,q->'reviewed_query');
    END IF;
    IF code IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_task_generated_code(tenant_id,task_id,profile_key,template_ref,template_sha256,path,
        content,size_bytes,sha256,admission,run_id,turn,configuration)
        VALUES(p_tenant,task,a->>'profile',tmpl.template_ref,tmpl.template_sha256,tmpl.block_path,code->>'content',
          octet_length(code->>'content'),code->>'sha256',code->'admission',p_run,r.turn+1,
          CASE WHEN cfg IS NULL THEN NULL ELSE (code->'configuration')||jsonb_build_object('described',described) END);
    END IF;
    IF ob IS NOT NULL THEN
      request:=jsonb_build_object('schema','enrichedlab.lab-objects-request.v1','collection',ob->>'collection',
        'bucket',ob->>'bucket','prefix',ob->>'prefix','generation',ob->>'generation',
        'manifest_sha256',ob->>'manifest_sha256','files',ob->'files',
        'origin',jsonb_build_object('run',p_run,'turn',r.turn+1,'action_key',a->>'action_key'));
      INSERT INTO enrichedlab_agent.broker_object_requests(tenant_id,task_id,profile_key,collection_id,bucket,prefix,
        generation,manifest_sha256,files,request,request_sha256,run_id,turn)
      VALUES(p_tenant,task,a->>'profile',(ob->>'collection')::uuid,ob->>'bucket',ob->>'prefix',ob->>'generation',
        ob->>'manifest_sha256',ARRAY(SELECT jsonb_array_elements_text(ob->'files')),request,
        encode(sha256(convert_to(request::text,'UTF8')),'hex'),p_run,r.turn+1);
    END IF;
    IF ac IS NOT NULL THEN
      request:=jsonb_build_object('schema','enrichedlab.template-analysis-request.v1','project',r.project_id,
        'source_task',parent,'source_package_sha256',ac->>'package','table_path',ta->>'table_path',
        'table_sha256',ac->>'table_sha256','feature',ta->>'feature','settings',ta->'settings','title',ac->>'title',
        'previous',NULL,'catalog_sha256',ac->>'catalog_sha256',
        'origin',jsonb_build_object('run',p_run,'turn',r.turn+1,'action_key',a->>'action_key'));
      INSERT INTO enrichedlab_agent.broker_analysis_requests (tenant_id,task_id,project_id,source_task_id,
        source_artifact_id,source_package_sha256,table_path,table_sha256,table_rows,table_columns,feature,settings,
        title,previous_task_id,catalog_sha256,request,request_sha256,principal_id)
      VALUES (p_tenant,task,r.project_id,parent,(ac->>'artifact')::uuid,ac->>'package',ta->>'table_path',
        ac->>'table_sha256',(ac->>'rows')::bigint,ac->'columns',ta->>'feature',ta->'settings',ac->>'title',NULL,
        ac->>'catalog_sha256',request,encode(sha256(convert_to(request::text,'UTF8')),'hex'),r.actor_id);
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
    status=CASE WHEN p_outcome='delegate' THEN 'waiting' WHEN p_outcome='revise' THEN 'queued' ELSE p_outcome END,
    code=CASE WHEN p_outcome='delegate' THEN 'waiting_for_tools' ELSE p_outcome END,
    available_at=clock_timestamp()+interval '5 seconds',lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=p_run;
  RETURN jsonb_build_object('revision',revision,'tasks',tasks,'turn',r.turn+1);
END $function$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_commit_research_turn(text,uuid,text,bigint,bigint,jsonb,jsonb,jsonb,text) FROM PUBLIC;

-- One method as the lab's tools list shows it (migration 080), now naming the lab objects kind.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_lab_tool_json(
  p_tenant text, p_actor uuid, p_group text, p enrichedlab_agent.broker_agent_profiles)
RETURNS jsonb LANGUAGE sql STABLE SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT jsonb_strip_nulls(jsonb_build_object(
    'profile', p.profile_key,
    'name', p.name,
    'description', p.description,
    'route', p.route,
    'kind', CASE
      WHEN p.route = 'server-query' THEN 'query_method'
      WHEN p.route = 'server-document' THEN 'document_method'
      WHEN p.route = 'server-objects' THEN 'objects_method'
      WHEN p.route = 'server-render' THEN 'render_method'
      WHEN p.route = 'server-analysis' THEN 'server_analysis'
      WHEN t.profile_key IS NOT NULL AND jsonb_typeof(t.contract->'configuration') = 'object' THEN 'configured_method'
      WHEN t.profile_key IS NOT NULL THEN 'template_analysis'
      ELSE 'sandbox_method' END,
    -- Granted directly by the catalog, through these tool groups, or both.
    'via_groups', (
      SELECT coalesce(jsonb_agg(DISTINCT gr.tool_group_key ORDER BY gr.tool_group_key), '[]'::jsonb)
        FROM enrichedlab_agent.broker_tool_group_grants gr
        JOIN enrichedlab_agent.broker_tool_group_members m
          ON m.tenant_id = gr.tenant_id AND m.tool_group_key = gr.tool_group_key AND m.version = gr.version
       WHERE gr.tenant_id = p_tenant AND gr.group_key = p_group AND gr.revoked_at IS NULL
         AND m.profile_key = p.profile_key),
    'source', (SELECT s.source_key FROM enrichedlab_agent.broker_sql_sources s
                WHERE s.tenant_id = p.tenant_id AND s.profile_key = p.profile_key ORDER BY s.source_key LIMIT 1),
    'template', CASE WHEN t.profile_key IS NOT NULL
      THEN jsonb_build_object('ref', t.template_ref, 'sha256', t.template_sha256) END,
    'configuration', CASE WHEN jsonb_typeof(t.contract->'configuration') = 'object' THEN jsonb_build_object(
      'block_sha256', CASE WHEN jsonb_typeof(t.contract->'configuration'->'block_sha256') = 'string'
                        THEN t.contract->'configuration'->'block_sha256' END,
      'settings', (
        SELECT coalesce(jsonb_agg(jsonb_strip_nulls(jsonb_build_object(
                 'name', setting.key,
                 'label', CASE WHEN jsonb_typeof(setting.value->'label') = 'string' THEN setting.value->'label' END,
                 'choices', (
                   SELECT coalesce(jsonb_agg(jsonb_strip_nulls(jsonb_build_object('key', choice.key,
                            'label', CASE WHEN jsonb_typeof(choice.value->'label') = 'string'
                                       THEN choice.value->'label' END)) ORDER BY choice.key), '[]'::jsonb)
                     FROM jsonb_each(CASE WHEN jsonb_typeof(setting.value->'choices') = 'object'
                            THEN setting.value->'choices' ELSE '{}'::jsonb END) AS choice))) ORDER BY setting.key),
                 '[]'::jsonb)
          FROM jsonb_each(CASE WHEN jsonb_typeof(t.contract->'configuration'->'settings') = 'object'
                 THEN t.contract->'configuration'->'settings' ELSE '{}'::jsonb END) AS setting)) END,
    -- The newest version of each reviewed query whose source is this method's, while it is enabled.
    'reviewed_queries', (
      SELECT jsonb_agg(jsonb_strip_nulls(jsonb_build_object(
               'key', q.tool_key, 'version', q.version, 'source', q.source_key,
               'title', CASE WHEN jsonb_typeof(q.definition->'title') = 'string' THEN q.definition->'title' END,
               'purpose', CASE WHEN jsonb_typeof(q.definition->'purpose') = 'string' THEN q.definition->'purpose' END,
               'parameters', (
                 SELECT coalesce(jsonb_agg(jsonb_strip_nulls(jsonb_build_object(
                          'name', CASE WHEN jsonb_typeof(item->'name') = 'string' THEN item->'name' END,
                          'description', CASE WHEN jsonb_typeof(item->'description') = 'string'
                                           THEN item->'description' END,
                          'type', CASE WHEN item->>'type' IN ('text', 'integer', 'number', 'enum') THEN item->'type' END,
                          'required', CASE WHEN jsonb_typeof(item->'required') = 'boolean' THEN item->'required' END,
                          'max_length', CASE WHEN jsonb_typeof(item->'max_length') = 'number' THEN item->'max_length' END,
                          'minimum', CASE WHEN jsonb_typeof(item->'minimum') = 'number' THEN item->'minimum' END,
                          'maximum', CASE WHEN jsonb_typeof(item->'maximum') = 'number' THEN item->'maximum' END,
                          'choices', CASE WHEN jsonb_typeof(item->'choices') = 'array' THEN (
                            SELECT jsonb_agg(c ORDER BY n) FROM jsonb_array_elements(item->'choices')
                              WITH ORDINALITY AS x(c, n) WHERE jsonb_typeof(c) = 'string') END)) ORDER BY n),
                          '[]'::jsonb)
                   FROM jsonb_array_elements(CASE WHEN jsonb_typeof(q.definition->'parameters') = 'array'
                          THEN q.definition->'parameters' ELSE '[]'::jsonb END) WITH ORDINALITY AS pa(item, n)),
               'output_columns', (
                 SELECT coalesce(jsonb_agg(jsonb_strip_nulls(jsonb_build_object(
                          'name', CASE WHEN jsonb_typeof(item->'name') = 'string' THEN item->'name' END,
                          'type', CASE WHEN jsonb_typeof(item->'type') = 'string' THEN item->'type' END,
                          'description', CASE WHEN jsonb_typeof(item->'description') = 'string'
                                           THEN item->'description' END)) ORDER BY n), '[]'::jsonb)
                   FROM jsonb_array_elements(CASE WHEN jsonb_typeof(q.definition->'output_columns') = 'array'
                          THEN q.definition->'output_columns' ELSE '[]'::jsonb END) WITH ORDINALITY AS oc(item, n)),
               'row_cap', CASE WHEN jsonb_typeof(q.definition->'row_cap') = 'number'
                                AND (q.definition->>'row_cap') ~ '^[0-9]{1,9}$' THEN q.definition->'row_cap' END,
               'template_sha256', CASE WHEN q.definition->>'template_sha256' ~ '^sha256:[0-9a-f]{64}$'
                                    THEN q.definition->'template_sha256' END)) ORDER BY q.tool_key)
        FROM (SELECT DISTINCT ON (r.tool_key) r.tool_key, r.version, r.source_key, r.definition
                FROM enrichedlab_agent.broker_reviewed_queries r
               WHERE r.tenant_id = p.tenant_id
               ORDER BY r.tool_key, r.version DESC) q
        JOIN enrichedlab_agent.broker_sql_sources s
          ON s.tenant_id = p.tenant_id AND s.source_key = q.source_key AND s.profile_key = p.profile_key
       WHERE q.definition->>'state' = 'enabled'),
    -- The newest three finished analyses of the method, in this lab's projects the reader can open, whose newest
    -- review a person accepted.
    'examples', (
      SELECT coalesce(jsonb_agg(jsonb_build_object('task', e.task_id, 'project', e.project_id,
               'finished_at', e.finished_at) ORDER BY e.finished_at DESC, e.task_id), '[]'::jsonb)
        FROM (SELECT tk.task_id, pt.project_id, tk.finished_at
                FROM enrichedlab_agent.broker_project_tasks pt
                JOIN enrichedlab_agent.broker_projects pr
                  ON pr.tenant_id = pt.tenant_id AND pr.project_id = pt.project_id
                JOIN enrichedlab_agent.broker_tasks tk ON tk.tenant_id = pt.tenant_id AND tk.task_id = pt.task_id
               WHERE pt.tenant_id = p_tenant AND pr.group_key = p_group
                 AND tk.profile_key = p.profile_key AND tk.status = 'completed' AND tk.finished_at IS NOT NULL
                 AND enrichedlab_agent.broker_project_can(p_tenant, p_actor, pt.project_id, 'read')
                 AND (SELECT rv.decision FROM enrichedlab_agent.broker_task_reviews rv
                       WHERE rv.tenant_id = tk.tenant_id AND rv.task_id = tk.task_id
                       ORDER BY rv.created_at DESC LIMIT 1) = 'accepted'
               ORDER BY tk.finished_at DESC, tk.task_id LIMIT 3) e)))
    FROM (SELECT 1) one
    LEFT JOIN enrichedlab_agent.broker_profile_templates t
      ON t.tenant_id = p.tenant_id AND t.profile_key = p.profile_key
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_lab_tool_json(text, uuid, text, enrichedlab_agent.broker_agent_profiles)
  FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (83) ON CONFLICT DO NOTHING;
COMMIT;
