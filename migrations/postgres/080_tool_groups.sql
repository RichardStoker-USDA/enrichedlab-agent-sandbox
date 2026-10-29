-- Tool groups granted to labs and each lab's tools (K-t1 parts 2 and 3). A tool group is a named,
-- versioned set of methods. The catalog seed stores each version once and refuses a stored version whose contents
-- would change; granting a version to a lab adds its methods to the lab's profile grants, which proposal and the worker
-- enforce as before, and the seed removes a profile grant only when neither a direct grant nor another granted group
-- holds it. A grant is never deleted: revoking it records when, and granting again adds a new row.
-- `broker_lab_tools` is the workspace's read of one lab's tools for a member: each enabled method granted to the lab,
-- how it is granted, its kind, template digest, configured setting and choice labels, the reviewed queries it offers
-- and up to three accepted result examples the reader can open, by the same projections as the knowledge export's
-- version 6. Catalog fields only: never SQL, R text a choice inserts, rows, values or identities of people.
-- Rollback: an older broker neither reads nor writes these tables, and its seed removes profile grants only a group held.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_tool_groups (
  tenant_id text NOT NULL REFERENCES enrichedlab_agent.broker_tenants(tenant_id),
  tool_group_key text NOT NULL CHECK (tool_group_key ~ '^[a-z][a-z0-9_-]{1,63}$'),
  version integer NOT NULL CHECK (version BETWEEN 1 AND 10000),
  title text NOT NULL CHECK (char_length(title) BETWEEN 1 AND 160 AND title !~ '[[:cntrl:]]' AND btrim(title) <> ''),
  description text NOT NULL CHECK (char_length(description) BETWEEN 1 AND 2000 AND description !~ '[[:cntrl:]]'
    AND btrim(description) <> ''),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, tool_group_key, version)
);
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_tool_group_members (
  tenant_id text NOT NULL,
  tool_group_key text NOT NULL,
  version integer NOT NULL,
  profile_key text NOT NULL,
  PRIMARY KEY (tenant_id, tool_group_key, version, profile_key),
  FOREIGN KEY (tenant_id, tool_group_key, version)
    REFERENCES enrichedlab_agent.broker_tool_groups (tenant_id, tool_group_key, version),
  FOREIGN KEY (tenant_id, profile_key) REFERENCES enrichedlab_agent.broker_agent_profiles (tenant_id, profile_key)
);
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_tool_group_grants (
  grant_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  tenant_id text NOT NULL,
  group_key text NOT NULL,
  tool_group_key text NOT NULL,
  version integer NOT NULL,
  granted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  revoked_at timestamptz,
  CHECK (revoked_at IS NULL OR revoked_at >= granted_at),
  FOREIGN KEY (tenant_id, group_key) REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, tool_group_key, version)
    REFERENCES enrichedlab_agent.broker_tool_groups (tenant_id, tool_group_key, version)
);
-- One active version of a tool group per lab.
CREATE UNIQUE INDEX IF NOT EXISTS broker_tool_group_grants_active
  ON enrichedlab_agent.broker_tool_group_grants (tenant_id, group_key, tool_group_key) WHERE revoked_at IS NULL;

DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_tool_groups', 'broker_tool_group_members', 'broker_tool_group_grants'] LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON enrichedlab_agent.%I FROM PUBLIC', t);
  END LOOP;
END $$;

-- A stored version and its members never change and are never truncated; a grant may only gain its revocation time,
-- once.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_tool_group_immutable()
RETURNS trigger LANGUAGE plpgsql SET search_path = pg_catalog, enrichedlab_agent AS $$
BEGIN
  -- Fields are read through to_jsonb: the same function guards tables that have no revocation column.
  IF TG_TABLE_NAME = 'broker_tool_group_grants' AND TG_OP = 'UPDATE' THEN
    IF to_jsonb(OLD)->'revoked_at' = 'null'::jsonb AND to_jsonb(NEW)->'revoked_at' <> 'null'::jsonb
      AND (to_jsonb(NEW) - 'revoked_at') = (to_jsonb(OLD) - 'revoked_at') THEN
      RETURN NEW;
    END IF;
  END IF;
  RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:immutable_record';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_tool_group_immutable() FROM PUBLIC;
DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_tool_groups', 'broker_tool_group_members', 'broker_tool_group_grants'] LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.%I
      FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_tool_group_immutable()', t);
    EXECUTE format('DROP TRIGGER IF EXISTS no_truncate ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER no_truncate BEFORE TRUNCATE ON enrichedlab_agent.%I
      FOR EACH STATEMENT EXECUTE FUNCTION enrichedlab_agent.broker_tool_group_immutable()', t);
  END LOOP;
END $$;

-- One lab's tools for a member of the lab; any other reader is refused as `broker:lab_not_found`.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_lab_tools(p_tenant text, p_actor uuid, p_group text)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_get_groups(p_tenant, p_actor) g WHERE g.group_key = p_group) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:lab_not_found';
  END IF;
  RETURN jsonb_build_object(
    'lab', p_group,
    'tool_groups', (
      SELECT coalesce(jsonb_agg(jsonb_build_object(
               'key', tg.tool_group_key, 'version', tg.version, 'title', tg.title, 'description', tg.description,
               'granted_at', gr.granted_at,
               'tools', (SELECT coalesce(jsonb_agg(m.profile_key ORDER BY m.profile_key), '[]'::jsonb)
                           FROM enrichedlab_agent.broker_tool_group_members m
                          WHERE m.tenant_id = tg.tenant_id AND m.tool_group_key = tg.tool_group_key
                            AND m.version = tg.version))
             ORDER BY tg.title, tg.tool_group_key), '[]'::jsonb)
        FROM enrichedlab_agent.broker_tool_group_grants gr
        JOIN enrichedlab_agent.broker_tool_groups tg
          ON tg.tenant_id = gr.tenant_id AND tg.tool_group_key = gr.tool_group_key AND tg.version = gr.version
       WHERE gr.tenant_id = p_tenant AND gr.group_key = p_group AND gr.revoked_at IS NULL),
    'tools', (
      SELECT coalesce(jsonb_agg(enrichedlab_agent.broker_lab_tool_json(p_tenant, p_actor, p_group, p)
               ORDER BY p.name, p.profile_key), '[]'::jsonb)
        FROM enrichedlab_agent.broker_group_profile_grants g
        JOIN enrichedlab_agent.broker_agent_profiles p
          ON p.tenant_id = g.tenant_id AND p.profile_key = g.profile_key
       WHERE g.tenant_id = p_tenant AND g.group_key = p_group AND p.enabled));
END;
$$;

-- One method as the lab's tools list shows it. Called only from `broker_lab_tools`, which checks the reader.
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
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_lab_tools(text, uuid, text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (80) ON CONFLICT DO NOTHING;
COMMIT;
