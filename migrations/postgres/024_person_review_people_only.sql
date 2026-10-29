-- A person's review is a person's. Migration 023 let any principal holding the project's `review`
-- capability record one, and a project owned by a workload account gives it that capability: the live canary's
-- service account recorded two reviews, each of which the workspace then presented as a named person's reading.
-- Recording now needs a person's account; a service account gets `review_person_only`. Rows already recorded stay (they
-- are immutable), and every review now carries `reviewer_kind` so the workspace can say which a service account made.
-- Both functions keep their signatures, so their grants stay; the running API (5674317) keeps working.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_review_json(p_tenant text, r enrichedlab_agent.broker_task_reviews)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT jsonb_build_object('review_id', r.review_id, 'task_id', r.task_id, 'attempt', r.attempt,
    'decision', r.decision, 'note', r.note, 'reviewer_id', r.reviewer_id,
    -- The display name only: a task reader may not hold the capability that shows emails.
    'reviewer_name', (SELECT p.display_name FROM enrichedlab_agent.broker_principals p
      WHERE p.tenant_id = p_tenant AND p.principal_id = r.reviewer_id),
    -- Rows from before migration 024 may name a service account; the workspace says so.
    'reviewer_kind', (SELECT CASE WHEN p.issuer = 'kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END
      FROM enrichedlab_agent.broker_principals p WHERE p.tenant_id = p_tenant AND p.principal_id = r.reviewer_id),
    'outputs', r.outputs, 'created_at', r.created_at,
    'covers_current_outputs', r.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, r.task_id))
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_task_review(p_tenant text, p_actor uuid, p_task uuid,
  p_decision text, p_note text, p_request uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE t enrichedlab_agent.broker_tasks%ROWTYPE; existing enrichedlab_agent.broker_task_reviews%ROWTYPE;
  saved enrichedlab_agent.broker_task_reviews%ROWTYPE; clean text; outs jsonb; project uuid;
BEGIN
  SELECT * INTO t FROM enrichedlab_agent.broker_tasks WHERE tenant_id = p_tenant AND task_id = p_task FOR SHARE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_task_can(p_tenant, p_actor, p_task, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found'; END IF;
  -- Review is a project capability; an analysis outside a project has no reviewer.
  SELECT pt.project_id INTO project FROM enrichedlab_agent.broker_project_tasks pt
    WHERE pt.tenant_id = p_tenant AND pt.task_id = p_task;
  IF project IS NULL OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, project, 'review') THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:review_forbidden'; END IF;
  -- Only a person reviews: a workload account can own a project, but its decision would not be a person's reading.
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_principals p WHERE p.tenant_id = p_tenant
      AND p.principal_id = p_actor AND p.issuer <> 'kubernetes://serviceaccount') THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:review_person_only'; END IF;
  -- Surrounding spaces, tabs and line breaks are not a note.
  clean := nullif(btrim(coalesce(p_note, ''), E' \t\r\n'), '');
  SELECT * INTO existing FROM enrichedlab_agent.broker_task_reviews
    WHERE tenant_id = p_tenant AND reviewer_id = p_actor AND request_id = p_request;
  IF FOUND THEN
    IF existing.task_id <> p_task OR existing.decision IS DISTINCT FROM p_decision OR existing.note IS DISTINCT FROM clean THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict'; END IF;
    RETURN enrichedlab_agent.broker_task_review_json(p_tenant, existing);
  END IF;
  IF p_request IS NULL OR p_decision IS NULL OR p_decision NOT IN ('accepted', 'needs_changes', 'rejected')
    OR (p_decision <> 'accepted' AND clean IS NULL)
    OR (clean IS NOT NULL AND char_length(clean) > 2000) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:review_invalid'; END IF;
  IF t.status NOT IN ('completed', 'failed', 'cancelled', 'review_required') THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:review_not_finished'; END IF;
  outs := enrichedlab_agent.broker_task_review_outputs(p_tenant, p_task);
  -- Accepting needs an analysis that finished with results to accept, including one that stopped for review.
  IF p_decision = 'accepted' AND (t.status NOT IN ('completed', 'review_required') OR jsonb_array_length(outs) = 0) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:review_nothing_to_accept'; END IF;
  INSERT INTO enrichedlab_agent.broker_task_reviews (tenant_id, task_id, attempt, lease_generation, reviewer_id,
      request_id, decision, note, outputs)
    VALUES (p_tenant, p_task, t.attempt, t.lease_generation, p_actor, p_request, p_decision, clean, outs)
    RETURNING * INTO saved;
  RETURN enrichedlab_agent.broker_task_review_json(p_tenant, saved);
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (24) ON CONFLICT DO NOTHING;
COMMIT;
