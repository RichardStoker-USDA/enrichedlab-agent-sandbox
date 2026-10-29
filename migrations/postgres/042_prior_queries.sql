-- Earlier working queries for the research lead. In a test run the lead wrote a query that
-- returned no rows while the lab's earlier analysis of a similar question had one that worked, and the lead could not
-- see it. The coordinator names the analyses its knowledge search found; under the run's lease this read returns the
-- admitted query of each one the run may learn from: a query analysis in another lab-visible project of the run's lab,
-- which the run's person may read, finished, whose last receipt completed, whose method the run may use, and whose
-- newest review by a person is not a rejection. It returns the query text, its source, its digest and how many rows it
-- returned, never a row. Nothing else changes.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_prior_queries(p_tenant text, p_run uuid, p_worker text,
  p_generation bigint, p_tasks uuid[])
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  r enrichedlab_agent.broker_research_runs%ROWTYPE;
  grp text;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_tasks IS NULL OR cardinality(p_tasks) > 12 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:prior_queries_invalid';
  END IF;
  SELECT p.group_key INTO grp FROM enrichedlab_agent.broker_projects p
  WHERE p.tenant_id = p_tenant AND p.project_id = r.project_id;
  RETURN coalesce((
    SELECT jsonb_agg(x.item ORDER BY x.ord)
    FROM (
      SELECT min(a.ord) AS ord,
        jsonb_build_object('task_id', q.task_id, 'source', q.source_key, 'title', s.title, 'release', s.release,
          'admitted_sql', q.admitted_sql, 'sql_sha256', q.sql_sha256, 'row_count', rc.row_count) AS item
      FROM unnest(p_tasks) WITH ORDINALITY AS a(task_id, ord)
      JOIN enrichedlab_agent.broker_task_queries q ON q.tenant_id = p_tenant AND q.task_id = a.task_id
      JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = q.tenant_id AND t.task_id = q.task_id
      JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = t.tenant_id AND pt.task_id = t.task_id
      JOIN enrichedlab_agent.broker_projects pr ON pr.tenant_id = pt.tenant_id AND pr.project_id = pt.project_id
      JOIN enrichedlab_agent.broker_sql_sources s ON s.tenant_id = q.tenant_id AND s.source_key = q.source_key
      CROSS JOIN LATERAL (
        SELECT rr.status, rr.row_count FROM enrichedlab_agent.broker_query_receipts rr
        WHERE rr.tenant_id = q.tenant_id AND rr.task_id = q.task_id
        ORDER BY rr.recorded_at DESC LIMIT 1) rc
      WHERE pr.group_key = grp AND pr.visibility = 'lab' AND pr.project_id <> r.project_id
        AND t.status IN ('completed', 'review_required') AND rc.status = 'completed'
        AND q.profile_key = ANY (r.allowed_profiles)
        AND enrichedlab_agent.broker_project_can(p_tenant, r.actor_id, pr.project_id, 'read')
        AND coalesce((SELECT rv.decision FROM enrichedlab_agent.broker_task_reviews rv
          WHERE rv.tenant_id = t.tenant_id AND rv.task_id = t.task_id
          ORDER BY rv.created_at DESC LIMIT 1), '') <> 'rejected'
      GROUP BY q.task_id, q.source_key, s.title, s.release, q.admitted_sql, q.sql_sha256, rc.row_count
    ) x), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_prior_queries(text, uuid, text, bigint, uuid[]) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (42) ON CONFLICT DO NOTHING;
COMMIT;
