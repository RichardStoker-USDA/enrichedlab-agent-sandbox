-- The research lead reads people's reviews. The coordinator's results read gains, for each analysis in
-- the plan, the newest review by a person, so the lead's context can withhold rejected results and carry a request
-- for changes. Same signature and grant; the running coordinator ignores the new key.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_results(
  p_tenant text,
  p_run uuid,
  p_worker text,
  p_generation bigint
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  r enrichedlab_agent.broker_research_runs%ROWTYPE;
  plan jsonb;
  results jsonb;
  findings jsonb;
  reviews jsonb;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  SELECT c.state->'plan' INTO plan
  FROM enrichedlab_agent.broker_projects p
  JOIN enrichedlab_agent.broker_project_checkpoints c
    ON c.tenant_id = p.tenant_id AND c.project_id = p.project_id AND c.revision = p.revision
  WHERE p.tenant_id = p_tenant AND p.project_id = r.project_id;
  SELECT coalesce(jsonb_agg(x.item ORDER BY x.task_id, x.created_at DESC), '[]'::jsonb) INTO results
  FROM (
    SELECT a.task_id, a.created_at, jsonb_build_object(
      'task_id', a.task_id,
      'ref', 'artifact://tasks/' || a.task_id::text || '/' || a.artifact_id::text,
      'label', a.label,
      'status', d.status,
      'digest_sha256', d.digest_sha256,
      'digest', d.digest) AS item,
      row_number() OVER (PARTITION BY a.task_id ORDER BY a.created_at DESC, a.artifact_id) AS n
    FROM enrichedlab_agent.broker_project_tasks pt
    JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id = pt.tenant_id AND a.task_id = pt.task_id
    JOIN enrichedlab_agent.broker_artifact_digests d
      ON d.tenant_id = a.tenant_id AND d.artifact_id = a.artifact_id AND d.artifact_sha256 = a.sha256
    WHERE pt.tenant_id = p_tenant AND pt.project_id = r.project_id
      AND a.visibility = 'researcher' AND a.role = 'result'
      AND EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(plan, '[]'::jsonb)) s WHERE s->>'task_id' = a.task_id::text)
  ) x WHERE x.n <= 4;
  SELECT jsonb_build_object('run_id', t.run_id, 'turn', t.turn, 'recorded_at', t.created_at,
           'answer', t.record->'proposal'->'answer', 'findings', t.record->'proposal'->'findings')
    INTO findings
  FROM enrichedlab_agent.broker_research_turns t
  JOIN enrichedlab_agent.broker_research_runs rr ON rr.tenant_id = t.tenant_id AND rr.run_id = t.run_id
  WHERE t.tenant_id = p_tenant AND rr.project_id = r.project_id
    AND jsonb_typeof(t.record->'proposal'->'findings') = 'array'
    AND jsonb_array_length(t.record->'proposal'->'findings') > 0
  ORDER BY t.created_at DESC, t.turn DESC
  LIMIT 1;
  -- The newest person's review of each analysis in the plan. A service account's row never counts, no
  -- reviewer identity is returned, and a note travels only with a decision other than acceptance.
  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id', v.task_id, 'decision', v.decision,
      'note', CASE WHEN v.decision <> 'accepted' THEN v.note END, 'recorded_at', v.created_at,
      'covers_current_outputs', v.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, v.task_id))
      ORDER BY v.task_id), '[]'::jsonb) INTO reviews
  FROM (
    SELECT DISTINCT ON (rv.task_id) rv.task_id, rv.decision, rv.note, rv.created_at, rv.outputs
    FROM enrichedlab_agent.broker_task_reviews rv
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = rv.tenant_id AND pt.task_id = rv.task_id
    JOIN enrichedlab_agent.broker_principals p ON p.tenant_id = rv.tenant_id AND p.principal_id = rv.reviewer_id
    WHERE rv.tenant_id = p_tenant AND pt.project_id = r.project_id AND p.issuer <> 'kubernetes://serviceaccount'
      AND EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(plan, '[]'::jsonb)) s WHERE s->>'task_id' = rv.task_id::text)
    ORDER BY rv.task_id, rv.created_at DESC, rv.review_id DESC
  ) v;
  RETURN jsonb_build_object('results', results, 'earlier_findings', findings, 'reviews', reviews);
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (25) ON CONFLICT DO NOTHING;
COMMIT;
