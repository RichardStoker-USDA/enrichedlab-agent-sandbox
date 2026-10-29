-- Reviewed SELECT templates. Owner registration checks shape here and admission in Rust;
-- proposal and worker admission remain mandatory even for a row an owner stored directly.
BEGIN;
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_reviewed_queries (
  tenant_id text NOT NULL,
  tool_key text NOT NULL CHECK (tool_key ~ '^[a-z][a-z0-9-]{2,63}$'),
  version integer NOT NULL CHECK (version > 0),
  source_key text NOT NULL,
  definition jsonb NOT NULL CHECK (jsonb_typeof(definition)='object' AND octet_length(definition::text)<=65536),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id,tool_key,version),
  FOREIGN KEY (tenant_id,source_key) REFERENCES enrichedlab_agent.broker_sql_sources(tenant_id,source_key)
);
ALTER TABLE enrichedlab_agent.broker_reviewed_queries ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_reviewed_queries FROM PUBLIC;
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_reviewed_query_immutable()
RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF TG_OP='UPDATE' AND (to_jsonb(NEW)-'definition')=(to_jsonb(OLD)-'definition')
    AND (NEW.definition-'state')=(OLD.definition-'state')
    AND OLD.definition->>'state'='enabled' AND NEW.definition->>'state'='retired' THEN RETURN NEW; END IF;
  RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:immutable_record';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_reviewed_query_immutable() FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_reviewed_queries;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_reviewed_queries
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_reviewed_query_immutable();

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_reviewed_parameter_valid(p jsonb)
RETURNS boolean LANGUAGE plpgsql IMMUTABLE SET search_path=pg_catalog AS $$
DECLARE v jsonb; lo numeric; hi numeric;
BEGIN
  IF jsonb_typeof(p) IS DISTINCT FROM 'object' OR jsonb_typeof(p->'name') IS DISTINCT FROM 'string'
    OR (p->>'name' ~ '^[a-z][a-z0-9_]{0,62}$') IS NOT TRUE
    OR jsonb_typeof(p->'description') IS DISTINCT FROM 'string'
    OR (char_length(p->>'description') BETWEEN 1 AND 300) IS NOT TRUE OR p->>'description' ~ '[[:cntrl:]]'
    OR jsonb_typeof(p->'required') IS DISTINCT FROM 'boolean'
    OR (p->>'required'='true')=(p ? 'default')
    OR EXISTS (SELECT 1 FROM jsonb_object_keys(p) k WHERE k NOT IN ('name','description','required','default','type',
      'max_length','minimum','maximum','choices')) THEN RETURN false; END IF;
  IF p->>'type'='text' THEN
    IF p ?| ARRAY['minimum','maximum','choices'] OR jsonb_typeof(p->'max_length') IS DISTINCT FROM 'number'
      OR (p->>'max_length' ~ '^[0-9]{1,3}$') IS NOT TRUE OR (p->>'max_length')::integer NOT BETWEEN 1 AND 256 THEN RETURN false; END IF;
  ELSIF p->>'type' IN ('integer','number') THEN
    IF p ?| ARRAY['max_length','choices'] OR jsonb_typeof(p->'minimum') IS DISTINCT FROM 'number'
      OR jsonb_typeof(p->'maximum') IS DISTINCT FROM 'number' THEN RETURN false; END IF;
    lo:=(p->>'minimum')::numeric; hi:=(p->>'maximum')::numeric;
    IF lo>hi OR lo < -1.7976931348623157e308 OR hi > 1.7976931348623157e308 THEN RETURN false; END IF;
    IF p->>'type'='integer' AND ((p->>'minimum' ~ '^-?[0-9]+$') IS NOT TRUE
      OR (p->>'maximum' ~ '^-?[0-9]+$') IS NOT TRUE OR lo<>trunc(lo) OR hi<>trunc(hi)
      OR lo < -9223372036854775808 OR hi > 9223372036854775807) THEN RETURN false; END IF;
  ELSIF p->>'type'='enum' THEN
    IF p ?| ARRAY['max_length','minimum','maximum'] OR jsonb_typeof(p->'choices') IS DISTINCT FROM 'array' THEN RETURN false; END IF;
    IF jsonb_array_length(p->'choices') NOT BETWEEN 1 AND 64
      OR (SELECT count(DISTINCT c) FROM jsonb_array_elements(p->'choices') c)<>jsonb_array_length(p->'choices') THEN RETURN false; END IF;
    FOR v IN SELECT value FROM jsonb_array_elements(p->'choices') LOOP
      IF jsonb_typeof(v)<>'string' OR char_length(v#>>'{}') NOT BETWEEN 1 AND 256
        OR v#>>'{}' ~ '[[:cntrl:]]' OR normalize(v#>>'{}',NFC)<>v#>>'{}' THEN RETURN false; END IF;
    END LOOP;
  ELSE RETURN false; END IF;
  IF p ? 'default' THEN
    v:=p->'default';
    IF p->>'type'='text' THEN
      IF jsonb_typeof(v)<>'string' OR v#>>'{}' ~ '[[:cntrl:]]'
        OR char_length(normalize(v#>>'{}',NFC))>(p->>'max_length')::integer THEN RETURN false; END IF;
    ELSIF p->>'type'='enum' THEN
      IF NOT (p->'choices' @> jsonb_build_array(v)) THEN RETURN false; END IF;
    ELSE
      IF jsonb_typeof(v)<>'number' OR (v#>>'{}')::numeric NOT BETWEEN lo AND hi
        OR (p->>'type'='integer' AND ((v#>>'{}' ~ '^-?[0-9]+$') IS NOT TRUE OR (v#>>'{}')::numeric<>trunc((v#>>'{}')::numeric))) THEN RETURN false; END IF;
    END IF;
  END IF;
  RETURN true;
EXCEPTION WHEN data_exception THEN RETURN false;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_reviewed_parameter_valid(jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_register_reviewed_query(p_tenant text,p jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE cap integer; prior jsonb;
BEGIN
  IF jsonb_typeof(p) IS DISTINCT FROM 'object' OR octet_length(p::text)>65536 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:reviewed_query_invalid'; END IF;
  SELECT row_cap INTO cap FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id=p_tenant AND source_key=p->>'source';
  IF cap IS NULL OR EXISTS (SELECT 1 FROM jsonb_object_keys(p) k WHERE k NOT IN ('key','version','source','title','purpose',
      'parameters','output_columns','row_cap','state','template','template_sha256'))
    OR (p->>'key' ~ '^[a-z][a-z0-9-]{2,63}$') IS NOT TRUE
    OR jsonb_typeof(p->'version') IS DISTINCT FROM 'number' OR (p->>'version' ~ '^[0-9]{1,10}$') IS NOT TRUE
    OR (p->>'version')::numeric NOT BETWEEN 1 AND 2147483647 OR jsonb_typeof(p->'title') IS DISTINCT FROM 'string'
    OR (char_length(p->>'title') BETWEEN 1 AND 120) IS NOT TRUE OR p->>'title' ~ '[[:cntrl:]]'
    OR jsonb_typeof(p->'purpose') IS DISTINCT FROM 'string' OR (char_length(p->>'purpose') BETWEEN 1 AND 1000) IS NOT TRUE
    OR p->>'purpose' ~ '[[:cntrl:]]' OR p->>'state' IS DISTINCT FROM 'enabled'
    OR jsonb_typeof(p->'row_cap') IS DISTINCT FROM 'number' OR (p->>'row_cap' ~ '^[0-9]{1,6}$') IS NOT TRUE
    OR (p->>'row_cap')::integer NOT BETWEEN 1 AND cap
    OR jsonb_typeof(p->'template') IS DISTINCT FROM 'string' OR (octet_length(p->>'template') BETWEEN 1 AND 8192) IS NOT TRUE
    OR (p->>'template_sha256') IS DISTINCT FROM encode(sha256(convert_to(p->>'template','UTF8')),'hex')
    OR jsonb_typeof(p->'parameters') IS DISTINCT FROM 'array' OR jsonb_typeof(p->'output_columns') IS DISTINCT FROM 'array' THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:reviewed_query_invalid'; END IF;
  IF jsonb_array_length(p->'parameters')>16 OR jsonb_array_length(p->'output_columns') NOT BETWEEN 1 AND 64
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(p->'parameters') v WHERE NOT enrichedlab_agent.broker_reviewed_parameter_valid(v))
    OR (SELECT count(DISTINCT v->>'name') FROM jsonb_array_elements(p->'parameters') v)<>jsonb_array_length(p->'parameters')
    OR (SELECT count(DISTINCT v->>'name') FROM jsonb_array_elements(p->'output_columns') v)<>jsonb_array_length(p->'output_columns') THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:reviewed_query_invalid'; END IF;
  FOR prior IN SELECT value FROM jsonb_array_elements(p->'output_columns') LOOP
    IF jsonb_typeof(prior) IS DISTINCT FROM 'object' THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:reviewed_query_invalid'; END IF;
    IF EXISTS (SELECT 1 FROM jsonb_object_keys(prior) k WHERE k NOT IN ('name','type','description'))
      OR jsonb_typeof(prior->'name') IS DISTINCT FROM 'string' OR (prior->>'name' ~ '^[a-z][a-z0-9_]{0,62}$') IS NOT TRUE
      OR jsonb_typeof(prior->'type') IS DISTINCT FROM 'string' OR (char_length(prior->>'type') BETWEEN 1 AND 64) IS NOT TRUE
      OR prior->>'type' ~ '[[:cntrl:]]' OR jsonb_typeof(prior->'description') IS DISTINCT FROM 'string'
      OR (char_length(prior->>'description') BETWEEN 1 AND 300) IS NOT TRUE OR prior->>'description' ~ '[[:cntrl:]]' THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:reviewed_query_invalid'; END IF;
  END LOOP;
  INSERT INTO enrichedlab_agent.broker_reviewed_queries(tenant_id,tool_key,version,source_key,definition)
    VALUES(p_tenant,p->>'key',(p->>'version')::integer,p->>'source',p) ON CONFLICT DO NOTHING;
  SELECT definition INTO prior FROM enrichedlab_agent.broker_reviewed_queries
    WHERE tenant_id=p_tenant AND tool_key=p->>'key' AND version=(p->>'version')::integer;
  IF (prior-'state') IS DISTINCT FROM (p-'state') THEN
    RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:idempotency_conflict'; END IF;
  RETURN jsonb_build_object('key',prior->'key','version',prior->'version','state',prior->'state');
EXCEPTION WHEN data_exception THEN
  RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:reviewed_query_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_register_reviewed_query(text,jsonb) FROM PUBLIC;

ALTER TABLE enrichedlab_agent.broker_task_queries ADD COLUMN IF NOT EXISTS reviewed_query jsonb
  CHECK (reviewed_query IS NULL OR (jsonb_typeof(reviewed_query)='object' AND octet_length(reviewed_query::text)<=65536));

-- Query custody must match the immutable registry version; retirement prevents new commits, not reruns.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_reviewed_query_custody()
RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r jsonb:=NEW.reviewed_query; d jsonb;
BEGIN
  IF r IS NULL THEN RETURN NEW; END IF;
  SELECT definition INTO d FROM enrichedlab_agent.broker_reviewed_queries
    WHERE tenant_id=NEW.tenant_id AND source_key=NEW.source_key AND tool_key=r->>'tool'
      AND version::text=r->>'version' AND definition->>'state'='enabled';
  IF d IS NULL OR jsonb_typeof(r) IS DISTINCT FROM 'object'
    OR EXISTS (SELECT 1 FROM jsonb_object_keys(r) k WHERE k NOT IN ('tool','version','template','template_sha256',
      'schema_sha256','parameters','requested','normalized','row_cap'))
    OR r->'template' IS DISTINCT FROM d->'template' OR r->'template_sha256' IS DISTINCT FROM d->'template_sha256'
    OR r->'parameters' IS DISTINCT FROM d->'parameters' OR r->'row_cap' IS DISTINCT FROM d->'row_cap'
    OR (r->>'schema_sha256' ~ '^[0-9a-f]{64}$') IS NOT TRUE
    OR jsonb_typeof(r->'requested') IS DISTINCT FROM 'object' OR jsonb_typeof(r->'normalized') IS DISTINCT FROM 'object'
    OR NEW.row_limit>(d->>'row_cap')::integer THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:reviewed_query_invalid'; END IF;
  RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_reviewed_query_custody() FROM PUBLIC;
DROP TRIGGER IF EXISTS reviewed_query_custody ON enrichedlab_agent.broker_task_queries;
CREATE TRIGGER reviewed_query_custody BEFORE INSERT ON enrichedlab_agent.broker_task_queries
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_reviewed_query_custody();

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_sql_sources(p_tenant text, p_run uuid, p_worker text,
  p_generation bigint)
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
    SELECT jsonb_agg(jsonb_build_object('profile', s.profile_key, 'source', s.source_key, 'title', s.title,
      'description', s.description, 'release', s.release, 'relations', s.relations, 'functions', to_jsonb(s.functions),
      'row_cap', s.row_cap, 'schema', s.schema_name,
        'reviewed_tools',coalesce((SELECT jsonb_agg(t.definition ORDER BY t.tool_key,t.version DESC)
          FROM enrichedlab_agent.broker_reviewed_queries t WHERE t.tenant_id=s.tenant_id AND t.source_key=s.source_key
            AND t.definition->>'state'='enabled'),'[]'::jsonb))
      || coalesce((SELECT jsonb_build_object('card', c.card, 'card_sha256', c.card_sha256)
        FROM enrichedlab_agent.broker_sql_source_cards c
        WHERE c.tenant_id = s.tenant_id AND c.source_key = s.source_key
          AND c.manifest_sha256 = enrichedlab_agent.broker_sql_source_card_key(s.snapshot)
        ORDER BY c.created_at DESC, c.card_sha256 LIMIT 1), '{}'::jsonb)
      ORDER BY s.profile_key)
    FROM enrichedlab_agent.broker_sql_sources s
    WHERE s.tenant_id = p_tenant AND s.profile_key = ANY (r.allowed_profiles)), '[]'::jsonb);
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_query(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object('source', s.source_key, 'title', s.title, 'release', s.release, 'schema', s.schema_name,
    'relations', (SELECT jsonb_agg(x->>'name' ORDER BY x->>'name') FROM jsonb_array_elements(s.relations) x),
    'functions', to_jsonb(s.functions), 'row_cap', s.row_cap, 'statement_timeout_ms', s.statement_timeout_ms,
    'connection_ref', s.connection_ref, 'snapshot', s.snapshot, 'proposed_sql', q.proposed_sql,
    'admitted_sql', q.admitted_sql, 'sql_sha256', q.sql_sha256, 'guard', q.guard, 'row_limit', q.row_limit, 'reviewed_query', q.reviewed_query)
  FROM enrichedlab_agent.broker_tasks t
  JOIN enrichedlab_agent.broker_task_queries q USING (tenant_id, task_id)
  JOIN enrichedlab_agent.broker_sql_sources s ON s.tenant_id = q.tenant_id AND s.source_key = q.source_key
  WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
    AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running') AND t.desired_status = 'run'
    AND t.lease_expires_at > CURRENT_TIMESTAMP
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_query_detail(p_tenant text, p_actor uuid, p_task uuid)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks WHERE tenant_id = p_tenant AND task_id = p_task
      AND enrichedlab_agent.broker_task_can(p_tenant, p_actor, p_task, 'read')) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  RETURN (SELECT jsonb_build_object('task_id', q.task_id, 'profile', q.profile_key, 'source', s.source_key,
      'title', s.title, 'description', s.description, 'release', s.release, 'snapshot', s.snapshot,
      'data_class', s.data_class,
      'proposed_sql', q.proposed_sql, 'admitted_sql', q.admitted_sql, 'sql_sha256', q.sql_sha256, 'guard', q.guard,
      'relations', to_jsonb(q.relations), 'row_limit', q.row_limit, 'run_id', q.run_id, 'turn', q.turn,
      'reviewed_query', q.reviewed_query, 'created_at', q.created_at, 'validation', 'unvalidated',
      'receipts', coalesce((SELECT jsonb_agg(jsonb_build_object('attempt', r.attempt, 'status', r.status,
          'error_code', r.error_code, 'row_count', r.row_count, 'columns', r.columns, 'package_sha256', r.package_sha256,
          'server_version', r.server_version, 'source_version', r.source_version, 'started_at', r.started_at,
          'finished_at', r.finished_at)
          ORDER BY r.attempt, r.lease_generation)
        FROM enrichedlab_agent.broker_query_receipts r WHERE r.tenant_id = q.tenant_id AND r.task_id = q.task_id), '[]'::jsonb))
    FROM enrichedlab_agent.broker_task_queries q
    JOIN enrichedlab_agent.broker_sql_sources s ON s.tenant_id = q.tenant_id AND s.source_key = q.source_key
    WHERE q.tenant_id = p_tenant AND q.task_id = p_task);
END;
$$;

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
  ta jsonb; ac jsonb; route text; parent uuid; request jsonb;
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
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_dataset_requests dr WHERE dr.tenant_id=pt.tenant_id AND dr.task_id=pt.task_id);
  IF pending+jsonb_array_length(p_actions)>r.max_parallel THEN RAISE EXCEPTION USING ERRCODE='53300',MESSAGE='broker:group_queue_full'; END IF;
  FOR a IN SELECT value FROM jsonb_array_elements(p_actions) LOOP
    IF (a->>'profile'=ANY(r.allowed_profiles)) IS NOT TRUE
      OR (a->>'action_key' ~ '^[a-zA-Z0-9_-]{1,100}$') IS NOT TRUE
      OR (a->>'request_sha256' ~ '^[0-9a-f]{64}$') IS NOT TRUE OR NOT EXISTS (
        SELECT 1 FROM jsonb_array_elements(p_state->'plan') s WHERE s->>'action_key'=a->>'action_key' AND s->>'status'='pending' AND s->>'task_id' IS NULL
      ) OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=r.project_id AND action_key=a->>'action_key') THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
    code:=a->'generated_code'; cfg:=NULL; described:=NULL; q:=a->'query';
    ta:=a->'template_analysis'; ac:=NULL; parent:=NULL;
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
  ar enrichedlab_agent.broker_analysis_requests%ROWTYPE; ax enrichedlab_agent.broker_analysis_receipts%ROWTYPE;
  dr enrichedlab_agent.broker_dataset_requests%ROWTYPE; dx enrichedlab_agent.broker_dataset_receipts%ROWTYPE;
  ds enrichedlab_agent.broker_datasets%ROWTYPE;
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
  SELECT * INTO ar FROM enrichedlab_agent.broker_analysis_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF ar.task_id IS NOT NULL THEN
    SELECT * INTO ax FROM enrichedlab_agent.broker_analysis_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
  END IF;
  SELECT * INTO dr FROM enrichedlab_agent.broker_dataset_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF dr.task_id IS NOT NULL THEN
    SELECT * INTO dx FROM enrichedlab_agent.broker_dataset_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
    SELECT * INTO ds FROM enrichedlab_agent.broker_datasets WHERE tenant_id=p_tenant AND dataset_id=dr.dataset_id;
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
  ELSIF fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL OR dr.task_id IS NOT NULL THEN
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
  ELSIF ar.task_id IS NOT NULL THEN
    classes:='["prior_output"]'::jsonb; classes_by:='analysis_request';
  ELSIF dr.task_id IS NOT NULL THEN
    -- An intake reads the file a person sent, as the guard rebuilt it; a preparation reads the intake's kept grid.
    classes:=CASE WHEN dr.operation='intake' THEN '["object_data"]'::jsonb ELSE '["prior_output"]'::jsonb END;
    classes_by:='dataset_request';
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
  tool_class:=CASE WHEN fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL OR dr.task_id IS NOT NULL THEN 'vetted_service'
    WHEN q.reviewed_query IS NOT NULL THEN 'reviewed_query' WHEN q.task_id IS NOT NULL THEN 'guarded_query'
    WHEN g.task_id IS NOT NULL AND g.configuration IS NOT NULL THEN 'configured'
    WHEN g.task_id IS NOT NULL THEN 'agent_built' WHEN tool IS NOT NULL THEN 'vetted_service'
    WHEN has_template THEN 'agent_built' ELSE 'agent_session' END;
  IF has_template AND g.task_id IS NULL AND tool IS NULL THEN gaps:=gaps||'"code_not_recorded"'::jsonb; END IF;
  output_class:=CASE tool_class WHEN 'agent_built' THEN 'agent_built_tool_output' WHEN 'vetted_service' THEN 'vetted_tool_output'
    WHEN 'configured' THEN 'configured_tool_output' WHEN 'reviewed_query' THEN 'query_output' WHEN 'guarded_query' THEN 'query_output' ELSE 'agent_session_output' END;
  -- A figure version or a template analysis ran in the render service, so the image it names is the one its newest
  -- receipt records, and only a completed one's receipt must name one (a refusal comes back before anything runs); any
  -- other task ran in the image its profile named.
  unpinned:=CASE WHEN fr.task_id IS NOT NULL THEN fx.status='completed' AND fx.image_digest IS NULL
    WHEN ar.task_id IS NOT NULL THEN ax.status='completed' AND ax.image_digest IS NULL
    ELSE coalesce(t.profile_snapshot->>'image_ref','') !~ '@sha256:[0-9a-f]{64}$' END;
  IF unpinned THEN
    gaps:=gaps||'"image_not_digest_pinned"'::jsonb; END IF;

  SELECT coalesce(jsonb_agg(jsonb_build_object('artifact_id',a.artifact_id,'role',a.role,'label',a.label,
      'relative_path',a.relative_path,'media_type',a.media_type,'size_bytes',a.size_bytes,'sha256',a.sha256,
      'created_at',a.created_at,'provenance_class',output_class) ORDER BY a.created_at,a.artifact_id),'[]'::jsonb)
    INTO outputs FROM enrichedlab_agent.broker_artifacts a
    WHERE a.tenant_id=p_tenant AND a.task_id=p_task AND a.visibility='researcher';
  -- A figure version or template analysis that did not complete has no files by design: the service refused its
  -- settings or R failed, and its receipt says so. Only a completed one without its files is a gap.
  IF jsonb_array_length(outputs)=0 AND (CASE WHEN fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL OR dr.task_id IS NOT NULL
      THEN t.status='completed'
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
      'reviewed_query',q.reviewed_query,'guard',q.guard,'relations',to_jsonb(q.relations),'row_limit',q.row_limit,'run_id',q.run_id,'turn',q.turn,
      'created_at',q.created_at,'title',src.title,'release',src.release,
      'snapshot_sha256',src.snapshot->>'manifest_sha256','data_class',src.data_class,
      'snapshot_kind',src.snapshot->>'kind','source_version',(SELECT r.source_version FROM enrichedlab_agent.broker_query_receipts r
        WHERE r.tenant_id=p_tenant AND r.task_id=p_task AND r.status='completed'
        ORDER BY r.attempt DESC,r.lease_generation DESC LIMIT 1)) END,
    'render',CASE WHEN fr.task_id IS NULL THEN NULL ELSE jsonb_build_object('source_task',fr.source_task_id,
      'table_path',fr.table_path,'table_sha256',fr.table_sha256,'source_package_sha256',fr.source_package_sha256,
      'feature',fr.feature,'controls',fr.controls,'title',fr.title,'previous',fr.previous_task_id,
      'request_sha256',fr.request_sha256,'created_at',fr.created_at,'service_version',fx.service_version,
      'image_digest',fx.image_digest) END,
    'analysis',CASE WHEN ar.task_id IS NULL THEN NULL ELSE jsonb_build_object('source_task',ar.source_task_id,
      'table_path',ar.table_path,'table_sha256',ar.table_sha256,'source_package_sha256',ar.source_package_sha256,
      'feature',ar.feature,'settings',ar.settings,'title',ar.title,'previous',ar.previous_task_id,
      'request_sha256',ar.request_sha256,'created_at',ar.created_at,'service_version',ax.service_version,
      'image_digest',ax.image_digest,'method',ax.method) END,
    'dataset',CASE WHEN dr.task_id IS NULL THEN NULL ELSE jsonb_build_object('dataset',dr.dataset_id,
      'operation',dr.operation,'file_name',ds.file_name,'file_sha256',ds.sha256,'guard_intake',ds.guard_intake,
      'guard_policy',ds.guard_policy,'grid_sha256',ds.grid_sha256,'spec_sha256',dr.spec_sha256,
      'previous',dr.previous_task_id,'title',dr.title,'request_sha256',dr.request_sha256,'created_at',dr.created_at,
      'executor',dx.executor,'executor_version',dx.executor_version,'table_sha256',dx.table_sha256) END,
    'execution',jsonb_build_object('image_ref',t.profile_snapshot->'image_ref','cpu_limit',t.profile_snapshot->'cpu_limit',
      'memory_limit',t.profile_snapshot->'memory_limit','max_run_seconds',t.profile_snapshot->'max_run_seconds',
      'attempts',t.attempt,'scientific_history',(SELECT count(*) FROM enrichedlab_agent.broker_task_scientific_history h
        WHERE h.tenant_id=p_tenant AND h.task_id=p_task)),
    'outputs',outputs,
    'review',jsonb_build_object('state',CASE WHEN tool_class IN ('agent_built','agent_session','configured','guarded_query','reviewed_query') THEN 'unvalidated' ELSE 'not_reviewed' END),
    'gaps',gaps);
END $function$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_sql_sources(text,uuid,text,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_query(text,uuid,text,integer,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_query_detail(text,uuid,uuid) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_commit_research_turn(text,uuid,text,bigint,bigint,jsonb,jsonb,jsonb,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_lineage(text,uuid,uuid) FROM PUBLIC;
-- Existing coordinator, worker and API signatures keep their existing role grants. Registration stays owner-only.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (71) ON CONFLICT DO NOTHING;
COMMIT;
