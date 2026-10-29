-- Figure versions. A person who may contribute to a project asks for a figure from one result table
-- of a finished analysis in it. The request is recorded with the task it creates, in one transaction, under the
-- project's figure method (route `server-render`). The worker's render route reads the table from the retained package
-- the request names, has the reviewed R render service draw it, and records one receipt per attempt. The worker is
-- the only broker component that reaches the render service, so it also keeps the service's catalog of plot features
-- and their settings here for the API and the workspace's figure editor. Each edit is a new version; nothing is
-- overwritten.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_render_catalogs (
  tenant_id text NOT NULL REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  catalog_sha256 text NOT NULL,
  service_version text NOT NULL,
  image_digest text,
  features jsonb NOT NULL,
  first_seen_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  last_seen_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, catalog_sha256),
  CHECK (catalog_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (service_version ~ '^[0-9A-Za-z][0-9A-Za-z.+-]{0,31}$'),
  CHECK (image_digest IS NULL OR image_digest ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (jsonb_typeof(features) = 'array' AND jsonb_array_length(features) BETWEEN 1 AND 128
    AND octet_length(features::text) <= 262144)
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_figure_requests (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  project_id uuid NOT NULL,
  source_task_id uuid NOT NULL,
  source_artifact_id uuid NOT NULL,
  source_package_sha256 text NOT NULL,
  table_path text NOT NULL,
  table_sha256 text NOT NULL,
  table_rows bigint NOT NULL,
  table_columns jsonb NOT NULL,
  feature text NOT NULL,
  controls jsonb NOT NULL,
  title text NOT NULL,
  previous_task_id uuid,
  catalog_sha256 text NOT NULL,
  request jsonb NOT NULL,
  request_sha256 text NOT NULL,
  principal_id uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, source_task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, source_artifact_id) REFERENCES enrichedlab_agent.broker_artifacts (tenant_id, artifact_id),
  FOREIGN KEY (tenant_id, previous_task_id) REFERENCES enrichedlab_agent.broker_figure_requests (tenant_id, task_id),
  FOREIGN KEY (tenant_id, catalog_sha256) REFERENCES enrichedlab_agent.broker_render_catalogs (tenant_id, catalog_sha256),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (source_package_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (table_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (length(table_path) BETWEEN 1 AND 256 AND table_path !~ '(^|/)\.\.(/|$)' AND table_path !~ '^/'),
  CHECK (feature ~ '^[a-z][a-z0-9_]{1,63}$'),
  CHECK (jsonb_typeof(controls) = 'object' AND octet_length(controls::text) <= 8192),
  CHECK (length(title) <= 200),
  CHECK (jsonb_typeof(table_columns) = 'array' AND jsonb_array_length(table_columns) BETWEEN 1 AND 64),
  CHECK (table_rows BETWEEN 1 AND 50000),
  CHECK (jsonb_typeof(request) = 'object'),
  CHECK (previous_task_id IS NULL OR previous_task_id <> task_id)
);

CREATE INDEX IF NOT EXISTS broker_figure_requests_source
  ON enrichedlab_agent.broker_figure_requests (tenant_id, source_task_id, created_at DESC);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_figure_receipts (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  status text NOT NULL,
  error_code text,
  message text,
  service_version text,
  image_digest text,
  files jsonb,
  notes jsonb NOT NULL,
  package_sha256 text,
  started_at timestamptz NOT NULL,
  finished_at timestamptz NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_figure_requests (tenant_id, task_id),
  CHECK (attempt >= 1),
  CHECK (lease_generation > 0),
  CHECK (status IN ('completed', 'refused', 'failed')),
  CHECK ((status = 'completed') = (error_code IS NULL)),
  CHECK (status <> 'completed' OR (package_sha256 IS NOT NULL AND files IS NOT NULL AND service_version IS NOT NULL)),
  CHECK (error_code IS NULL OR error_code ~ '^[a-z][a-z0-9_]{2,63}$'),
  CHECK (message IS NULL OR length(message) BETWEEN 1 AND 300),
  CHECK (service_version IS NULL OR service_version ~ '^[0-9A-Za-z][0-9A-Za-z.+-]{0,31}$'),
  CHECK (image_digest IS NULL OR image_digest ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (files IS NULL OR (jsonb_typeof(files) = 'array' AND jsonb_array_length(files) BETWEEN 1 AND 8)),
  CHECK (jsonb_typeof(notes) = 'array' AND jsonb_array_length(notes) <= 32 AND octet_length(notes::text) <= 8192),
  CHECK (package_sha256 IS NULL OR package_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (finished_at >= started_at)
);

DO $$ BEGIN
  ALTER TABLE enrichedlab_agent.broker_render_catalogs ENABLE ROW LEVEL SECURITY;
  ALTER TABLE enrichedlab_agent.broker_figure_requests ENABLE ROW LEVEL SECURITY;
  ALTER TABLE enrichedlab_agent.broker_figure_receipts ENABLE ROW LEVEL SECURITY;
  REVOKE ALL ON enrichedlab_agent.broker_render_catalogs FROM PUBLIC;
  REVOKE ALL ON enrichedlab_agent.broker_figure_requests FROM PUBLIC;
  REVOKE ALL ON enrichedlab_agent.broker_figure_receipts FROM PUBLIC;
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_figure_requests;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_figure_requests
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_figure_receipts;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_figure_receipts
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
END $$;

-- Worker: the render service's figure features as it last read them. The catalog's identity is the digest of its
-- stored JSON, so the same catalog read again only moves its last-seen time.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_render_catalog(p_tenant text, p_worker text,
  p_version text, p_image_digest text, p_features jsonb)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_sha text;
  f jsonb;
  s jsonb;
  k text;
BEGIN
  IF p_worker IS NULL OR length(p_worker) NOT BETWEEN 1 AND 128 OR jsonb_typeof(p_features) IS DISTINCT FROM 'array' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:render_catalog_invalid';
  END IF;
  FOR f IN SELECT value FROM jsonb_array_elements(p_features) LOOP
    IF jsonb_typeof(f) IS DISTINCT FROM 'object' OR coalesce(f->>'id', '') !~ '^[a-z][a-z0-9_]{1,63}$'
      OR jsonb_typeof(f->'label') IS DISTINCT FROM 'string' OR length(f->>'label') NOT BETWEEN 1 AND 120
      OR jsonb_typeof(f->'params') IS DISTINCT FROM 'object'
      OR EXISTS (SELECT 1 FROM jsonb_object_keys(f) x WHERE x NOT IN ('id', 'label', 'description', 'params')) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:render_catalog_invalid';
    END IF;
    FOR k, s IN SELECT key, value FROM jsonb_each(f->'params') LOOP
      IF k !~ '^[a-z][a-z0-9_]{0,63}$' OR jsonb_typeof(s) IS DISTINCT FROM 'object'
        OR coalesce(s->>'type', '') NOT IN ('column', 'columns', 'enum', 'number', 'numbers', 'string') THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:render_catalog_invalid';
      END IF;
    END LOOP;
  END LOOP;
  v_sha := encode(sha256(convert_to(p_features::text, 'UTF8')), 'hex');
  INSERT INTO enrichedlab_agent.broker_render_catalogs (tenant_id, catalog_sha256, service_version, image_digest, features)
  VALUES (p_tenant, v_sha, p_version, p_image_digest, p_features)
  ON CONFLICT (tenant_id, catalog_sha256) DO UPDATE SET last_seen_at = clock_timestamp();
  RETURN v_sha;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_render_catalog(text, text, text, text, jsonb) FROM PUBLIC;

-- API: the catalog the worker read most recently, for any enabled principal of the tenant; null before the first read.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_render_catalog(p_tenant text, p_actor uuid)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object('catalog_sha256', c.catalog_sha256, 'service_version', c.service_version,
    'image_digest', c.image_digest, 'features', c.features, 'seen_at', c.last_seen_at,
    -- The labs the reader belongs to or holds a project in that have exactly one enabled figure method, so the
    -- workspace offers figures only where a request can be accepted.
    'groups', coalesce((SELECT jsonb_agg(f.group_key ORDER BY f.group_key) FROM (
        SELECT g.group_key FROM enrichedlab_agent.broker_group_profile_grants g
        JOIN enrichedlab_agent.broker_agent_profiles p ON p.tenant_id = g.tenant_id AND p.profile_key = g.profile_key
        WHERE g.tenant_id = p_tenant AND p.enabled AND p.route = 'server-render'
          AND (EXISTS (SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
                 WHERE m.tenant_id = p_tenant AND m.group_key = g.group_key AND m.principal_id = p_actor)
            OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_grants pg
                 JOIN enrichedlab_agent.broker_projects pr ON pr.tenant_id = pg.tenant_id AND pr.project_id = pg.project_id
                 WHERE pg.tenant_id = p_tenant AND pg.principal_id = p_actor AND pr.group_key = g.group_key))
        GROUP BY g.group_key HAVING count(*) = 1) f), '[]'::jsonb))
  FROM enrichedlab_agent.broker_render_catalogs c
  WHERE c.tenant_id = p_tenant
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.broker_principals a
      WHERE a.tenant_id = p_tenant AND a.principal_id = p_actor AND a.enabled)
  ORDER BY c.last_seen_at DESC, c.catalog_sha256
  LIMIT 1
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_render_catalog(text, uuid) FROM PUBLIC;

-- The figure method runs only from a person's figure request, so it is never offered as a method to run or to give the
-- research agent: the profile choices below mark it not offered, and 016's person submissions and 011's run
-- authorization refuse it. A task on the route without a request would finish as figure_not_recorded; these refuse it
-- first.
-- The profile list (migration 002) with whether each method is offered: a person chooses among the offered ones to run
-- or to give the research agent, and every listed one still names the tasks that ran under it.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_profile_choices(p_tenant text, p_principal uuid)
RETURNS TABLE(profile_key text, name text, description text, tool_bundle text, max_run_seconds integer, offered boolean)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT DISTINCT profiles.profile_key, profiles.name, profiles.description,
         profiles.tool_bundle, profiles.max_run_seconds, profiles.route <> 'server-render'
  FROM enrichedlab_agent.broker_group_memberships AS memberships
  JOIN enrichedlab_agent.broker_group_profile_grants AS grants
    ON grants.tenant_id = memberships.tenant_id
   AND grants.group_key = memberships.group_key
  JOIN enrichedlab_agent.broker_agent_profiles AS profiles
    ON profiles.tenant_id = grants.tenant_id
   AND profiles.profile_key = grants.profile_key
  WHERE memberships.tenant_id = p_tenant
    AND memberships.principal_id = p_principal
    AND profiles.enabled
  ORDER BY profiles.name, profiles.profile_key
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_profile_choices(text, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_submit_person_task(p_tenant text,p_actor uuid,p_group text,p_profile text,p_objective text,p_key text,p_sha text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE task uuid;
BEGIN
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_agent_profiles
      WHERE tenant_id=p_tenant AND profile_key=p_profile AND route='server-render') THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden';
  END IF;
  task:=enrichedlab_agent.broker_submit_task(p_tenant,p_actor,p_group,p_profile,p_objective,p_key,p_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id,task_id,origin,basis,principal_id)
    VALUES(p_tenant,task,'person','submission',p_actor) ON CONFLICT (tenant_id,task_id) DO NOTHING;
  RETURN task;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_submit_person_project_task(p_tenant text,p_actor uuid,p_project uuid,p_action text,p_parent uuid,p_group text,p_profile text,p_objective text,p_key text,p_sha text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE task uuid;
BEGIN
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_agent_profiles
      WHERE tenant_id=p_tenant AND profile_key=p_profile AND route='server-render') THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden';
  END IF;
  task:=enrichedlab_agent.broker_submit_project_task(p_tenant,p_actor,p_project,p_action,p_parent,p_group,p_profile,p_objective,p_key,p_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id,task_id,origin,basis,principal_id)
    VALUES(p_tenant,task,'person','submission',p_actor) ON CONFLICT (tenant_id,task_id) DO NOTHING;
  RETURN task;
END $$;

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
        WHERE p.tenant_id=p_tenant AND p.project_id=p_project AND f.profile_key=k AND f.enabled AND f.route<>'server-render'
      )
    )
$$;

-- API: a person's request for a figure version. It checks the person may contribute to the project, the source is a
-- finished analysis of that project, the table is one its retained package's digest lists and is small enough, the
-- feature is in the newest catalog, and each setting is one the feature declares with a value of its kind (a column
-- setting names a column of the table). The service checks values and clamps numbers when it draws. The request and
-- its task are written together; the same request id with the same request returns the same task.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_figure(p_tenant text, p_actor uuid, p_project uuid,
  p_source_task uuid, p_table_path text, p_feature text, p_controls jsonb, p_title text, p_previous uuid,
  p_request_id uuid)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_group text;
  v_src enrichedlab_agent.broker_tasks%ROWTYPE;
  v_artifact uuid;
  v_package text;
  v_table jsonb;
  v_rows bigint;
  v_columns jsonb;
  v_catalog enrichedlab_agent.broker_render_catalogs%ROWTYPE;
  v_feature jsonb;
  v_params jsonb;
  v_profile text;
  v_profiles integer;
  v_prev enrichedlab_agent.broker_figure_requests%ROWTYPE;
  v_title text := btrim(coalesce(p_title, ''));
  v_request jsonb;
  v_sha text;
  v_task uuid;
  v_objective text;
  k text;
  v jsonb;
  s jsonb;
  v_ok boolean;
BEGIN
  SELECT group_key INTO v_group FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = p_project;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'contribute') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  SELECT t.* INTO v_src FROM enrichedlab_agent.broker_tasks t
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = t.tenant_id AND pt.task_id = t.task_id
    WHERE t.tenant_id = p_tenant AND t.task_id = p_source_task AND pt.project_id = p_project;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  IF v_src.status <> 'completed' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:figure_source_not_ready';
  END IF;
  -- The table as the digest of the source's current retained package lists it.
  SELECT a.artifact_id, a.sha256, e INTO v_artifact, v_package, v_table
    FROM enrichedlab_agent.broker_artifacts a
    JOIN enrichedlab_agent.broker_artifact_digests d
      ON d.tenant_id = a.tenant_id AND d.artifact_id = a.artifact_id AND d.artifact_sha256 = a.sha256
    CROSS JOIN LATERAL jsonb_array_elements(coalesce(d.digest->'tables', '[]'::jsonb)) e
    WHERE a.tenant_id = p_tenant AND a.task_id = p_source_task AND a.role = 'result' AND a.visibility = 'researcher'
      AND d.status = 'summarized' AND e->>'path' = p_table_path
    ORDER BY d.recorded_at DESC
    LIMIT 1;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_table_unknown';
  END IF;
  v_rows := (v_table->>'rows')::bigint;
  v_columns := v_table->'columns';
  IF v_rows IS NULL OR v_rows < 1 OR v_rows > 50000 OR v_table ? 'columns_omitted'
    OR jsonb_typeof(v_columns) IS DISTINCT FROM 'array' OR jsonb_array_length(v_columns) NOT BETWEEN 1 AND 64
    OR coalesce(v_table->>'sha256', '') !~ '^[0-9a-f]{64}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_table_unsuitable';
  END IF;
  -- The lab's one figure method: an enabled profile on the render route that the project's group may use.
  SELECT count(*), min(p.profile_key) INTO v_profiles, v_profile
    FROM enrichedlab_agent.broker_agent_profiles p
    JOIN enrichedlab_agent.broker_group_profile_grants g ON g.tenant_id = p.tenant_id AND g.profile_key = p.profile_key
    WHERE p.tenant_id = p_tenant AND p.enabled AND p.route = 'server-render' AND g.group_key = v_group;
  IF v_profiles <> 1 THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:figure_method_unavailable';
  END IF;
  SELECT c.* INTO v_catalog FROM enrichedlab_agent.broker_render_catalogs c WHERE c.tenant_id = p_tenant
    ORDER BY c.last_seen_at DESC, c.catalog_sha256 LIMIT 1;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:figure_catalog_unavailable';
  END IF;
  SELECT f INTO v_feature FROM jsonb_array_elements(v_catalog.features) f WHERE f->>'id' = p_feature;
  IF v_feature IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_feature_unknown';
  END IF;
  v_params := v_feature->'params';
  IF jsonb_typeof(p_controls) IS DISTINCT FROM 'object' OR octet_length(p_controls::text) > 8192
    OR (SELECT count(*) FROM jsonb_object_keys(p_controls)) > 40 OR length(v_title) > 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_settings_invalid';
  END IF;
  FOR k, v IN SELECT key, value FROM jsonb_each(p_controls) LOOP
    s := v_params->k;
    IF s IS NULL THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_settings_invalid';
    END IF;
    v_ok := CASE s->>'type'
      WHEN 'column' THEN jsonb_typeof(v) = 'string' AND v_columns @> jsonb_build_array(v)
      WHEN 'columns' THEN jsonb_typeof(v) = 'array' AND jsonb_array_length(v) BETWEEN 1 AND 16
        AND NOT EXISTS (SELECT 1 FROM jsonb_array_elements(v) c WHERE jsonb_typeof(c) <> 'string' OR NOT v_columns @> jsonb_build_array(c))
      WHEN 'enum' THEN jsonb_typeof(v) IN ('string', 'number', 'boolean') AND EXISTS (SELECT 1
        FROM jsonb_array_elements(coalesce(s->'choices', '[]'::jsonb)) c WHERE lower(c #>> '{}') = lower(v #>> '{}'))
      WHEN 'number' THEN jsonb_typeof(v) = 'number'
      WHEN 'numbers' THEN jsonb_typeof(v) = 'array' AND jsonb_array_length(v) BETWEEN 1 AND 16
        AND NOT EXISTS (SELECT 1 FROM jsonb_array_elements(v) c WHERE jsonb_typeof(c) <> 'number')
      WHEN 'string' THEN jsonb_typeof(v) = 'string' AND length(v #>> '{}') <= 200
      ELSE false END;
    IF v_ok IS NOT TRUE THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_settings_invalid';
    END IF;
  END LOOP;
  -- Every required column is named, so a figure never rests on the service guessing one.
  FOR k, s IN SELECT key, value FROM jsonb_each(v_params) LOOP
    IF (s->>'required')::boolean IS TRUE AND s->>'type' IN ('column', 'columns') AND NOT p_controls ? k THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_settings_invalid';
    END IF;
  END LOOP;
  IF p_previous IS NOT NULL THEN
    SELECT r.* INTO v_prev FROM enrichedlab_agent.broker_figure_requests r
      WHERE r.tenant_id = p_tenant AND r.task_id = p_previous AND r.project_id = p_project
        AND r.source_task_id = p_source_task AND r.table_path = p_table_path;
    IF NOT FOUND THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_previous_invalid';
    END IF;
  END IF;
  v_request := jsonb_build_object('schema', 'enrichedlab.figure-request.v1', 'project', p_project,
    'source_task', p_source_task, 'source_package_sha256', v_package, 'table_path', p_table_path,
    'table_sha256', v_table->>'sha256', 'feature', p_feature, 'controls', p_controls, 'title', v_title,
    'previous', p_previous, 'catalog_sha256', v_catalog.catalog_sha256);
  v_sha := encode(sha256(convert_to(v_request::text, 'UTF8')), 'hex');
  v_objective := CASE WHEN v_title <> '' THEN v_title
    ELSE (v_feature->>'label') || ' of ' || replace(regexp_replace(regexp_replace(p_table_path, '^.*/', ''),
      '\.[A-Za-z0-9]+$', ''), '_', ' ') END;
  -- The person submission refuses the figure route, so the request submits the project task and its origin itself.
  v_task := enrichedlab_agent.broker_submit_project_task(p_tenant, p_actor, p_project,
    'figure-' || p_request_id::text, p_source_task, v_group, v_profile, v_objective, 'figure-' || p_request_id::text, v_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id, task_id, origin, basis, principal_id)
    VALUES (p_tenant, v_task, 'person', 'submission', p_actor) ON CONFLICT (tenant_id, task_id) DO NOTHING;
  INSERT INTO enrichedlab_agent.broker_figure_requests (tenant_id, task_id, project_id, source_task_id,
    source_artifact_id, source_package_sha256, table_path, table_sha256, table_rows, table_columns, feature, controls,
    title, previous_task_id, catalog_sha256, request, request_sha256, principal_id)
  VALUES (p_tenant, v_task, p_project, p_source_task, v_artifact, v_package, p_table_path, v_table->>'sha256',
    v_rows, v_columns, p_feature, p_controls, v_title, p_previous, v_catalog.catalog_sha256, v_request, v_sha, p_actor)
  ON CONFLICT (tenant_id, task_id) DO NOTHING;
  RETURN v_task;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_request_figure(text, uuid, uuid, uuid, text, text, jsonb, text, uuid, uuid)
  FROM PUBLIC;

-- Worker: the request of its current lease with the source package's place in the artifact store.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_figure(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object('source_task', r.source_task_id, 'package_path', a.relative_path,
    'package_sha256', r.source_package_sha256, 'package_bytes', a.size_bytes, 'table_path', r.table_path,
    'table_sha256', r.table_sha256, 'table_rows', r.table_rows, 'feature', r.feature, 'controls', r.controls,
    'title', r.title, 'request_sha256', r.request_sha256, 'catalog_sha256', r.catalog_sha256)
  FROM enrichedlab_agent.broker_tasks t
  JOIN enrichedlab_agent.broker_figure_requests r USING (tenant_id, task_id)
  JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id = r.tenant_id AND a.artifact_id = r.source_artifact_id
  WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
    AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running') AND t.desired_status = 'run'
    AND t.lease_expires_at > CURRENT_TIMESTAMP
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_figure(text, uuid, text, integer, bigint) FROM PUBLIC;

-- Worker: one receipt per attempt and lease, under that lease. The same receipt again is accepted; a different one is not.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_figure_receipt(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint, p_status text, p_error text, p_message text, p_service_version text,
  p_image_digest text, p_files jsonb, p_notes jsonb, p_package_sha256 text, p_started timestamptz,
  p_finished timestamptz)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v enrichedlab_agent.broker_figure_receipts%ROWTYPE;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks t
      JOIN enrichedlab_agent.broker_figure_requests r USING (tenant_id, task_id)
      WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
        AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running')
        AND t.lease_expires_at > CURRENT_TIMESTAMP) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_finished < p_started OR p_started > clock_timestamp() + interval '1 minute' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_receipt_invalid';
  END IF;
  SELECT * INTO v FROM enrichedlab_agent.broker_figure_receipts
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = p_attempt AND lease_generation = p_generation;
  IF FOUND THEN
    IF v.status IS DISTINCT FROM p_status OR v.error_code IS DISTINCT FROM p_error
      OR v.package_sha256 IS DISTINCT FROM p_package_sha256 THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
  ELSE
    INSERT INTO enrichedlab_agent.broker_figure_receipts (tenant_id, task_id, attempt, lease_generation, status,
      error_code, message, service_version, image_digest, files, notes, package_sha256, started_at, finished_at)
    VALUES (p_tenant, p_task, p_attempt, p_generation, p_status, p_error, p_message, p_service_version,
      p_image_digest, p_files, coalesce(p_notes, '[]'::jsonb), p_package_sha256, p_started, p_finished)
    RETURNING * INTO v;
  END IF;
  RETURN jsonb_build_object('status', v.status, 'recorded_at', v.recorded_at);
EXCEPTION WHEN check_violation THEN
  RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_receipt_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_figure_receipt(text, uuid, text, integer, bigint, text, text,
  text, text, text, jsonb, jsonb, text, timestamptz, timestamptz) FROM PUBLIC;

-- API: a figure version's request, its source analysis as a scientist reads it and every receipt, for anyone who may
-- read the version; null for a task that is not a figure version.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_figure_detail(p_tenant text, p_actor uuid, p_task uuid)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  r enrichedlab_agent.broker_figure_requests%ROWTYPE;
  v_source_title text;
BEGIN
  IF NOT enrichedlab_agent.broker_task_can(p_tenant, p_actor, p_task, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  SELECT * INTO r FROM enrichedlab_agent.broker_figure_requests WHERE tenant_id = p_tenant AND task_id = p_task;
  IF NOT FOUND THEN
    RETURN NULL;
  END IF;
  SELECT t.objective INTO v_source_title FROM enrichedlab_agent.broker_tasks t
    WHERE t.tenant_id = p_tenant AND t.task_id = r.source_task_id;
  RETURN jsonb_build_object('task_id', r.task_id, 'project_id', r.project_id, 'source_task', r.source_task_id,
    'source_title', v_source_title, 'source_package_sha256', r.source_package_sha256, 'table_path', r.table_path,
    'table_sha256', r.table_sha256, 'table_rows', r.table_rows, 'table_columns', r.table_columns,
    'feature', r.feature, 'controls', r.controls, 'title', r.title, 'previous', r.previous_task_id,
    'catalog_sha256', r.catalog_sha256, 'request_sha256', r.request_sha256, 'created_at', r.created_at,
    'receipts', coalesce((SELECT jsonb_agg(jsonb_build_object('attempt', x.attempt, 'status', x.status,
        'error_code', x.error_code, 'message', x.message, 'service_version', x.service_version,
        'image_digest', x.image_digest, 'files', x.files, 'notes', x.notes, 'package_sha256', x.package_sha256,
        'started_at', x.started_at, 'finished_at', x.finished_at) ORDER BY x.attempt, x.lease_generation)
      FROM enrichedlab_agent.broker_figure_receipts x WHERE x.tenant_id = p_tenant AND x.task_id = p_task), '[]'::jsonb));
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_figure_detail(text, uuid, uuid) FROM PUBLIC;

-- API: the figure versions made from one analysis, newest first, for anyone who may read that analysis. A version the
-- reader may not read is left out.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_figure_versions(p_tenant text, p_actor uuid, p_source_task uuid)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_task_can(p_tenant, p_actor, p_source_task, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  RETURN coalesce((SELECT jsonb_agg(jsonb_build_object('task_id', r.task_id, 'table_path', r.table_path,
      'feature', r.feature, 'title', r.title, 'previous', r.previous_task_id, 'status', t.status,
      'created_at', r.created_at, 'finished_at', t.finished_at) ORDER BY r.created_at DESC, r.task_id)
    FROM enrichedlab_agent.broker_figure_requests r
    JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = r.tenant_id AND t.task_id = r.task_id
    WHERE r.tenant_id = p_tenant AND r.source_task_id = p_source_task
      AND enrichedlab_agent.broker_task_can(p_tenant, p_actor, r.task_id, 'read')), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_figure_versions(text, uuid, uuid) FROM PUBLIC;

-- The project read (migration 014) with one addition: each figure version names its figure, source analysis, table and
-- the version it edits, so the project's contents can title it and show the newest version of each figure.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_project(p_tenant text,p_actor uuid,p_id uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE result jsonb;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_id,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  SELECT to_jsonb(p)-'tenant_id' INTO result FROM enrichedlab_agent.broker_projects p WHERE tenant_id=p_tenant AND project_id=p_id;
  RETURN result || jsonb_build_object(
    'capabilities',(SELECT coalesce(jsonb_agg(c),'[]') FROM unnest(ARRAY['read','review','contribute','steer','cancel','manage']) c WHERE enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_id,c)),
    'checkpoint',(SELECT jsonb_build_object('revision',c.revision,'author_id',c.author_id,'created_at',c.created_at,'state',c.state) FROM enrichedlab_agent.broker_project_checkpoints c WHERE c.tenant_id=p_tenant AND c.project_id=p_id ORDER BY revision DESC LIMIT 1),
    'tasks',(SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.submitted_at DESC,t.task_id DESC),'[]') FROM (
      SELECT t.task_id,pt.action_key,pt.parent_task_id,t.profile_key AS profile,t.status,t.attempt,t.updated_at,t.submitted_at,
        CASE WHEN fr.task_id IS NULL THEN NULL ELSE jsonb_build_object('title',t.objective,'feature',fr.feature,
          'source_task',fr.source_task_id,'table_path',fr.table_path,'previous',fr.previous_task_id) END AS figure
      FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
      LEFT JOIN enrichedlab_agent.broker_figure_requests fr ON fr.tenant_id=pt.tenant_id AND fr.task_id=pt.task_id
      WHERE pt.tenant_id=p_tenant AND pt.project_id=p_id ORDER BY t.submitted_at DESC,t.task_id DESC LIMIT 200
    ) t),
    'task_count',(SELECT count(*) FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=p_id),
    'inspection_expires_at',(SELECT i.expires_at FROM enrichedlab_agent.broker_project_inspections i
      WHERE i.tenant_id=p_tenant AND i.project_id=p_id AND i.principal_id=p_actor AND i.expires_at>clock_timestamp())
  );
END $$;

-- Figure versions are drawings of results a project already holds, not analyses: the lab index's analysis counts
-- (migration 026) and what waits for review (037) leave them out. Each function is its latest version with that one
-- exclusion.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_projects(p_tenant text,p_actor uuid,p_limit integer)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF p_limit NOT BETWEEN 1 AND 200 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:list_limit_invalid'; END IF;
  RETURN QUERY SELECT (to_jsonb(p)-'tenant_id')||jsonb_build_object(
      'owner',jsonb_build_object('principal_id',o.principal_id,'display_name',o.display_name,
        'kind',CASE WHEN o.issuer='kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END),
      'question',(SELECT c.state->>'goal' FROM enrichedlab_agent.broker_project_checkpoints c
        WHERE c.tenant_id=p.tenant_id AND c.project_id=p.project_id AND c.revision=p.revision),
      'capabilities',(SELECT coalesce(jsonb_agg(k),'[]'::jsonb)
        FROM unnest(ARRAY['read','review','contribute','steer','cancel','manage']) k
        WHERE enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,k)),
      'latest_run',(SELECT jsonb_build_object('run_id',r.run_id,'status',r.status,'code',r.code,'turn',r.turn,
          'max_turns',r.max_turns,'updated_at',r.updated_at)
        FROM enrichedlab_agent.broker_research_runs r
        WHERE r.tenant_id=p.tenant_id AND r.project_id=p.project_id ORDER BY r.created_at DESC,r.run_id LIMIT 1),
      'task_counts',(SELECT jsonb_build_object('total',count(*),
          'active',count(*) FILTER (WHERE t.status IN ('queued','leased','starting','running','cancelling')),
          'finished',count(*) FILTER (WHERE t.status IN ('completed','review_required')),
          'failed',count(*) FILTER (WHERE t.status='failed'),
          -- A finished analysis whose newest review by a person still covers its result files.
          'reviewed',count(*) FILTER (WHERE t.status IN ('completed','review_required') AND v.covers),
          'rejected',count(*) FILTER (WHERE t.status IN ('completed','review_required') AND v.covers AND v.decision='rejected'),
          'changes',count(*) FILTER (WHERE t.status IN ('completed','review_required') AND v.covers AND v.decision='needs_changes'))
        FROM enrichedlab_agent.broker_project_tasks pt
        JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=pt.tenant_id AND t.task_id=pt.task_id
        LEFT JOIN LATERAL (
          SELECT rv.decision, rv.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, rv.task_id) AS covers
          FROM enrichedlab_agent.broker_task_reviews rv
          JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=rv.tenant_id AND a.principal_id=rv.reviewer_id
          WHERE rv.tenant_id=pt.tenant_id AND rv.task_id=pt.task_id AND a.issuer<>'kubernetes://serviceaccount'
          ORDER BY rv.created_at DESC, rv.review_id DESC LIMIT 1) v ON true
        WHERE pt.tenant_id=p.tenant_id AND pt.project_id=p.project_id
          AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr
            WHERE fr.tenant_id=pt.tenant_id AND fr.task_id=pt.task_id)))
    FROM enrichedlab_agent.broker_projects p
    JOIN enrichedlab_agent.broker_principals o ON o.tenant_id=p.tenant_id AND o.principal_id=p.owner_id
    WHERE p.tenant_id=p_tenant AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,'read')
    ORDER BY p.updated_at DESC,p.project_id LIMIT p_limit;
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_projects(text,uuid,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_inbox(p_tenant text, p_actor uuid)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  WITH projects AS (
    SELECT p.project_id, p.group_key, p.title, p.revision,
           enrichedlab_agent.broker_project_can(p_tenant, p_actor, p.project_id, 'steer') AS steer,
           enrichedlab_agent.broker_project_can(p_tenant, p_actor, p.project_id, 'review') AS review
    FROM enrichedlab_agent.broker_projects p
    WHERE p.tenant_id = p_tenant
  ),
  latest_runs AS (
    SELECT DISTINCT ON (r.project_id) r.project_id, r.run_id, r.status, r.code, r.turn, r.updated_at
    FROM enrichedlab_agent.broker_research_runs r
    WHERE r.tenant_id = p_tenant
    ORDER BY r.project_id, r.created_at DESC, r.run_id DESC
  ),
  -- The agent stopped and asks for a person: the newest run of a project the caller may steer or review.
  review_requests AS (
    SELECT 'review_request' AS kind, r.run_id::text AS ref, r.run_id::text AS version, r.updated_at AS at,
           jsonb_build_object('lab', p.group_key, 'project_id', p.project_id, 'project_title', p.title, 'run_id', r.run_id,
             'code', r.code, 'turn', r.turn,
             -- The recorded reason, in the agent's own words: the methods check's when it asked for review, else the
             -- lead's summary; none when the run stopped before a decision.
             'reason_source', CASE WHEN t.record->'method_review'->>'decision' = 'needs_review' THEN 'methods_check'
                                   WHEN t.record IS NOT NULL THEN 'lead' END,
             'reason', CASE WHEN t.record->'method_review'->>'decision' = 'needs_review' THEN t.record->'method_review'->>'reason'
                            ELSE t.record->'proposal'->>'summary' END) AS item
    FROM latest_runs r
    JOIN projects p ON p.project_id = r.project_id
    LEFT JOIN LATERAL (
      SELECT rt.record FROM enrichedlab_agent.broker_research_turns rt
      WHERE rt.tenant_id = p_tenant AND rt.run_id = r.run_id ORDER BY rt.turn DESC LIMIT 1
    ) t ON true
    WHERE r.status = 'review_required' AND (p.steer OR p.review)
  ),
  -- Finished analyses whose newest review by a person does not cover their current result files.
  analysis_reviews AS (
    SELECT 'analysis_review' AS kind, t.task_id::text AS ref,
           md5(enrichedlab_agent.broker_task_review_outputs(p_tenant, t.task_id)::text) AS version,
           coalesce(t.finished_at, t.updated_at) AS at,
           jsonb_build_object('lab', p.group_key, 'project_id', p.project_id, 'project_title', p.title, 'task_id', t.task_id,
             'action_key', pt.action_key, 'status', t.status) AS item
    FROM projects p
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = p_tenant AND pt.project_id = p.project_id
    JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = p_tenant AND t.task_id = pt.task_id
    WHERE p.review AND enrichedlab_agent.broker_is_person(p_tenant, p_actor)
      AND t.status IN ('completed', 'review_required')
      -- A figure version redraws results the project already holds, so it does not wait for review.
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr
        WHERE fr.tenant_id = p_tenant AND fr.task_id = t.task_id)
      AND NOT EXISTS (
        SELECT 1 FROM (
          SELECT rv.outputs FROM enrichedlab_agent.broker_task_reviews rv
          JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = rv.tenant_id AND a.principal_id = rv.reviewer_id
          WHERE rv.tenant_id = p_tenant AND rv.task_id = t.task_id AND a.principal_class = 'person'
          ORDER BY rv.created_at DESC, rv.review_id DESC LIMIT 1
        ) newest
        WHERE newest.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, t.task_id))
  ),
  -- Steps of the current plan whose analysis the system could not confirm, versioned by the revision that marked them.
  step_checks AS (
    SELECT 'step_check' AS kind, p.project_id::text || ':' || (s->>'action_key') AS ref,
           coalesce((SELECT e.revision FROM enrichedlab_agent.broker_plan_events e
                     WHERE e.tenant_id = p_tenant AND e.project_id = p.project_id AND e.action_key = s->>'action_key'
                       AND e.status_after = 'uncertain' ORDER BY e.revision DESC LIMIT 1), p.revision)::text AS version,
           c.created_at AS at,
           jsonb_build_object('lab', p.group_key, 'project_id', p.project_id, 'project_title', p.title,
             'action_key', s->>'action_key', 'title', s->>'title', 'task_id', s->>'task_id') AS item
    FROM projects p
    JOIN enrichedlab_agent.broker_project_checkpoints c
      ON c.tenant_id = p_tenant AND c.project_id = p.project_id AND c.revision = p.revision
    CROSS JOIN LATERAL jsonb_array_elements(c.state->'plan') s
    WHERE p.steer AND s->>'status' = 'uncertain'
  ),
  -- Papers in a lab's library that no curator has accepted, for the lab's curators; one item per lab.
  paper_acceptance AS (
    SELECT 'paper_acceptance' AS kind, w.group_key AS ref, md5(string_agg(w.paper_id::text, ',' ORDER BY w.paper_id)) AS version,
           max(w.created_at) AS at, jsonb_build_object('lab', w.group_key, 'papers', count(*)) AS item
    FROM (
      SELECT lp.group_key, lp.paper_id, lp.created_at FROM enrichedlab_agent.broker_library_papers lp
      WHERE lp.tenant_id = p_tenant AND enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, lp.group_key)
        AND enrichedlab_agent.broker_library_removal(p_tenant, lp.paper_id) IS NULL
        AND enrichedlab_agent.broker_library_acceptance(p_tenant, lp.paper_id) IS NULL
    ) w
    GROUP BY w.group_key
  ),
  -- Shared notes the lead no longer reads because their review-by date passed or their evidence changed.
  note_attention AS (
    SELECT 'note_attention' AS kind, n.note_id::text AS ref,
           (x.state->>'state') || ':' || (x.state->>'review_by') || coalesce(':' || (x.state->>'task_id'), '') AS version,
           (x.state->>'review_by')::timestamptz AS at,
           jsonb_build_object('lab', n.group_key, 'note_id', n.note_id, 'project_id', n.project_id, 'state', x.state->>'state',
             'cause', x.state->>'cause', 'review_by', x.state->>'review_by') AS item
    FROM enrichedlab_agent.broker_lab_notes n
    CROSS JOIN LATERAL (SELECT enrichedlab_agent.broker_lab_note_state(p_tenant, n) AS state) x
    WHERE n.tenant_id = p_tenant AND enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, n.group_key)
      AND x.state->>'state' IN ('expired', 'paused')
  ),
  items AS (
    SELECT * FROM review_requests UNION ALL SELECT * FROM analysis_reviews UNION ALL SELECT * FROM step_checks
    UNION ALL SELECT * FROM paper_acceptance UNION ALL SELECT * FROM note_attention
  )
  SELECT jsonb_build_object(
    'can_acknowledge', enrichedlab_agent.broker_is_person(p_tenant, p_actor),
    'items', coalesce(jsonb_agg(i.item || jsonb_build_object('kind', i.kind, 'ref', i.ref, 'version', i.version, 'at', i.at,
      'seen_at', a.acknowledged_at) ORDER BY i.at DESC, i.kind, i.ref), '[]'::jsonb))
  FROM items i
  LEFT JOIN enrichedlab_agent.broker_inbox_acknowledgements a
    ON a.tenant_id = p_tenant AND a.principal_id = p_actor AND a.kind = i.kind AND a.item_ref = i.ref AND a.item_version = i.version
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_inbox(text, uuid) FROM PUBLIC;

-- Lineage version 1 (migration 039) with a figure version's render record: its method class is the vetted render
-- service, its input class the prior output it drew, and it has no plan step or workspace inputs of its own.
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
  -- A figure version ran in the render service, so the image it names is the one its newest receipt records; any other
  -- task ran in the image its profile named.
  unpinned:=CASE WHEN fr.task_id IS NOT NULL THEN fx.task_id IS NOT NULL AND fx.image_digest IS NULL
    ELSE coalesce(t.profile_snapshot->>'image_ref','') !~ '@sha256:[0-9a-f]{64}$' END;
  IF unpinned THEN
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

-- The turn commit (migration 038) with one change: a person's figure versions are not the run's work, so they do not
-- count against its parallel limit.
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
    WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND t.status IN ('queued','leased','starting','running','cancelling')
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr WHERE fr.tenant_id=pt.tenant_id AND fr.task_id=pt.task_id);
  IF pending+jsonb_array_length(p_actions)>r.max_parallel THEN RAISE EXCEPTION USING ERRCODE='53300',MESSAGE='broker:group_queue_full'; END IF;
  FOR a IN SELECT value FROM jsonb_array_elements(p_actions) LOOP
    IF (a->>'profile'=ANY(r.allowed_profiles)) IS NOT TRUE
      OR (a->>'action_key' ~ '^[a-zA-Z0-9_-]{1,100}$') IS NOT TRUE
      OR (a->>'request_sha256' ~ '^[0-9a-f]{64}$') IS NOT TRUE OR NOT EXISTS (
        SELECT 1 FROM jsonb_array_elements(p_state->'plan') s WHERE s->>'action_key'=a->>'action_key' AND s->>'status'='pending' AND s->>'task_id' IS NULL
      ) OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=r.project_id AND action_key=a->>'action_key') THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
    code:=a->'generated_code'; cfg:=NULL; described:=NULL; q:=a->'query';
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
        OR EXISTS (SELECT 1 FROM jsonb_object_keys(q) k WHERE k NOT IN ('source','proposed_sql','admitted_sql','sha256','guard','relations','row_limit'))
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
    task:=enrichedlab_agent.broker_submit_project_task(p_tenant,r.actor_id,r.project_id,a->>'action_key',NULL,project.group_key,a->>'profile',a->>'objective',
      'research-'||p_run::text||'-'||(r.turn+1)::text||'-'||(a->>'action_key'),a->>'request_sha256');
    IF src.source_key IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_task_queries(tenant_id,task_id,source_key,profile_key,proposed_sql,admitted_sql,
        sql_sha256,guard,relations,row_limit,run_id,turn)
        VALUES(p_tenant,task,src.source_key,src.profile_key,q->>'proposed_sql',q->>'admitted_sql',q->>'sha256',q->>'guard',
          ARRAY(SELECT jsonb_array_elements_text(q->'relations')),(q->>'row_limit')::integer,p_run,r.turn+1);
    END IF;
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
END $function$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (40) ON CONFLICT DO NOTHING;
COMMIT;
