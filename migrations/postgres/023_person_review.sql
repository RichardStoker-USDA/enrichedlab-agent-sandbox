-- A person's review of an analysis. Every analysis said it was not yet reviewed by a person and nothing
-- could record one. A principal with the project's `review` capability now records a decision on the analysis as it
-- stands: accepted, needs changes or rejected, with a note. The row binds the task's attempt and lease generation and
-- the SHA-256 of every researcher-visible output at that moment, so a review names the exact bytes it covered. The
-- reviewer is the authenticated principal. Rows are immutable; a later review is a new row and the newest is current.
-- Additive: no existing function changes, so the running API, coordinator and worker keep working.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_reviews (
  tenant_id text NOT NULL,
  review_id uuid NOT NULL DEFAULT gen_random_uuid(),
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  reviewer_id uuid NOT NULL,
  request_id uuid NOT NULL,
  decision text NOT NULL,
  note text,
  outputs jsonb NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, review_id),
  UNIQUE (tenant_id, reviewer_id, request_id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, reviewer_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (decision IN ('accepted', 'needs_changes', 'rejected')),
  CHECK (note IS NULL OR char_length(note) BETWEEN 1 AND 2000),
  CHECK (decision = 'accepted' OR note IS NOT NULL),
  CHECK (attempt >= 0),
  CHECK (lease_generation >= 0),
  CHECK (jsonb_typeof(outputs) = 'array' AND octet_length(outputs::text) <= 65536)
);
CREATE INDEX IF NOT EXISTS broker_task_reviews_task ON enrichedlab_agent.broker_task_reviews (tenant_id, task_id, created_at);
ALTER TABLE enrichedlab_agent.broker_task_reviews ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_task_reviews FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_task_reviews;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_task_reviews
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- The researcher-visible outputs a review covers: every current artifact of the task, by path and digest.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_review_outputs(p_tenant text, p_task uuid)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT coalesce(jsonb_agg(jsonb_build_object('artifact_id', a.artifact_id, 'relative_path', a.relative_path,
      'sha256', a.sha256) ORDER BY a.relative_path, a.artifact_id), '[]'::jsonb)
  FROM enrichedlab_agent.broker_artifacts a
  WHERE a.tenant_id = p_tenant AND a.task_id = p_task AND a.visibility = 'researcher'
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_review_json(p_tenant text, r enrichedlab_agent.broker_task_reviews)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT jsonb_build_object('review_id', r.review_id, 'task_id', r.task_id, 'attempt', r.attempt,
    'decision', r.decision, 'note', r.note, 'reviewer_id', r.reviewer_id,
    -- The display name only: a task reader may not hold the capability that shows emails.
    'reviewer_name', (SELECT p.display_name FROM enrichedlab_agent.broker_principals p
      WHERE p.tenant_id = p_tenant AND p.principal_id = r.reviewer_id),
    'outputs', r.outputs, 'created_at', r.created_at,
    'covers_current_outputs', r.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, r.task_id))
$$;

-- A reviewer's decision on an analysis as it stands. Idempotent per reviewer and request.
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

-- Every review of a readable task, newest first.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_reviews(p_tenant text, p_actor uuid, p_task uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks WHERE tenant_id = p_tenant AND task_id = p_task
      AND enrichedlab_agent.broker_task_can(p_tenant, p_actor, p_task, 'read')) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found'; END IF;
  RETURN (SELECT coalesce(jsonb_agg(enrichedlab_agent.broker_task_review_json(p_tenant, r) ORDER BY r.created_at DESC, r.review_id), '[]'::jsonb)
    FROM enrichedlab_agent.broker_task_reviews r WHERE r.tenant_id = p_tenant AND r.task_id = p_task);
END $$;

-- The newest review of each analysis in a readable project.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_reviews(p_tenant text, p_actor uuid, p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found'; END IF;
  RETURN (SELECT coalesce(jsonb_agg(enrichedlab_agent.broker_task_review_json(p_tenant, n) ORDER BY n.created_at DESC, n.review_id), '[]'::jsonb)
    FROM enrichedlab_agent.broker_task_reviews n
    WHERE n.tenant_id = p_tenant AND n.review_id IN (
      SELECT DISTINCT ON (r.task_id) r.review_id FROM enrichedlab_agent.broker_task_reviews r
        JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = r.tenant_id AND pt.task_id = r.task_id
        WHERE r.tenant_id = p_tenant AND pt.project_id = p_project
        ORDER BY r.task_id, r.created_at DESC, r.review_id DESC));
END $$;

-- Package: every review of the project's analyses, oldest first per analysis. The API reads it in the same
-- repeatable-read transaction as broker_project_package_snapshot.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_review_records(p_tenant text, p_actor uuid, p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found'; END IF;
  RETURN (SELECT coalesce(jsonb_agg(enrichedlab_agent.broker_task_review_json(p_tenant, r) ORDER BY r.task_id, r.created_at, r.review_id), '[]'::jsonb)
    FROM enrichedlab_agent.broker_task_reviews r
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = r.tenant_id AND pt.task_id = r.task_id
    WHERE r.tenant_id = p_tenant AND pt.project_id = p_project);
END $$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'enrichedlab_agent' AND p.proname IN ('broker_task_review_outputs', 'broker_task_review_json',
      'broker_record_task_review', 'broker_task_reviews', 'broker_project_reviews', 'broker_project_review_records')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (23) ON CONFLICT DO NOTHING;
COMMIT;
