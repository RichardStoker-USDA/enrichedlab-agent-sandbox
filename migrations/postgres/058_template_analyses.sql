-- Template analyses. A person who may contribute to a project asks for an
-- analysis of one result table of a finished analysis in it: one of the render service's analysis templates, reviewed
-- R code that no model wrote, with settings. The request is recorded with the task it creates, in one transaction, under
-- the lab's template-analysis method (route `server-analysis`). The worker's analysis route reads the table from the
-- retained package the request names, has the render service run the template, and keeps a package with every row of
-- the results at full precision, the headline statistics, any figure the template drew, the template and shared R code
-- and the exact settings, with one receipt per attempt. Unlike a figure version, a template analysis is an analysis: it
-- counts in the project's totals and its results table can be the source of a figure or of another analysis.
BEGIN;

-- The service's analysis features as the worker last read them, kept apart from the figure catalog so a figure request
-- can never name an analysis and an analysis request never a figure.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_analysis_catalogs (
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

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_analysis_requests (
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
  settings jsonb NOT NULL,
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
  FOREIGN KEY (tenant_id, previous_task_id) REFERENCES enrichedlab_agent.broker_analysis_requests (tenant_id, task_id),
  FOREIGN KEY (tenant_id, catalog_sha256) REFERENCES enrichedlab_agent.broker_analysis_catalogs (tenant_id, catalog_sha256),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (source_package_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (table_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (length(table_path) BETWEEN 1 AND 256 AND table_path !~ '(^|/)\.\.(/|$)' AND table_path !~ '^/'),
  CHECK (feature ~ '^[a-z][a-z0-9_]{1,63}$'),
  CHECK (jsonb_typeof(settings) = 'object' AND octet_length(settings::text) <= 8192),
  CHECK (length(title) <= 200),
  CHECK (jsonb_typeof(table_columns) = 'array' AND jsonb_array_length(table_columns) BETWEEN 1 AND 64),
  CHECK (table_rows BETWEEN 1 AND 50000),
  CHECK (jsonb_typeof(request) = 'object'),
  CHECK (previous_task_id IS NULL OR previous_task_id <> task_id)
);

CREATE INDEX IF NOT EXISTS broker_analysis_requests_source
  ON enrichedlab_agent.broker_analysis_requests (tenant_id, source_task_id, created_at DESC);

-- One receipt per attempt and lease. A completed one names the package and its files (each with its role: the results
-- table, the headline statistics, the full record, a figure) and the method the template stated; a refused one may
-- carry the service's refusal as the broker checked it, so the workspace can point at the setting to change.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_analysis_receipts (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  status text NOT NULL,
  error_code text,
  message text,
  refusal jsonb,
  service_version text,
  image_digest text,
  method text,
  files jsonb,
  notes jsonb NOT NULL,
  package_sha256 text,
  started_at timestamptz NOT NULL,
  finished_at timestamptz NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_analysis_requests (tenant_id, task_id),
  CHECK (attempt >= 1),
  CHECK (lease_generation > 0),
  CHECK (status IN ('completed', 'refused', 'failed')),
  CHECK ((status = 'completed') = (error_code IS NULL)),
  CHECK (status <> 'completed' OR (package_sha256 IS NOT NULL AND files IS NOT NULL AND service_version IS NOT NULL)),
  CHECK (status = 'completed' OR (package_sha256 IS NULL AND files IS NULL AND method IS NULL)),
  CHECK (error_code IS NULL OR error_code ~ '^[a-z][a-z0-9_]{2,63}$'),
  CHECK (message IS NULL OR length(message) BETWEEN 1 AND 300),
  -- CASE fixes the order: the key and array tests run only on an object.
  CHECK (refusal IS NULL OR (status = 'refused' AND CASE WHEN jsonb_typeof(refusal) = 'object' THEN
    refusal - 'code' - 'columns' - 'counts' = '{}'::jsonb
    AND coalesce(refusal->>'code', '') ~ '^[a-z][a-z0-9_]{0,63}$'
    AND CASE WHEN jsonb_typeof(refusal->'columns') = 'array' AND jsonb_typeof(refusal->'counts') = 'array'
      THEN jsonb_array_length(refusal->'columns') <= 8 AND jsonb_array_length(refusal->'counts') <= 8 ELSE false END
    AND octet_length(refusal::text) <= 2048 ELSE false END)),
  CHECK (service_version IS NULL OR service_version ~ '^[0-9A-Za-z][0-9A-Za-z.+-]{0,31}$'),
  CHECK (image_digest IS NULL OR image_digest ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (method IS NULL OR length(method) BETWEEN 1 AND 2000),
  CHECK (files IS NULL OR (jsonb_typeof(files) = 'array' AND jsonb_array_length(files) BETWEEN 1 AND 16)),
  CHECK (jsonb_typeof(notes) = 'array' AND jsonb_array_length(notes) <= 32 AND octet_length(notes::text) <= 16384),
  CHECK (package_sha256 IS NULL OR package_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (finished_at >= started_at)
);

DO $$ BEGIN
  ALTER TABLE enrichedlab_agent.broker_analysis_catalogs ENABLE ROW LEVEL SECURITY;
  ALTER TABLE enrichedlab_agent.broker_analysis_requests ENABLE ROW LEVEL SECURITY;
  ALTER TABLE enrichedlab_agent.broker_analysis_receipts ENABLE ROW LEVEL SECURITY;
  REVOKE ALL ON enrichedlab_agent.broker_analysis_catalogs FROM PUBLIC;
  REVOKE ALL ON enrichedlab_agent.broker_analysis_requests FROM PUBLIC;
  REVOKE ALL ON enrichedlab_agent.broker_analysis_receipts FROM PUBLIC;
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_analysis_requests;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_analysis_requests
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_analysis_receipts;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_analysis_receipts
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
END $$;

-- Worker: the service's analysis features, checked as migration 057 checks figure features, so the broker checks every
-- setting a person can send. The catalog's identity is the digest of its stored JSON.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_analysis_catalog(p_tenant text, p_worker text,
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
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_catalog_invalid';
  END IF;
  FOR f IN SELECT value FROM jsonb_array_elements(p_features) LOOP
    IF jsonb_typeof(f) IS DISTINCT FROM 'object' OR coalesce(f->>'id', '') !~ '^[a-z][a-z0-9_]{1,63}$'
      OR jsonb_typeof(f->'label') IS DISTINCT FROM 'string' OR length(f->>'label') NOT BETWEEN 1 AND 120
      OR jsonb_typeof(f->'params') IS DISTINCT FROM 'object'
      OR EXISTS (SELECT 1 FROM jsonb_object_keys(f) x WHERE x NOT IN ('id', 'label', 'description', 'params')) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_catalog_invalid';
    END IF;
    FOR k, s IN SELECT key, value FROM jsonb_each(f->'params') LOOP
      IF k !~ '^[a-z][a-z0-9_]{0,63}$' OR jsonb_typeof(s) IS DISTINCT FROM 'object'
        OR coalesce(s->>'type', '') NOT IN ('column', 'columns', 'enum', 'number', 'numbers', 'string', 'colors')
        OR (s->>'type' = 'colors' AND (jsonb_typeof(s->'of') IS DISTINCT FROM 'string'
          OR f->'params'->(s->>'of')->>'type' IS DISTINCT FROM 'column'
          OR jsonb_typeof(s->'choices') IS DISTINCT FROM 'array'
          OR jsonb_array_length(s->'choices') NOT BETWEEN 1 AND 64
          OR EXISTS (SELECT 1 FROM jsonb_array_elements(s->'choices') c
            WHERE jsonb_typeof(c) IS DISTINCT FROM 'string' OR c #>> '{}' !~ '^#[0-9A-Fa-f]{6}$')))
        OR (s ? 'maxlen' AND CASE WHEN jsonb_typeof(s->'maxlen') = 'number'
          THEN (s->'maxlen')::numeric NOT BETWEEN 1 AND 9999
            OR (s->'maxlen')::numeric <> trunc((s->'maxlen')::numeric)
          ELSE true END) THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_catalog_invalid';
      END IF;
    END LOOP;
  END LOOP;
  -- The identity covers the service that declared the features, so an upgrade that keeps them is a new observation
  -- with its own version and image, never the old one seen again.
  v_sha := encode(sha256(convert_to(jsonb_build_object('features', p_features, 'service_version', p_version,
    'image_digest', p_image_digest)::text, 'UTF8')), 'hex');
  INSERT INTO enrichedlab_agent.broker_analysis_catalogs (tenant_id, catalog_sha256, service_version, image_digest, features)
  VALUES (p_tenant, v_sha, p_version, p_image_digest, p_features)
  ON CONFLICT (tenant_id, catalog_sha256) DO UPDATE SET last_seen_at = clock_timestamp();
  RETURN v_sha;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_analysis_catalog(text, text, text, text, jsonb) FROM PUBLIC;

-- API: the analysis catalog the worker read most recently, for any enabled principal of the tenant, with the labs the
-- reader belongs to or holds a project in that have exactly one enabled template-analysis method; null before the
-- worker's first read.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_analysis_catalog(p_tenant text, p_actor uuid)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object('catalog_sha256', c.catalog_sha256, 'service_version', c.service_version,
    'image_digest', c.image_digest, 'features', c.features, 'seen_at', c.last_seen_at,
    'groups', coalesce((SELECT jsonb_agg(f.group_key ORDER BY f.group_key) FROM (
        SELECT g.group_key FROM enrichedlab_agent.broker_group_profile_grants g
        JOIN enrichedlab_agent.broker_agent_profiles p ON p.tenant_id = g.tenant_id AND p.profile_key = g.profile_key
        WHERE g.tenant_id = p_tenant AND p.enabled AND p.route = 'server-analysis'
          AND (EXISTS (SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
                 WHERE m.tenant_id = p_tenant AND m.group_key = g.group_key AND m.principal_id = p_actor)
            OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_grants pg
                 JOIN enrichedlab_agent.broker_projects pr ON pr.tenant_id = pg.tenant_id AND pr.project_id = pg.project_id
                 WHERE pg.tenant_id = p_tenant AND pg.principal_id = p_actor AND pr.group_key = g.group_key))
        GROUP BY g.group_key HAVING count(*) = 1) f), '[]'::jsonb))
  FROM enrichedlab_agent.broker_analysis_catalogs c
  WHERE c.tenant_id = p_tenant
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.broker_principals a
      WHERE a.tenant_id = p_tenant AND a.principal_id = p_actor AND a.enabled)
  ORDER BY c.last_seen_at DESC, c.catalog_sha256
  LIMIT 1
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_analysis_catalog(text, uuid) FROM PUBLIC;

-- The checks of an analysis request, as migration 057 checks a figure request: the person may contribute to the
-- project, the source is a finished analysis of it, the table is one its retained package's digest lists and is small
-- enough, the lab has exactly one template-analysis method, the feature is in the newest analysis catalog, each setting
-- is one the feature declares with a value of its kind, and every required column is named. Returns what they found.
-- A research lead's step names the result it was shown the table in and the catalog it chose from (migration 060): the
-- table must be that result's and the newest catalog that one (`analysis_catalog_changed` otherwise).
DROP FUNCTION IF EXISTS enrichedlab_agent.broker_check_analysis_request(text, uuid, uuid, uuid, text, text, jsonb, text);
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_check_analysis_request(p_tenant text, p_actor uuid, p_project uuid,
  p_source_task uuid, p_table_path text, p_feature text, p_settings jsonb, p_title text, p_artifact uuid DEFAULT NULL,
  p_catalog_sha256 text DEFAULT NULL)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
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
  v_catalog enrichedlab_agent.broker_analysis_catalogs%ROWTYPE;
  v_feature jsonb;
  v_params jsonb;
  v_profile text;
  v_profiles integer;
  v_title text := btrim(coalesce(p_title, ''));
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
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:analysis_source_not_ready';
  END IF;
  SELECT a.artifact_id, a.sha256, e INTO v_artifact, v_package, v_table
    FROM enrichedlab_agent.broker_artifacts a
    JOIN enrichedlab_agent.broker_artifact_digests d
      ON d.tenant_id = a.tenant_id AND d.artifact_id = a.artifact_id AND d.artifact_sha256 = a.sha256
    CROSS JOIN LATERAL jsonb_array_elements(coalesce(d.digest->'tables', '[]'::jsonb)) e
    WHERE a.tenant_id = p_tenant AND a.task_id = p_source_task AND a.role = 'result' AND a.visibility = 'researcher'
      AND d.status = 'summarized' AND e->>'path' = p_table_path AND (p_artifact IS NULL OR a.artifact_id = p_artifact)
    ORDER BY d.recorded_at DESC
    LIMIT 1;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_table_unknown';
  END IF;
  v_rows := (v_table->>'rows')::bigint;
  v_columns := v_table->'columns';
  IF v_rows IS NULL OR v_rows < 1 OR v_rows > 50000 OR v_table ? 'columns_omitted'
    OR jsonb_typeof(v_columns) IS DISTINCT FROM 'array' OR jsonb_array_length(v_columns) NOT BETWEEN 1 AND 64
    OR coalesce(v_table->>'sha256', '') !~ '^[0-9a-f]{64}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_table_unsuitable';
  END IF;
  SELECT count(*), min(p.profile_key) INTO v_profiles, v_profile
    FROM enrichedlab_agent.broker_agent_profiles p
    JOIN enrichedlab_agent.broker_group_profile_grants g ON g.tenant_id = p.tenant_id AND g.profile_key = p.profile_key
    WHERE p.tenant_id = p_tenant AND p.enabled AND p.route = 'server-analysis' AND g.group_key = v_group;
  IF v_profiles <> 1 THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:analysis_method_unavailable';
  END IF;
  SELECT c.* INTO v_catalog FROM enrichedlab_agent.broker_analysis_catalogs c WHERE c.tenant_id = p_tenant
    ORDER BY c.last_seen_at DESC, c.catalog_sha256 LIMIT 1;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:analysis_catalog_unavailable';
  END IF;
  IF p_catalog_sha256 IS NOT NULL AND v_catalog.catalog_sha256 IS DISTINCT FROM p_catalog_sha256 THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:analysis_catalog_changed';
  END IF;
  SELECT f INTO v_feature FROM jsonb_array_elements(v_catalog.features) f WHERE f->>'id' = p_feature;
  IF v_feature IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_feature_unknown';
  END IF;
  v_params := v_feature->'params';
  IF jsonb_typeof(p_settings) IS DISTINCT FROM 'object' OR octet_length(p_settings::text) > 8192
    OR (SELECT count(*) FROM jsonb_object_keys(p_settings)) > 40 OR length(v_title) > 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_settings_invalid';
  END IF;
  FOR k, v IN SELECT key, value FROM jsonb_each(p_settings) LOOP
    s := v_params->k;
    IF s IS NULL THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_settings_invalid';
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
      WHEN 'string' THEN jsonb_typeof(v) = 'string' AND length(v #>> '{}') <= least(200,
        CASE WHEN jsonb_typeof(s->'maxlen') = 'number' AND (s->'maxlen')::numeric BETWEEN 1 AND 9999
          THEN floor((s->'maxlen')::numeric)::integer ELSE 200 END)
      WHEN 'colors' THEN jsonb_typeof(v) = 'object' AND (SELECT count(*) FROM jsonb_object_keys(v)) BETWEEN 1 AND 12
        AND NOT EXISTS (SELECT 1 FROM jsonb_each(v) e WHERE length(e.key) NOT BETWEEN 1 AND 200
          OR jsonb_typeof(e.value) <> 'string' OR NOT EXISTS (SELECT 1
            FROM jsonb_array_elements(coalesce(s->'choices', '[]'::jsonb)) c WHERE lower(c #>> '{}') = lower(e.value #>> '{}')))
      ELSE false END;
    IF v_ok IS NOT TRUE THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_settings_invalid';
    END IF;
  END LOOP;
  -- Every required column is named, so an analysis never rests on the service guessing one.
  FOR k, s IN SELECT key, value FROM jsonb_each(v_params) LOOP
    IF (s->>'required')::boolean IS TRUE AND s->>'type' IN ('column', 'columns') AND NOT p_settings ? k THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_settings_invalid';
    END IF;
  END LOOP;
  RETURN jsonb_build_object('group', v_group, 'profile', v_profile, 'artifact', v_artifact, 'package', v_package,
    'table_sha256', v_table->>'sha256', 'rows', v_rows, 'columns', v_columns, 'catalog_sha256', v_catalog.catalog_sha256,
    'feature_label', v_feature->>'label', 'title', v_title);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_check_analysis_request(text, uuid, uuid, uuid, text, text, jsonb, text,
  uuid, text) FROM PUBLIC;

-- API: a person's request for a template analysis: the checks, the previous analysis it changes, then the task and its
-- record in one transaction. The same request id with the same request returns the same task. A repeat is judged on
-- what the person sent, under the project's lock and their current right to contribute; it keeps the catalog and
-- source it pinned, so a catalog the worker read since, or a feature it no longer offers, does not change the answer.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_analysis(p_tenant text, p_actor uuid, p_project uuid,
  p_source_task uuid, p_table_path text, p_feature text, p_settings jsonb, p_title text, p_previous uuid,
  p_request_id uuid)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  c jsonb;
  v_title text;
  v_request jsonb;
  v_sha text;
  v_task uuid;
  v_objective text;
  v_prior enrichedlab_agent.broker_analysis_requests%ROWTYPE;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = p_project FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'contribute') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  SELECT r.* INTO v_prior FROM enrichedlab_agent.broker_project_tasks pt
    JOIN enrichedlab_agent.broker_analysis_requests r ON r.tenant_id = pt.tenant_id AND r.task_id = pt.task_id
    WHERE pt.tenant_id = p_tenant AND pt.project_id = p_project AND pt.action_key = 'analysis-' || p_request_id::text;
  IF FOUND THEN
    IF v_prior.source_task_id IS DISTINCT FROM p_source_task
      OR v_prior.table_path IS DISTINCT FROM p_table_path OR v_prior.feature IS DISTINCT FROM p_feature
      OR v_prior.settings IS DISTINCT FROM p_settings OR v_prior.title IS DISTINCT FROM btrim(coalesce(p_title, ''))
      OR v_prior.previous_task_id IS DISTINCT FROM p_previous THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
    RETURN v_prior.task_id;
  END IF;
  c := enrichedlab_agent.broker_check_analysis_request(p_tenant, p_actor, p_project, p_source_task, p_table_path,
    p_feature, p_settings, p_title);
  v_title := c->>'title';
  IF p_previous IS NOT NULL AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_analysis_requests r
      WHERE r.tenant_id = p_tenant AND r.task_id = p_previous AND r.project_id = p_project
        AND r.source_task_id = p_source_task AND r.table_path = p_table_path) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_previous_invalid';
  END IF;
  v_request := jsonb_build_object('schema', 'enrichedlab.template-analysis-request.v1', 'project', p_project,
    'source_task', p_source_task, 'source_package_sha256', c->>'package', 'table_path', p_table_path,
    'table_sha256', c->>'table_sha256', 'feature', p_feature, 'settings', p_settings, 'title', v_title,
    'previous', p_previous, 'catalog_sha256', c->>'catalog_sha256');
  v_sha := encode(sha256(convert_to(v_request::text, 'UTF8')), 'hex');
  v_objective := CASE WHEN v_title <> '' THEN v_title
    ELSE (c->>'feature_label') || ' of ' || replace(regexp_replace(regexp_replace(p_table_path, '^.*/', ''),
      '\.[A-Za-z0-9]+$', ''), '_', ' ') END;
  -- The person submission refuses the analysis route, so the request submits the project task and its origin itself.
  v_task := enrichedlab_agent.broker_submit_project_task(p_tenant, p_actor, p_project,
    'analysis-' || p_request_id::text, p_source_task, c->>'group', c->>'profile', v_objective,
    'analysis-' || p_request_id::text, v_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id, task_id, origin, basis, principal_id)
    VALUES (p_tenant, v_task, 'person', 'submission', p_actor) ON CONFLICT (tenant_id, task_id) DO NOTHING;
  INSERT INTO enrichedlab_agent.broker_analysis_requests (tenant_id, task_id, project_id, source_task_id,
    source_artifact_id, source_package_sha256, table_path, table_sha256, table_rows, table_columns, feature, settings,
    title, previous_task_id, catalog_sha256, request, request_sha256, principal_id)
  VALUES (p_tenant, v_task, p_project, p_source_task, (c->>'artifact')::uuid, c->>'package', p_table_path,
    c->>'table_sha256', (c->>'rows')::bigint, c->'columns', p_feature, p_settings, v_title, p_previous,
    c->>'catalog_sha256', v_request, v_sha, p_actor)
  ON CONFLICT (tenant_id, task_id) DO NOTHING;
  RETURN v_task;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_request_analysis(text, uuid, uuid, uuid, text, text, jsonb, text, uuid,
  uuid) FROM PUBLIC;

-- Worker: the request of its current, unexpired lease with the source package's place in the artifact store.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_analysis(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object('source_task', r.source_task_id, 'package_path', a.relative_path,
    'package_sha256', r.source_package_sha256, 'package_bytes', a.size_bytes, 'table_path', r.table_path,
    'table_sha256', r.table_sha256, 'table_rows', r.table_rows, 'feature', r.feature, 'settings', r.settings,
    'title', r.title, 'request_sha256', r.request_sha256, 'catalog_sha256', r.catalog_sha256)
  FROM enrichedlab_agent.broker_tasks t
  JOIN enrichedlab_agent.broker_analysis_requests r USING (tenant_id, task_id)
  JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id = r.tenant_id AND a.artifact_id = r.source_artifact_id
  WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
    AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running') AND t.desired_status = 'run'
    AND t.lease_expires_at > CURRENT_TIMESTAMP
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_analysis(text, uuid, text, integer, bigint) FROM PUBLIC;

-- Worker: a figure's receipt, as migration 040 recorded it, now also while a cancellation the worker is answering
-- waits for it, and with every field compared on a repeat. Before this, a figure stopped for a cancellation could not
-- record its receipt, so the task stayed cancelling until its lease ran out.
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
  PERFORM 1 FROM enrichedlab_agent.broker_tasks t
    JOIN enrichedlab_agent.broker_figure_requests r USING (tenant_id, task_id)
    WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
      AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running', 'cancelling')
      AND t.lease_expires_at > clock_timestamp()
    FOR UPDATE OF t;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_finished < p_started OR p_started > clock_timestamp() + interval '1 minute' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_receipt_invalid';
  END IF;
  SELECT * INTO v FROM enrichedlab_agent.broker_figure_receipts
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = p_attempt AND lease_generation = p_generation;
  IF FOUND THEN
    IF v.status IS DISTINCT FROM p_status OR v.error_code IS DISTINCT FROM p_error OR v.message IS DISTINCT FROM p_message
      OR v.service_version IS DISTINCT FROM p_service_version OR v.image_digest IS DISTINCT FROM p_image_digest
      OR v.files IS DISTINCT FROM p_files OR v.notes IS DISTINCT FROM coalesce(p_notes, '[]'::jsonb)
      OR v.package_sha256 IS DISTINCT FROM p_package_sha256 OR v.started_at IS DISTINCT FROM p_started
      OR v.finished_at IS DISTINCT FROM p_finished THEN
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

-- Worker: one receipt per attempt and lease, under that unexpired lease, also while a cancellation the worker is
-- answering waits for it. The same receipt again, every field equal, is accepted; any difference is refused.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_analysis_receipt(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint, p_status text, p_error text, p_message text, p_refusal jsonb,
  p_service_version text, p_image_digest text, p_method text, p_files jsonb, p_notes jsonb, p_package_sha256 text,
  p_started timestamptz, p_finished timestamptz)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v enrichedlab_agent.broker_analysis_receipts%ROWTYPE;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_tasks t
    JOIN enrichedlab_agent.broker_analysis_requests r USING (tenant_id, task_id)
    WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
      AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running', 'cancelling')
      AND t.lease_expires_at > clock_timestamp()
    FOR UPDATE OF t;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_finished < p_started OR p_started > clock_timestamp() + interval '1 minute' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_receipt_invalid';
  END IF;
  SELECT * INTO v FROM enrichedlab_agent.broker_analysis_receipts
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = p_attempt AND lease_generation = p_generation;
  IF FOUND THEN
    IF v.status IS DISTINCT FROM p_status OR v.error_code IS DISTINCT FROM p_error OR v.message IS DISTINCT FROM p_message
      OR v.refusal IS DISTINCT FROM p_refusal OR v.service_version IS DISTINCT FROM p_service_version
      OR v.image_digest IS DISTINCT FROM p_image_digest OR v.method IS DISTINCT FROM p_method
      OR v.files IS DISTINCT FROM p_files OR v.notes IS DISTINCT FROM coalesce(p_notes, '[]'::jsonb)
      OR v.package_sha256 IS DISTINCT FROM p_package_sha256 OR v.started_at IS DISTINCT FROM p_started
      OR v.finished_at IS DISTINCT FROM p_finished THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
  ELSE
    INSERT INTO enrichedlab_agent.broker_analysis_receipts (tenant_id, task_id, attempt, lease_generation, status,
      error_code, message, refusal, service_version, image_digest, method, files, notes, package_sha256, started_at,
      finished_at)
    VALUES (p_tenant, p_task, p_attempt, p_generation, p_status, p_error, p_message, p_refusal, p_service_version,
      p_image_digest, p_method, p_files, coalesce(p_notes, '[]'::jsonb), p_package_sha256, p_started, p_finished)
    RETURNING * INTO v;
  END IF;
  RETURN jsonb_build_object('status', v.status, 'recorded_at', v.recorded_at);
EXCEPTION WHEN check_violation THEN
  RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analysis_receipt_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_analysis_receipt(text, uuid, text, integer, bigint, text, text,
  text, jsonb, text, text, text, jsonb, jsonb, text, timestamptz, timestamptz) FROM PUBLIC;

-- API: a template analysis's request, its source analysis as a scientist reads it and every receipt, for anyone who
-- may read the analysis; null for a task that is not a template analysis.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_analysis_detail(p_tenant text, p_actor uuid, p_task uuid)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  r enrichedlab_agent.broker_analysis_requests%ROWTYPE;
  v_source_title text;
BEGIN
  IF NOT enrichedlab_agent.broker_task_can(p_tenant, p_actor, p_task, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  SELECT * INTO r FROM enrichedlab_agent.broker_analysis_requests WHERE tenant_id = p_tenant AND task_id = p_task;
  IF NOT FOUND THEN
    RETURN NULL;
  END IF;
  SELECT t.objective INTO v_source_title FROM enrichedlab_agent.broker_tasks t
    WHERE t.tenant_id = p_tenant AND t.task_id = r.source_task_id;
  RETURN jsonb_build_object('task_id', r.task_id, 'project_id', r.project_id, 'source_task', r.source_task_id,
    'source_title', v_source_title, 'source_package_sha256', r.source_package_sha256, 'table_path', r.table_path,
    'table_sha256', r.table_sha256, 'table_rows', r.table_rows, 'table_columns', r.table_columns,
    'feature', r.feature, 'settings', r.settings, 'title', r.title, 'previous', r.previous_task_id,
    'catalog_sha256', r.catalog_sha256, 'request_sha256', r.request_sha256, 'created_at', r.created_at,
    'receipts', coalesce((SELECT jsonb_agg(jsonb_build_object('attempt', x.attempt, 'status', x.status,
        'error_code', x.error_code, 'message', x.message, 'refusal', x.refusal, 'service_version', x.service_version,
        'image_digest', x.image_digest, 'method', x.method, 'files', x.files, 'notes', x.notes,
        'package_sha256', x.package_sha256, 'started_at', x.started_at, 'finished_at', x.finished_at)
        ORDER BY x.attempt, x.lease_generation)
      FROM enrichedlab_agent.broker_analysis_receipts x WHERE x.tenant_id = p_tenant AND x.task_id = p_task), '[]'::jsonb));
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_analysis_detail(text, uuid, uuid) FROM PUBLIC;

-- API: the template analyses made from one analysis, newest first, for anyone who may read that analysis. One the reader
-- may not read is left out.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_analysis_versions(p_tenant text, p_actor uuid, p_source_task uuid)
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
    FROM enrichedlab_agent.broker_analysis_requests r
    JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = r.tenant_id AND t.task_id = r.task_id
    WHERE r.tenant_id = p_tenant AND r.source_task_id = p_source_task
      AND enrichedlab_agent.broker_task_can(p_tenant, p_actor, r.task_id, 'read')), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_analysis_versions(text, uuid, uuid) FROM PUBLIC;

-- The template-analysis method, like the figure method, runs only from a person's request: the profile choices mark it
-- not offered, person submissions refuse it and a research run cannot be given it (migration 040 for figures).
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_profile_choices(p_tenant text, p_principal uuid)
RETURNS TABLE(profile_key text, name text, description text, tool_bundle text, max_run_seconds integer, offered boolean)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT DISTINCT profiles.profile_key, profiles.name, profiles.description,
         profiles.tool_bundle, profiles.max_run_seconds, profiles.route NOT IN ('server-render', 'server-analysis')
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
      WHERE tenant_id=p_tenant AND profile_key=p_profile AND route IN ('server-render','server-analysis')) THEN
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
      WHERE tenant_id=p_tenant AND profile_key=p_profile AND route IN ('server-render','server-analysis')) THEN
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
        WHERE p.tenant_id=p_tenant AND p.project_id=p_project AND f.profile_key=k AND f.enabled
          AND f.route NOT IN ('server-render','server-analysis')
      )
    )
$$;

-- The project's task items (migration 054) with one addition: a template analysis names its feature, source analysis,
-- table and the analysis it changes, so the project's contents can title it.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_task_items(p_tenant text, p_project uuid,
  p_after_submitted timestamptz, p_after_task uuid, p_limit integer)
RETURNS SETOF jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
  WITH line AS (
    SELECT q.task_id, row_number() OVER (ORDER BY q.submitted_at, q.task_id) - 1 AS ahead
    FROM enrichedlab_agent.broker_tasks q
    JOIN enrichedlab_agent.broker_research_groups qg ON qg.tenant_id = q.tenant_id AND qg.group_key = q.group_key
    WHERE q.tenant_id = p_tenant AND q.status = 'queued' AND q.desired_status = 'run'
      AND q.attempt < q.max_task_attempts AND qg.enabled
      AND enrichedlab_agent.broker_execution_allowed(q.tenant_id, q.task_id)
  )
  SELECT to_jsonb(t) FROM (
    SELECT t.task_id, pt.action_key, pt.parent_task_id, t.profile_key AS profile, t.status, t.attempt, t.updated_at,
      t.submitted_at,
      CASE WHEN fr.task_id IS NULL THEN NULL ELSE jsonb_build_object('title', t.objective, 'feature', fr.feature,
        'source_task', fr.source_task_id, 'table_path', fr.table_path, 'previous', fr.previous_task_id) END AS figure,
      CASE WHEN ar.task_id IS NULL THEN NULL ELSE jsonb_build_object('title', t.objective, 'feature', ar.feature,
        'source_task', ar.source_task_id, 'table_path', ar.table_path, 'previous', ar.previous_task_id)
        END AS template_analysis,
      CASE WHEN t.status = 'queued' AND t.desired_status = 'run' THEN jsonb_build_object(
        'ahead', line.ahead,
        'lab_full', coalesce((SELECT count(*) FROM enrichedlab_agent.broker_tasks a
          WHERE a.tenant_id = t.tenant_id AND a.group_key = t.group_key
            AND a.status IN ('leased', 'starting', 'running', 'cancelling') AND a.lease_expires_at >= now())
          >= g.max_active_agents, false),
        'person_full', coalesce((SELECT count(*) FROM enrichedlab_agent.broker_tasks a
          WHERE a.tenant_id = t.tenant_id AND a.group_key = t.group_key AND a.principal_id = t.principal_id
            AND a.status IN ('leased', 'starting', 'running', 'cancelling') AND a.lease_expires_at >= now())
          >= g.max_active_agents_per_principal, false)) END AS queue
    FROM enrichedlab_agent.broker_project_tasks pt
    JOIN enrichedlab_agent.broker_tasks t USING (tenant_id, task_id)
    LEFT JOIN enrichedlab_agent.broker_figure_requests fr ON fr.tenant_id = pt.tenant_id AND fr.task_id = pt.task_id
    LEFT JOIN enrichedlab_agent.broker_analysis_requests ar ON ar.tenant_id = pt.tenant_id AND ar.task_id = pt.task_id
    LEFT JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = t.tenant_id AND g.group_key = t.group_key
    LEFT JOIN line ON line.task_id = t.task_id
    WHERE pt.tenant_id = p_tenant AND pt.project_id = p_project
      AND (p_after_submitted IS NULL OR (t.submitted_at, t.task_id) < (p_after_submitted, p_after_task))
    ORDER BY t.submitted_at DESC, t.task_id DESC
    LIMIT p_limit + 1
  ) t;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_task_items(text, uuid, timestamptz, uuid, integer) FROM PUBLIC;

-- Task lineage (migration 053) with template analyses: like a figure version, a person's request is its basis, so it has
-- no plan step or workspace of its own; it read a prior output by digest, ran a reviewed service template (class
-- `vetted_service`, outputs `vetted_tool_output`), names the image its newest receipt records, and only a completed
-- analysis without files is a gap. The `analysis` record gives its source, table, feature, settings and service.
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
  ELSIF fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL THEN
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
  tool_class:=CASE WHEN fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL THEN 'vetted_service'
    WHEN q.task_id IS NOT NULL THEN 'guarded_query'
    WHEN g.task_id IS NOT NULL AND g.configuration IS NOT NULL THEN 'configured'
    WHEN g.task_id IS NOT NULL THEN 'agent_built' WHEN tool IS NOT NULL THEN 'vetted_service'
    WHEN has_template THEN 'agent_built' ELSE 'agent_session' END;
  IF has_template AND g.task_id IS NULL AND tool IS NULL THEN gaps:=gaps||'"code_not_recorded"'::jsonb; END IF;
  output_class:=CASE tool_class WHEN 'agent_built' THEN 'agent_built_tool_output' WHEN 'vetted_service' THEN 'vetted_tool_output'
    WHEN 'configured' THEN 'configured_tool_output' WHEN 'guarded_query' THEN 'query_output' ELSE 'agent_session_output' END;
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
  IF jsonb_array_length(outputs)=0 AND (CASE WHEN fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL THEN t.status='completed'
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
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (58) ON CONFLICT DO NOTHING;
COMMIT;
