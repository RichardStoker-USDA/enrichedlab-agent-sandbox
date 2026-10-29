-- The research lead reads the lab library. The coordinator searches the lab's corpus of the document
-- service itself; this read, fenced by the run's lease like the lab-notes read, tells it which of the documents a search
-- returned are papers still in the library of the project's lab, and whether the run's starter is still a member of
-- that lab. Only then may a passage enter the lead's context. Additive: the running API, coordinator and worker keep
-- working.
BEGIN;

-- For the coordinator holding a run's lease: {"member": whether the run's starter holds a role in the project's
-- enabled lab, "papers": the papers among p_documents that the lab's library holds and has not removed, each with its
-- paper id, document id, title and year, ordered by paper id}. A non-member gets no papers.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_library(
  p_tenant text,
  p_run uuid,
  p_worker text,
  p_generation bigint,
  p_documents uuid[]
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  r enrichedlab_agent.broker_research_runs%ROWTYPE;
  grp text;
  member boolean;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_documents IS NULL OR cardinality(p_documents) > 100 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:library_invalid';
  END IF;
  SELECT p.group_key INTO grp FROM enrichedlab_agent.broker_projects p
  WHERE p.tenant_id = p_tenant AND p.project_id = r.project_id;
  member := EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = m.tenant_id AND a.principal_id = m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_tenants t ON t.tenant_id = m.tenant_id AND t.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND g.enabled
    WHERE m.tenant_id = p_tenant AND m.group_key = grp AND m.principal_id = r.actor_id);
  IF NOT member THEN
    RETURN jsonb_build_object('member', false, 'papers', '[]'::jsonb);
  END IF;
  RETURN jsonb_build_object('member', true, 'papers', (
    SELECT coalesce(jsonb_agg(jsonb_build_object('paper_id', x.paper_id, 'document_id', x.document_id,
        'title', x.title, 'year', x.year) ORDER BY x.paper_id), '[]'::jsonb)
    FROM enrichedlab_agent.broker_library_papers x
    WHERE x.tenant_id = p_tenant AND x.group_key = grp AND x.document_id = ANY(p_documents)
      AND enrichedlab_agent.broker_library_removal(p_tenant, x.paper_id) IS NULL));
END;
$$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'enrichedlab_agent' AND p.proname IN ('broker_research_run_library')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (29) ON CONFLICT DO NOTHING;
COMMIT;
