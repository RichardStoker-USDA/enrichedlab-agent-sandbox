-- Stable, per-project evidence-package revisions. The registry
-- stores IDs and digests only. Package payload text remains in its owning tables.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_project_packages (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  disclosure_scope text NOT NULL CONSTRAINT broker_project_packages_scope_check
    CHECK (disclosure_scope IN
      ('ordinary_with_reasoning','ordinary_without_reasoning','inspection')),
  revision bigint NOT NULL CHECK (revision > 0),
  content_sha256 text NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
  inventory jsonb NOT NULL,
  created_by uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, project_id, disclosure_scope, revision),
  CONSTRAINT broker_project_packages_scope_content_key
    UNIQUE (tenant_id, project_id, disclosure_scope, content_sha256),
  FOREIGN KEY (tenant_id, project_id)
    REFERENCES enrichedlab_agent.broker_projects (tenant_id, project_id),
  FOREIGN KEY (tenant_id, created_by)
    REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (jsonb_typeof(inventory) = 'object' AND octet_length(inventory::text) <= 16777216)
);
-- An earlier draft of this migration may have been exercised before release. Upgrade that empty or
-- qualification-only registry in place; legacy rows enter only the widest scope,
-- so they can never become visible to an inspection or narrower ordinary reader.
ALTER TABLE enrichedlab_agent.broker_project_packages
  ADD COLUMN IF NOT EXISTS disclosure_scope text NOT NULL
    DEFAULT 'ordinary_with_reasoning';
ALTER TABLE enrichedlab_agent.broker_project_packages
  ALTER COLUMN disclosure_scope DROP DEFAULT;
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='enrichedlab_agent.broker_project_packages'::regclass
      AND conname='broker_project_packages_scope_check') THEN
    ALTER TABLE enrichedlab_agent.broker_project_packages
      ADD CONSTRAINT broker_project_packages_scope_check CHECK (disclosure_scope IN
        ('ordinary_with_reasoning','ordinary_without_reasoning','inspection'));
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='enrichedlab_agent.broker_project_packages'::regclass
      AND conname='broker_project_packages_scope_content_key') THEN
    ALTER TABLE enrichedlab_agent.broker_project_packages
      ADD CONSTRAINT broker_project_packages_scope_content_key
      UNIQUE (tenant_id,project_id,disclosure_scope,content_sha256);
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='enrichedlab_agent.broker_project_packages'::regclass
      AND conname='broker_project_packages_pkey'
      AND pg_get_constraintdef(oid) LIKE '%disclosure_scope%') THEN
    ALTER TABLE enrichedlab_agent.broker_project_packages
      DROP CONSTRAINT broker_project_packages_pkey;
    ALTER TABLE enrichedlab_agent.broker_project_packages
      ADD CONSTRAINT broker_project_packages_pkey
      PRIMARY KEY (tenant_id,project_id,disclosure_scope,revision);
  END IF;
END;
$$;
ALTER TABLE enrichedlab_agent.broker_project_packages ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_project_packages FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_project_packages;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE
  ON enrichedlab_agent.broker_project_packages
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- The package registry has three disclosure chains. Scope is decided here so an
-- API caller cannot place a restricted inventory in a less privileged chain.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_package_scope(
  p_tenant text, p_actor uuid, p_project uuid)
RETURNS text LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  group_name text;
  may_read_reasoning boolean;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'read') THEN
    RETURN NULL;
  END IF;
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_inspections i
    WHERE i.tenant_id=p_tenant AND i.project_id=p_project AND i.principal_id=p_actor
      AND i.expires_at>statement_timestamp()) THEN
    RETURN 'inspection';
  END IF;
  SELECT p.group_key INTO group_name FROM enrichedlab_agent.broker_projects p
    WHERE p.tenant_id=p_tenant AND p.project_id=p_project;
  SELECT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=m.tenant_id
      AND a.principal_id=m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id=m.tenant_id
      AND g.group_key=m.group_key AND g.enabled
    JOIN enrichedlab_agent.broker_projects p ON p.tenant_id=m.tenant_id
      AND p.group_key=m.group_key AND p.project_id=p_project
    WHERE m.tenant_id=p_tenant AND m.group_key=group_name AND m.principal_id=p_actor
      AND (p.owner_id=p_actor OR p.visibility='lab' OR EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_project_grants grant_record
        WHERE grant_record.tenant_id=p.tenant_id AND grant_record.project_id=p.project_id
          AND grant_record.principal_id=p_actor AND cardinality(grant_record.capabilities)>0)))
    INTO may_read_reasoning;
  RETURN CASE WHEN may_read_reasoning THEN 'ordinary_with_reasoning'
    ELSE 'ordinary_without_reasoning' END;
END;
$$;

-- The scope-specific advisory lock makes concurrent requests linearizable. Any
-- earlier matching content returns its immutable revision, not only the latest.
DROP FUNCTION IF EXISTS enrichedlab_agent.broker_register_project_package(
  text,uuid,uuid,text,jsonb);
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_register_project_package(
  p_tenant text, p_actor uuid, p_project uuid, p_scope text,
  p_content_sha256 text, p_inventory jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  actual_scope text;
  registered enrichedlab_agent.broker_project_packages%ROWTYPE;
  latest enrichedlab_agent.broker_project_packages%ROWTYPE;
  preceding jsonb := '{}'::jsonb;
BEGIN
  actual_scope := enrichedlab_agent.broker_project_package_scope(p_tenant,p_actor,p_project);
  IF actual_scope IS NULL THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  IF p_scope IS NULL OR p_scope NOT IN
       ('ordinary_with_reasoning','ordinary_without_reasoning','inspection')
     OR p_scope IS DISTINCT FROM actual_scope THEN
    RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='broker:project_package_scope_changed';
  END IF;
  IF p_content_sha256 IS NULL OR p_content_sha256 !~ '^[0-9a-f]{64}$'
     OR jsonb_typeof(p_inventory) IS DISTINCT FROM 'object'
     OR octet_length(p_inventory::text) > 16777216
     OR EXISTS (SELECT 1 FROM jsonb_each_text(p_inventory) e
       WHERE e.key !~ '^[A-Za-z0-9_.:/-]{1,200}$' OR e.value !~ '^[0-9a-f]{64}$') THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:project_package_invalid';
  END IF;

  PERFORM pg_advisory_xact_lock(hashtextextended(
    p_tenant || ':' || p_project::text || ':' || p_scope, 0));
  SELECT * INTO registered FROM enrichedlab_agent.broker_project_packages
    WHERE tenant_id=p_tenant AND project_id=p_project AND disclosure_scope=p_scope
      AND content_sha256=p_content_sha256;
  IF FOUND THEN
    IF registered.inventory IS DISTINCT FROM p_inventory THEN
      RAISE EXCEPTION USING ERRCODE='23000',MESSAGE='broker:project_package_digest_collision';
    END IF;
    SELECT inventory INTO preceding FROM enrichedlab_agent.broker_project_packages
      WHERE tenant_id=p_tenant AND project_id=p_project AND disclosure_scope=p_scope
        AND revision=registered.revision-1;
    RETURN jsonb_build_object('disclosure_scope',registered.disclosure_scope,
      'revision',registered.revision,'created_at',registered.created_at,
      'content_sha256',registered.content_sha256,'previous_revision',
      CASE WHEN registered.revision>1 THEN registered.revision-1 ELSE NULL END,
      'previous_inventory',coalesce(preceding,'{}'::jsonb));
  END IF;

  SELECT * INTO latest FROM enrichedlab_agent.broker_project_packages
    WHERE tenant_id=p_tenant AND project_id=p_project AND disclosure_scope=p_scope
    ORDER BY revision DESC LIMIT 1;
  IF FOUND THEN preceding := latest.inventory; END IF;
  INSERT INTO enrichedlab_agent.broker_project_packages
    (tenant_id,project_id,disclosure_scope,revision,content_sha256,inventory,created_by)
    VALUES (p_tenant,p_project,p_scope,coalesce(latest.revision,0)+1,
      p_content_sha256,p_inventory,p_actor)
    RETURNING * INTO registered;
  RETURN jsonb_build_object('disclosure_scope',registered.disclosure_scope,
    'revision',registered.revision,'created_at',registered.created_at,
    'content_sha256',registered.content_sha256,'previous_revision',
    CASE WHEN registered.revision>1 THEN registered.revision-1 ELSE NULL END,
    'previous_inventory',preceding);
END;
$$;

-- Extra records already held by the broker and needed to make each run readable
-- alone. Exact kept model reasoning remains restricted to current lab members.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_package_components(
  p_tenant text, p_actor uuid, p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  group_name text;
  disclosure_scope text;
  may_read_reasoning boolean;
  reasoning jsonb := '[]'::jsonb;
  digests jsonb;
  origins jsonb;
  papers jsonb;
BEGIN
  disclosure_scope := enrichedlab_agent.broker_project_package_scope(
    p_tenant,p_actor,p_project);
  IF disclosure_scope IS NULL THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  SELECT group_key INTO group_name FROM enrichedlab_agent.broker_projects
    WHERE tenant_id=p_tenant AND project_id=p_project;

  may_read_reasoning := disclosure_scope='ordinary_with_reasoning';

  IF may_read_reasoning THEN
    WITH addresses AS (
      SELECT t.run_id,t.turn,'lead'::text AS role,t.record->'lead'->>'reasoning_sha256' AS sha256
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT t.run_id,t.turn,'methods_specialist',t.record->'methods_specialist'->>'reasoning_sha256'
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT t.run_id,t.turn,'methods_specialist_refused',
        t.record->'methods_specialist_refused'->'receipt'->>'reasoning_sha256'
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT t.run_id,t.turn,'literature_specialist',s->>'reasoning_sha256'
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      CROSS JOIN LATERAL jsonb_array_elements(CASE
        WHEN jsonb_typeof(t.record->'literature_claims'->'specialist')='array'
        THEN t.record->'literature_claims'->'specialist' ELSE '[]'::jsonb END) s
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT e.run_id,coalesce(e.turn,0),'lead_refused',e.lead_reasoning_sha256
      FROM enrichedlab_agent.broker_research_run_events e JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=e.tenant_id AND r.run_id=e.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
    )
    SELECT coalesce(jsonb_agg(jsonb_build_object('run_id',a.run_id,'turn',a.turn,
      'role',a.role,'sha256',a.sha256,'text',convert_from(c.content,'UTF8'))
      ORDER BY a.run_id,a.turn,a.role,a.sha256),'[]'::jsonb) INTO reasoning
    FROM (SELECT DISTINCT run_id,turn,role,sha256 FROM addresses WHERE sha256 IS NOT NULL) a
    JOIN enrichedlab_agent.broker_research_contexts c
      ON c.tenant_id=p_tenant AND c.sha256=a.sha256;
  END IF;

  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id',a.task_id,'artifact_id',a.artifact_id,
    'run_id',o.run_id,'turn',o.turn,'status',d.status,'digest_sha256',d.digest_sha256,
    'digest',d.digest,'recorded_at',d.recorded_at) ORDER BY a.task_id,a.created_at,a.artifact_id),'[]'::jsonb)
    INTO digests
  FROM enrichedlab_agent.broker_project_tasks pt
  JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id=pt.tenant_id AND a.task_id=pt.task_id
  JOIN enrichedlab_agent.broker_artifact_digests d ON d.tenant_id=a.tenant_id
    AND d.artifact_id=a.artifact_id AND d.artifact_sha256=a.sha256
  LEFT JOIN enrichedlab_agent.broker_task_origins o ON o.tenant_id=a.tenant_id AND o.task_id=a.task_id
  WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project AND a.visibility='researcher';

  SELECT coalesce(jsonb_agg(to_jsonb(o)-'tenant_id' ORDER BY o.recorded_at,o.task_id),'[]'::jsonb)
    INTO origins FROM enrichedlab_agent.broker_task_origins o
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id=o.tenant_id AND pt.task_id=o.task_id
    WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project;

  WITH touched AS (
    SELECT DISTINCT passage->>'paper_id' AS paper_id
    FROM enrichedlab_agent.broker_research_turns t
    JOIN enrichedlab_agent.broker_research_runs r ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
    CROSS JOIN LATERAL jsonb_array_elements(
      (CASE WHEN jsonb_typeof(t.record->'context_inputs'->'literature'->'passages')='array'
        THEN t.record->'context_inputs'->'literature'->'passages' ELSE '[]'::jsonb END)
      || (CASE WHEN jsonb_typeof(t.record->'context_inputs'->'literature'->'withheld')='array'
        THEN t.record->'context_inputs'->'literature'->'withheld' ELSE '[]'::jsonb END)) passage
    WHERE r.tenant_id=p_tenant AND r.project_id=p_project AND passage ? 'paper_id'
  )
  -- An inspection is one disclosure chain regardless of whether its administrator
  -- also belongs to the lab, so it always stores the common ID-only paper view.
  SELECT coalesce(jsonb_agg(CASE WHEN p.paper_id IS NULL
      OR disclosure_scope<>'ordinary_with_reasoning'
    THEN jsonb_build_object('paper_id',t.paper_id)
    ELSE jsonb_build_object('paper_id',t.paper_id,'source_sha256',p.source_sha256,'byte_size',p.byte_size,
      'title',p.title,'authors',p.authors,'year',p.year,'doi',p.doi,'page_count',p.page_count,
      'parse',p.parse,'created_at',p.created_at) END ORDER BY t.paper_id),'[]'::jsonb) INTO papers
  FROM touched t LEFT JOIN enrichedlab_agent.broker_library_papers p
    ON p.tenant_id=p_tenant AND p.group_key=group_name AND p.paper_id::text=t.paper_id;

  RETURN jsonb_build_object('disclosure_scope',disclosure_scope,
    'reasoning_access',may_read_reasoning,'reasoning',reasoning,
    'digests',digests,'task_origins',origins,'papers',papers);
END;
$$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_package_scope(text,uuid,uuid) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_register_project_package(text,uuid,uuid,text,text,jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_package_components(text,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (68) ON CONFLICT DO NOTHING;
COMMIT;
