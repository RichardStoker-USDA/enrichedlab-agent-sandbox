-- The research lead's own queries. In test runs six of eight runs spent their second
-- turn proposing again the query their first turn had run: an analysis the lead reads shows its title and results
-- table, not the query that made them. Under the run's lease this read returns, for the query analyses of the run's
-- own project that the coordinator names (at most 64), the admitted query text and digest and the status, error code
-- and row count of its newest receipt, never a row. Nothing else changes.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_own_queries(p_tenant text, p_run uuid, p_worker text,
  p_generation bigint, p_tasks uuid[])
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
  IF p_tasks IS NULL OR cardinality(p_tasks) > 64 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:own_queries_invalid';
  END IF;
  RETURN coalesce((
    SELECT jsonb_agg(x.item ORDER BY x.ord)
    FROM (
      SELECT min(a.ord) AS ord,
        jsonb_build_object('task_id', q.task_id, 'admitted_sql', q.admitted_sql, 'sql_sha256', q.sql_sha256,
          'status', rc.status, 'error_code', rc.error_code, 'row_count', rc.row_count) AS item
      FROM unnest(p_tasks) WITH ORDINALITY AS a(task_id, ord)
      JOIN enrichedlab_agent.broker_task_queries q ON q.tenant_id = p_tenant AND q.task_id = a.task_id
      JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = q.tenant_id AND pt.task_id = q.task_id
      LEFT JOIN LATERAL (
        SELECT rr.status, rr.error_code, rr.row_count FROM enrichedlab_agent.broker_query_receipts rr
        WHERE rr.tenant_id = q.tenant_id AND rr.task_id = q.task_id
        ORDER BY rr.recorded_at DESC LIMIT 1) rc ON true
      WHERE pt.project_id = r.project_id
      GROUP BY q.task_id, q.admitted_sql, q.sql_sha256, rc.status, rc.error_code, rc.row_count
    ) x), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_own_queries(text, uuid, text, bigint, uuid[]) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (43) ON CONFLICT DO NOTHING;
COMMIT;
