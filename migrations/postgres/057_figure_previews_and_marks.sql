-- Figure previews and the manuscript mark.
--
-- A preview is a drawing a person sees before keeping a version. The request passes the checks a version request
-- passes, then the worker that reaches the render service draws it and the API hands the SVG to the person once. A
-- preview makes no task, package, version or history entry. Its row holds the checked request and, until it is handed
-- over or expires, the drawing; the sweep deletes every row two minutes after it was made, so the per-person limits can
-- count recent previews. The drawing passes through this database, so its write-ahead log backups hold it for their
-- retention, like any other broker record.
--
-- A figure is named by its first version. A person who may steer the project marks one completed version of a figure
-- as the one in the manuscript, or clears the mark. Marks and clearings are immutable events with project audit
-- entries; the figure record and the versions list name the current mark.
BEGIN;

-- The worker records the service's catalog as in migration 040, and now accepts the colors type: a setting that maps
-- each group of one column setting of the same chart to a color, from the colors the chart lists. A catalog that
-- declares any other type is still refused whole, so the broker checks every setting a person can send. A declared
-- `maxlen` must be a whole number from 1 to 9999 (10.0 counts as 10), so no limit is lost to how it was written.
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

-- The checks a version request and a preview share. Returns what they found; raises the request's refusal codes.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_check_figure_request(p_tenant text, p_actor uuid, p_project uuid,
  p_source_task uuid, p_table_path text, p_feature text, p_controls jsonb, p_title text)
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
  v_catalog enrichedlab_agent.broker_render_catalogs%ROWTYPE;
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
      -- A string the service declares a shorter limit for keeps to it; 200 characters otherwise.
      WHEN 'string' THEN jsonb_typeof(v) = 'string' AND length(v #>> '{}') <= least(200,
        CASE WHEN jsonb_typeof(s->'maxlen') = 'number' AND (s->'maxlen')::numeric BETWEEN 1 AND 9999
          THEN floor((s->'maxlen')::numeric)::integer ELSE 200 END)
      -- One color per group: at most twelve groups, each mapped to one of the colors the service accepts.
      WHEN 'colors' THEN jsonb_typeof(v) = 'object' AND (SELECT count(*) FROM jsonb_object_keys(v)) BETWEEN 1 AND 12
        AND NOT EXISTS (SELECT 1 FROM jsonb_each(v) e WHERE length(e.key) NOT BETWEEN 1 AND 200
          OR jsonb_typeof(e.value) <> 'string' OR NOT EXISTS (SELECT 1
            FROM jsonb_array_elements(coalesce(s->'choices', '[]'::jsonb)) c WHERE lower(c #>> '{}') = lower(e.value #>> '{}')))
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
  RETURN jsonb_build_object('group', v_group, 'profile', v_profile, 'artifact', v_artifact, 'package', v_package,
    'table_sha256', v_table->>'sha256', 'rows', v_rows, 'columns', v_columns, 'catalog_sha256', v_catalog.catalog_sha256,
    'feature_label', v_feature->>'label', 'title', v_title);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_check_figure_request(text, uuid, uuid, uuid, text, text, jsonb, text)
  FROM PUBLIC;

-- A version request: the shared checks, the previous version, then the task and its record.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_figure(p_tenant text, p_actor uuid, p_project uuid,
  p_source_task uuid, p_table_path text, p_feature text, p_controls jsonb, p_title text, p_previous uuid,
  p_request_id uuid)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  c jsonb;
  v_prev enrichedlab_agent.broker_figure_requests%ROWTYPE;
  v_title text;
  v_request jsonb;
  v_sha text;
  v_task uuid;
  v_objective text;
BEGIN
  c := enrichedlab_agent.broker_check_figure_request(p_tenant, p_actor, p_project, p_source_task, p_table_path,
    p_feature, p_controls, p_title);
  v_title := c->>'title';
  IF p_previous IS NOT NULL THEN
    SELECT r.* INTO v_prev FROM enrichedlab_agent.broker_figure_requests r
      WHERE r.tenant_id = p_tenant AND r.task_id = p_previous AND r.project_id = p_project
        AND r.source_task_id = p_source_task AND r.table_path = p_table_path;
    IF NOT FOUND THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_previous_invalid';
    END IF;
  END IF;
  v_request := jsonb_build_object('schema', 'enrichedlab.figure-request.v1', 'project', p_project,
    'source_task', p_source_task, 'source_package_sha256', c->>'package', 'table_path', p_table_path,
    'table_sha256', c->>'table_sha256', 'feature', p_feature, 'controls', p_controls, 'title', v_title,
    'previous', p_previous, 'catalog_sha256', c->>'catalog_sha256');
  v_sha := encode(sha256(convert_to(v_request::text, 'UTF8')), 'hex');
  v_objective := CASE WHEN v_title <> '' THEN v_title
    ELSE (c->>'feature_label') || ' of ' || replace(regexp_replace(regexp_replace(p_table_path, '^.*/', ''),
      '\.[A-Za-z0-9]+$', ''), '_', ' ') END;
  -- The person submission refuses the figure route, so the request submits the project task and its origin itself.
  v_task := enrichedlab_agent.broker_submit_project_task(p_tenant, p_actor, p_project,
    'figure-' || p_request_id::text, p_source_task, c->>'group', c->>'profile', v_objective,
    'figure-' || p_request_id::text, v_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id, task_id, origin, basis, principal_id)
    VALUES (p_tenant, v_task, 'person', 'submission', p_actor) ON CONFLICT (tenant_id, task_id) DO NOTHING;
  INSERT INTO enrichedlab_agent.broker_figure_requests (tenant_id, task_id, project_id, source_task_id,
    source_artifact_id, source_package_sha256, table_path, table_sha256, table_rows, table_columns, feature, controls,
    title, previous_task_id, catalog_sha256, request, request_sha256, principal_id)
  VALUES (p_tenant, v_task, p_project, p_source_task, (c->>'artifact')::uuid, c->>'package', p_table_path,
    c->>'table_sha256', (c->>'rows')::bigint, c->'columns', p_feature, p_controls, v_title, p_previous,
    c->>'catalog_sha256', v_request, v_sha, p_actor)
  ON CONFLICT (tenant_id, task_id) DO NOTHING;
  RETURN v_task;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_request_figure(text, uuid, uuid, uuid, text, text, jsonb, text, uuid, uuid)
  FROM PUBLIC;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_figure_previews (
  tenant_id text NOT NULL,
  preview_id uuid NOT NULL DEFAULT gen_random_uuid(),
  principal_id uuid NOT NULL,
  project_id uuid NOT NULL,
  source_artifact_id uuid NOT NULL,
  request jsonb NOT NULL,
  request_sha256 text NOT NULL,
  state text NOT NULL DEFAULT 'pending',
  worker_id text,
  svg bytea,
  svg_sha256 text,
  result jsonb,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  claimed_at timestamptz,
  finished_at timestamptz,
  expires_at timestamptz NOT NULL DEFAULT clock_timestamp() + interval '60 seconds',
  PRIMARY KEY (tenant_id, preview_id),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  FOREIGN KEY (tenant_id, source_artifact_id) REFERENCES enrichedlab_agent.broker_artifacts (tenant_id, artifact_id)
    ON DELETE CASCADE,
  CHECK (state IN ('pending', 'drawing', 'drawn', 'refused', 'failed', 'delivered')),
  CHECK (jsonb_typeof(request) = 'object'),
  CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (svg IS NULL OR octet_length(svg) BETWEEN 1 AND 2097152),
  CHECK ((svg IS NULL) = (svg_sha256 IS NULL)),
  CHECK (svg_sha256 IS NULL OR svg_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK ((state = 'drawn') = (svg IS NOT NULL)),
  CHECK ((state IN ('pending', 'drawing')) = (finished_at IS NULL)),
  CHECK (result IS NULL OR (jsonb_typeof(result) = 'object' AND octet_length(result::text) <= 65536)),
  CHECK (worker_id IS NULL OR worker_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$')
);
CREATE INDEX IF NOT EXISTS broker_figure_previews_person
  ON enrichedlab_agent.broker_figure_previews (tenant_id, principal_id, created_at);
CREATE INDEX IF NOT EXISTS broker_figure_previews_pending
  ON enrichedlab_agent.broker_figure_previews (tenant_id, created_at) WHERE state = 'pending';

-- API: a preview request. The checks a version request passes, then at most one open preview and 30 a minute per
-- person (`capacity_reached`). Returns the preview's id; the worker that reaches the render service draws it.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_figure_preview(p_tenant text, p_actor uuid, p_project uuid,
  p_source_task uuid, p_table_path text, p_feature text, p_controls jsonb, p_title text)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  c jsonb;
  v_request jsonb;
  v_preview uuid;
BEGIN
  c := enrichedlab_agent.broker_check_figure_request(p_tenant, p_actor, p_project, p_source_task, p_table_path,
    p_feature, p_controls, p_title);
  -- One person's previews are counted one request at a time.
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-figure-preview:' || p_tenant || ':' || p_actor::text, 0));
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_previews
      WHERE tenant_id = p_tenant AND principal_id = p_actor AND state IN ('pending', 'drawing')
        AND expires_at > clock_timestamp())
    OR (SELECT count(*) FROM enrichedlab_agent.broker_figure_previews
      WHERE tenant_id = p_tenant AND principal_id = p_actor
        AND created_at > clock_timestamp() - interval '1 minute') >= 30 THEN
    RAISE EXCEPTION USING ERRCODE = '53400', MESSAGE = 'broker:capacity_reached';
  END IF;
  v_request := jsonb_build_object('schema', 'enrichedlab.figure-preview.v1', 'project', p_project,
    'source_task', p_source_task, 'source_package_sha256', c->>'package', 'table_path', p_table_path,
    'table_sha256', c->>'table_sha256', 'table_rows', (c->>'rows')::bigint, 'feature', p_feature,
    'controls', p_controls, 'title', c->>'title', 'catalog_sha256', c->>'catalog_sha256');
  INSERT INTO enrichedlab_agent.broker_figure_previews (tenant_id, principal_id, project_id, source_artifact_id,
    request, request_sha256)
  VALUES (p_tenant, p_actor, p_project, (c->>'artifact')::uuid, v_request,
    encode(sha256(convert_to(v_request::text, 'UTF8')), 'hex'))
  RETURNING preview_id INTO v_preview;
  RETURN v_preview;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_request_figure_preview(text, uuid, uuid, uuid, text, text, jsonb, text)
  FROM PUBLIC;

-- Whether a person may still have a preview of a project's table: what a preview request checks of them, now. A
-- worker claiming a preview and the hand-over of its drawing ask again, so a person removed from the project, or
-- disabled, while a preview waits or is drawn gets nothing.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_figure_preview_permitted(p_tenant text, p_actor uuid,
  p_project uuid, p_source_task uuid)
RETURNS boolean
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'contribute')
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_tasks pt
      WHERE pt.tenant_id = p_tenant AND pt.project_id = p_project AND pt.task_id = p_source_task)
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_figure_preview_permitted(text, uuid, uuid, uuid) FROM PUBLIC;

-- Worker: the oldest waiting preview, claimed by this worker, in the shape of a version's figure request with the
-- source package's place in the artifact store. Null when none waits. A preview whose requester may no longer have it
-- ends without being drawn.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_claim_figure_preview(p_tenant text, p_worker text)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v enrichedlab_agent.broker_figure_previews%ROWTYPE;
  v_path text;
  v_bytes bigint;
BEGIN
  IF p_worker IS NULL OR p_worker !~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:lease_invalid';
  END IF;
  LOOP
    SELECT * INTO v FROM enrichedlab_agent.broker_figure_previews
      WHERE tenant_id = p_tenant AND state = 'pending' AND expires_at > clock_timestamp()
      ORDER BY created_at, preview_id
      LIMIT 1
      FOR UPDATE SKIP LOCKED;
    IF NOT FOUND THEN
      RETURN NULL;
    END IF;
    EXIT WHEN enrichedlab_agent.broker_figure_preview_permitted(p_tenant, v.principal_id, v.project_id,
      (v.request->>'source_task')::uuid);
    UPDATE enrichedlab_agent.broker_figure_previews
      SET state = 'failed', result = jsonb_build_object('code', 'figure_preview_not_permitted'),
        finished_at = clock_timestamp()
      WHERE tenant_id = p_tenant AND preview_id = v.preview_id;
  END LOOP;
  SELECT a.relative_path, a.size_bytes INTO v_path, v_bytes FROM enrichedlab_agent.broker_artifacts a
    WHERE a.tenant_id = p_tenant AND a.artifact_id = v.source_artifact_id;
  UPDATE enrichedlab_agent.broker_figure_previews
    SET state = 'drawing', worker_id = p_worker, claimed_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND preview_id = v.preview_id;
  RETURN jsonb_build_object('preview_id', v.preview_id, 'figure', jsonb_build_object(
    'source_task', v.request->'source_task', 'package_path', v_path,
    'package_sha256', v.request->'source_package_sha256', 'package_bytes', v_bytes,
    'table_path', v.request->'table_path', 'table_sha256', v.request->'table_sha256',
    'table_rows', v.request->'table_rows', 'feature', v.request->'feature', 'controls', v.request->'controls',
    'title', v.request->'title', 'request_sha256', v.request_sha256,
    'catalog_sha256', v.request->'catalog_sha256'));
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_claim_figure_preview(text, text) FROM PUBLIC;

-- Worker: what the claimed preview came to. `drawn` carries the SVG, whose digest the database computes; `refused` and
-- `failed` carry a result only. Only the worker that claimed it, before it expires, records it. True when recorded.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_finish_figure_preview(p_tenant text, p_worker text, p_preview uuid,
  p_state text, p_svg bytea, p_result jsonb)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF p_state NOT IN ('drawn', 'refused', 'failed') OR (p_state = 'drawn') <> (p_svg IS NOT NULL)
    OR jsonb_typeof(p_result) IS DISTINCT FROM 'object' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_preview_invalid';
  END IF;
  IF p_svg IS NOT NULL AND octet_length(p_svg) > 2097152 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:figure_preview_too_large';
  END IF;
  UPDATE enrichedlab_agent.broker_figure_previews
    SET state = p_state, svg = p_svg, svg_sha256 = CASE WHEN p_svg IS NULL THEN NULL ELSE encode(sha256(p_svg), 'hex') END,
      result = p_result, finished_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND preview_id = p_preview AND state = 'drawing' AND worker_id = p_worker
      AND expires_at > clock_timestamp();
  RETURN FOUND;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_finish_figure_preview(text, text, uuid, text, bytea, jsonb) FROM PUBLIC;

-- API: the requester's preview. While it waits: `{"state": "waiting"}`. Once drawn, refused or failed: the result, with
-- the drawing as base64, handed over once; the row keeps no drawing after that. An expired preview reads `expired`. A
-- requester who may no longer have it reads `not_permitted`, and the preview ends with its drawing erased.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_take_figure_preview(p_tenant text, p_actor uuid, p_preview uuid)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v enrichedlab_agent.broker_figure_previews%ROWTYPE;
BEGIN
  SELECT * INTO v FROM enrichedlab_agent.broker_figure_previews
    WHERE tenant_id = p_tenant AND preview_id = p_preview AND principal_id = p_actor
    FOR UPDATE;
  IF NOT FOUND OR v.state = 'delivered' THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:figure_preview_not_found';
  END IF;
  IF NOT enrichedlab_agent.broker_figure_preview_permitted(p_tenant, p_actor, v.project_id,
      (v.request->>'source_task')::uuid) THEN
    UPDATE enrichedlab_agent.broker_figure_previews
      SET state = 'failed', svg = NULL, svg_sha256 = NULL,
        result = jsonb_build_object('code', 'figure_preview_not_permitted'),
        finished_at = COALESCE(finished_at, clock_timestamp())
      WHERE tenant_id = p_tenant AND preview_id = p_preview;
    RETURN jsonb_build_object('state', 'not_permitted');
  END IF;
  IF v.state IN ('pending', 'drawing') THEN
    RETURN jsonb_build_object('state', CASE WHEN v.expires_at > clock_timestamp() THEN 'waiting' ELSE 'expired' END);
  END IF;
  UPDATE enrichedlab_agent.broker_figure_previews SET state = 'delivered', svg = NULL, svg_sha256 = NULL
    WHERE tenant_id = p_tenant AND preview_id = p_preview;
  RETURN jsonb_build_object('state', v.state, 'request', v.request, 'result', v.result,
    'svg_base64', CASE WHEN v.svg IS NULL THEN NULL ELSE translate(encode(v.svg, 'base64'), E'\n', '') END,
    'svg_sha256', v.svg_sha256, 'svg_bytes', octet_length(v.svg));
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_take_figure_preview(text, uuid, uuid) FROM PUBLIC;

-- API: the requester's preview when the request that asked for it ends before the drawing is handed over (the person
-- closed the editor, or the wait ran out). It stops counting as their open preview at once, a worker drawing it can no
-- longer record it, and a drawing not yet handed over is erased. True when it changed the preview.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_abandon_figure_preview(p_tenant text, p_actor uuid, p_preview uuid)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  UPDATE enrichedlab_agent.broker_figure_previews
    SET state = 'failed', svg = NULL, svg_sha256 = NULL, result = jsonb_build_object('code', 'figure_preview_abandoned'),
      finished_at = COALESCE(finished_at, clock_timestamp())
    WHERE tenant_id = p_tenant AND preview_id = p_preview AND principal_id = p_actor
      AND state IN ('pending', 'drawing', 'drawn');
  RETURN FOUND;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_abandon_figure_preview(text, uuid, uuid) FROM PUBLIC;

-- Worker: previews past their expiry stop waiting, and every preview made more than two minutes ago is deleted.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_sweep_figure_previews(p_tenant text)
RETURNS integer
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_deleted integer;
BEGIN
  UPDATE enrichedlab_agent.broker_figure_previews
    SET state = 'failed', result = jsonb_build_object('code', 'figure_preview_expired'), finished_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND state IN ('pending', 'drawing') AND expires_at <= clock_timestamp();
  DELETE FROM enrichedlab_agent.broker_figure_previews
    WHERE tenant_id = p_tenant AND created_at < clock_timestamp() - interval '2 minutes';
  GET DIAGNOSTICS v_deleted = ROW_COUNT;
  RETURN v_deleted;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_sweep_figure_previews(text) FROM PUBLIC;

-- The manuscript mark. A figure is its first version's task; every later version names the one before it.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_figure_manuscript_events (
  tenant_id text NOT NULL,
  event_id uuid NOT NULL DEFAULT gen_random_uuid(),
  project_id uuid NOT NULL,
  figure_task_id uuid NOT NULL,
  action text NOT NULL,
  version_task_id uuid,
  actor_id uuid NOT NULL,
  occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, event_id),
  FOREIGN KEY (tenant_id, figure_task_id) REFERENCES enrichedlab_agent.broker_figure_requests (tenant_id, task_id),
  FOREIGN KEY (tenant_id, version_task_id) REFERENCES enrichedlab_agent.broker_figure_requests (tenant_id, task_id),
  FOREIGN KEY (tenant_id, actor_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (action IN ('marked', 'cleared')),
  CHECK ((action = 'marked') = (version_task_id IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS broker_figure_manuscript_events_figure
  ON enrichedlab_agent.broker_figure_manuscript_events (tenant_id, figure_task_id, occurred_at DESC);

-- The first version of the figure a version belongs to, following each version's previous one.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_figure_root(p_tenant text, p_task uuid)
RETURNS uuid
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  WITH RECURSIVE chain(task_id, previous, depth) AS (
    SELECT r.task_id, r.previous_task_id, 0 FROM enrichedlab_agent.broker_figure_requests r
      WHERE r.tenant_id = p_tenant AND r.task_id = p_task
    UNION ALL
    SELECT r.task_id, r.previous_task_id, chain.depth + 1 FROM chain
      JOIN enrichedlab_agent.broker_figure_requests r ON r.tenant_id = p_tenant AND r.task_id = chain.previous
      WHERE chain.depth < 10000
  )
  SELECT task_id FROM chain WHERE previous IS NULL LIMIT 1
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_figure_root(text, uuid) FROM PUBLIC;

-- A figure's current mark, or null.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_figure_manuscript(p_tenant text, p_figure uuid)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT CASE WHEN e.action = 'marked' THEN jsonb_build_object('figure', e.figure_task_id, 'version', e.version_task_id,
      'marked_by', e.actor_id, 'marked_by_name', (SELECT p.display_name FROM enrichedlab_agent.broker_principals p
        WHERE p.tenant_id = e.tenant_id AND p.principal_id = e.actor_id), 'marked_at', e.occurred_at) END
  FROM enrichedlab_agent.broker_figure_manuscript_events e
  WHERE e.tenant_id = p_tenant AND e.figure_task_id = p_figure
  ORDER BY e.occurred_at DESC, e.event_id DESC
  LIMIT 1
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_figure_manuscript(text, uuid) FROM PUBLIC;

-- API: a person who may steer the project marks one completed version of a figure as the one in the manuscript.
-- Marking the version already marked records nothing new.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_mark_figure_manuscript(p_tenant text, p_actor uuid,
  p_project uuid, p_figure uuid, p_version uuid)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_current jsonb;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'steer') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests r WHERE r.tenant_id = p_tenant
      AND r.task_id = p_figure AND r.project_id = p_project AND r.previous_task_id IS NULL) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:figure_not_found';
  END IF;
  -- Marks of one figure are made one at a time.
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-figure-manuscript:' || p_tenant || ':' || p_figure::text, 0));
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests r WHERE r.tenant_id = p_tenant
      AND r.task_id = p_version AND r.project_id = p_project)
    OR enrichedlab_agent.broker_figure_root(p_tenant, p_version) IS DISTINCT FROM p_figure THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:version_not_in_figure';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks t
      JOIN enrichedlab_agent.broker_figure_receipts x ON x.tenant_id = t.tenant_id AND x.task_id = t.task_id
      WHERE t.tenant_id = p_tenant AND t.task_id = p_version AND t.status = 'completed'
        AND x.status = 'completed') THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:version_not_drawn';
  END IF;
  v_current := enrichedlab_agent.broker_figure_manuscript(p_tenant, p_figure);
  IF v_current IS NOT NULL AND (v_current->>'version')::uuid = p_version THEN
    RETURN v_current;
  END IF;
  INSERT INTO enrichedlab_agent.broker_figure_manuscript_events (tenant_id, project_id, figure_task_id, action,
    version_task_id, actor_id)
  VALUES (p_tenant, p_project, p_figure, 'marked', p_version, p_actor);
  INSERT INTO enrichedlab_agent.broker_project_audit (tenant_id, project_id, actor_id, kind, detail)
  VALUES (p_tenant, p_project, p_actor, 'figure_manuscript_marked',
    jsonb_build_object('figure', p_figure, 'version', p_version));
  -- Open pages of the project see the change through its event stream.
  UPDATE enrichedlab_agent.broker_projects SET updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND project_id = p_project;
  RETURN enrichedlab_agent.broker_figure_manuscript(p_tenant, p_figure);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_mark_figure_manuscript(text, uuid, uuid, uuid, uuid) FROM PUBLIC;

-- API: clears a figure's mark. False when nothing was marked, and then nothing is recorded.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_clear_figure_manuscript(p_tenant text, p_actor uuid,
  p_project uuid, p_figure uuid)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_current jsonb;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'steer') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests r WHERE r.tenant_id = p_tenant
      AND r.task_id = p_figure AND r.project_id = p_project AND r.previous_task_id IS NULL) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:figure_not_found';
  END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-figure-manuscript:' || p_tenant || ':' || p_figure::text, 0));
  v_current := enrichedlab_agent.broker_figure_manuscript(p_tenant, p_figure);
  IF v_current IS NULL THEN
    RETURN false;
  END IF;
  INSERT INTO enrichedlab_agent.broker_figure_manuscript_events (tenant_id, project_id, figure_task_id, action,
    version_task_id, actor_id)
  VALUES (p_tenant, p_project, p_figure, 'cleared', NULL, p_actor);
  INSERT INTO enrichedlab_agent.broker_project_audit (tenant_id, project_id, actor_id, kind, detail)
  VALUES (p_tenant, p_project, p_actor, 'figure_manuscript_cleared',
    jsonb_build_object('figure', p_figure, 'version', v_current->'version'));
  UPDATE enrichedlab_agent.broker_projects SET updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND project_id = p_project;
  RETURN true;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_clear_figure_manuscript(text, uuid, uuid, uuid) FROM PUBLIC;

-- The figure record names its figure and the figure's mark.
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
  v_figure uuid;
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
  v_figure := enrichedlab_agent.broker_figure_root(p_tenant, p_task);
  RETURN jsonb_build_object('task_id', r.task_id, 'project_id', r.project_id, 'source_task', r.source_task_id,
    'source_title', v_source_title, 'source_package_sha256', r.source_package_sha256, 'table_path', r.table_path,
    'table_sha256', r.table_sha256, 'table_rows', r.table_rows, 'table_columns', r.table_columns,
    'feature', r.feature, 'controls', r.controls, 'title', r.title, 'previous', r.previous_task_id,
    'catalog_sha256', r.catalog_sha256, 'request_sha256', r.request_sha256, 'created_at', r.created_at,
    'figure', v_figure, 'manuscript', enrichedlab_agent.broker_figure_manuscript(p_tenant, v_figure),
    'receipts', coalesce((SELECT jsonb_agg(jsonb_build_object('attempt', x.attempt, 'status', x.status,
        'error_code', x.error_code, 'message', x.message, 'service_version', x.service_version,
        'image_digest', x.image_digest, 'files', x.files, 'notes', x.notes, 'package_sha256', x.package_sha256,
        'started_at', x.started_at, 'finished_at', x.finished_at) ORDER BY x.attempt, x.lease_generation)
      FROM enrichedlab_agent.broker_figure_receipts x WHERE x.tenant_id = p_tenant AND x.task_id = p_task), '[]'::jsonb));
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_figure_detail(text, uuid, uuid) FROM PUBLIC;

-- The versions made from one analysis, each naming its figure and that figure's mark.
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
      'created_at', r.created_at, 'finished_at', t.finished_at, 'figure', f.root,
      'manuscript', enrichedlab_agent.broker_figure_manuscript(p_tenant, f.root)) ORDER BY r.created_at DESC, r.task_id)
    FROM enrichedlab_agent.broker_figure_requests r
    JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = r.tenant_id AND t.task_id = r.task_id
    CROSS JOIN LATERAL (SELECT enrichedlab_agent.broker_figure_root(p_tenant, r.task_id) AS root) f
    WHERE r.tenant_id = p_tenant AND r.source_task_id = p_source_task
      AND enrichedlab_agent.broker_task_can(p_tenant, p_actor, r.task_id, 'read')), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_figure_versions(text, uuid, uuid) FROM PUBLIC;

DO $$ BEGIN
  ALTER TABLE enrichedlab_agent.broker_figure_previews ENABLE ROW LEVEL SECURITY;
  ALTER TABLE enrichedlab_agent.broker_figure_manuscript_events ENABLE ROW LEVEL SECURITY;
  REVOKE ALL ON enrichedlab_agent.broker_figure_previews FROM PUBLIC;
  REVOKE ALL ON enrichedlab_agent.broker_figure_manuscript_events FROM PUBLIC;
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_figure_manuscript_events;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_figure_manuscript_events
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (57) ON CONFLICT DO NOTHING;
COMMIT;
