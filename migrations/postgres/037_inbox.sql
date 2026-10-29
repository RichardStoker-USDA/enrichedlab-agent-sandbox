-- What waits for each person. One reader computes, for the caller, the items across their labs
-- that ask for a person: the agent's review requests, finished analyses without a person's review, steps that need a
-- check, papers waiting for a curator's acceptance and shared notes to renew or retire. Each item carries a version, so
-- an item that changes reads as new. A person marks an item as seen; the acknowledgement is an immutable row naming the
-- person, the item and its version. Service accounts read their items but cannot acknowledge. Additive: no existing
-- function changes.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_inbox_acknowledgements (
  tenant_id text NOT NULL,
  principal_id uuid NOT NULL,
  kind text NOT NULL
    CHECK (kind IN ('review_request', 'analysis_review', 'step_check', 'paper_acceptance', 'note_attention')),
  item_ref text NOT NULL CHECK (char_length(item_ref) BETWEEN 1 AND 200),
  item_version text NOT NULL CHECK (char_length(item_version) BETWEEN 1 AND 200),
  request_id uuid NOT NULL,
  acknowledged_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, principal_id, kind, item_ref, item_version),
  UNIQUE (tenant_id, principal_id, request_id),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id)
);
ALTER TABLE enrichedlab_agent.broker_inbox_acknowledgements ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_inbox_acknowledgements FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_inbox_acknowledgements;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_inbox_acknowledgements
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- The caller's items, newest first, each with when the caller saw its current version or null.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_inbox(p_tenant text, p_actor uuid)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  WITH projects AS (
    SELECT p.project_id, p.group_key, p.title, p.revision,
           enrichedlab_agent.broker_project_can(p_tenant, p_actor, p.project_id, 'steer') AS steer,
           enrichedlab_agent.broker_project_can(p_tenant, p_actor, p.project_id, 'review') AS review
    FROM enrichedlab_agent.broker_projects p
    WHERE p.tenant_id = p_tenant
  ),
  latest_runs AS (
    SELECT DISTINCT ON (r.project_id) r.project_id, r.run_id, r.status, r.code, r.turn, r.updated_at
    FROM enrichedlab_agent.broker_research_runs r
    WHERE r.tenant_id = p_tenant
    ORDER BY r.project_id, r.created_at DESC, r.run_id DESC
  ),
  -- The agent stopped and asks for a person: the newest run of a project the caller may steer or review.
  review_requests AS (
    SELECT 'review_request' AS kind, r.run_id::text AS ref, r.run_id::text AS version, r.updated_at AS at,
           jsonb_build_object('lab', p.group_key, 'project_id', p.project_id, 'project_title', p.title, 'run_id', r.run_id,
             'code', r.code, 'turn', r.turn,
             -- The recorded reason, in the agent's own words: the methods check's when it asked for review, else the
             -- lead's summary; none when the run stopped before a decision.
             'reason_source', CASE WHEN t.record->'method_review'->>'decision' = 'needs_review' THEN 'methods_check'
                                   WHEN t.record IS NOT NULL THEN 'lead' END,
             'reason', CASE WHEN t.record->'method_review'->>'decision' = 'needs_review' THEN t.record->'method_review'->>'reason'
                            ELSE t.record->'proposal'->>'summary' END) AS item
    FROM latest_runs r
    JOIN projects p ON p.project_id = r.project_id
    LEFT JOIN LATERAL (
      SELECT rt.record FROM enrichedlab_agent.broker_research_turns rt
      WHERE rt.tenant_id = p_tenant AND rt.run_id = r.run_id ORDER BY rt.turn DESC LIMIT 1
    ) t ON true
    WHERE r.status = 'review_required' AND (p.steer OR p.review)
  ),
  -- Finished analyses whose newest review by a person does not cover their current result files.
  analysis_reviews AS (
    SELECT 'analysis_review' AS kind, t.task_id::text AS ref,
           md5(enrichedlab_agent.broker_task_review_outputs(p_tenant, t.task_id)::text) AS version,
           coalesce(t.finished_at, t.updated_at) AS at,
           jsonb_build_object('lab', p.group_key, 'project_id', p.project_id, 'project_title', p.title, 'task_id', t.task_id,
             'action_key', pt.action_key, 'status', t.status) AS item
    FROM projects p
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = p_tenant AND pt.project_id = p.project_id
    JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = p_tenant AND t.task_id = pt.task_id
    WHERE p.review AND enrichedlab_agent.broker_is_person(p_tenant, p_actor)
      AND t.status IN ('completed', 'review_required')
      AND NOT EXISTS (
        SELECT 1 FROM (
          SELECT rv.outputs FROM enrichedlab_agent.broker_task_reviews rv
          JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = rv.tenant_id AND a.principal_id = rv.reviewer_id
          WHERE rv.tenant_id = p_tenant AND rv.task_id = t.task_id AND a.principal_class = 'person'
          ORDER BY rv.created_at DESC, rv.review_id DESC LIMIT 1
        ) newest
        WHERE newest.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, t.task_id))
  ),
  -- Steps of the current plan whose analysis the system could not confirm, versioned by the revision that marked them.
  step_checks AS (
    SELECT 'step_check' AS kind, p.project_id::text || ':' || (s->>'action_key') AS ref,
           coalesce((SELECT e.revision FROM enrichedlab_agent.broker_plan_events e
                     WHERE e.tenant_id = p_tenant AND e.project_id = p.project_id AND e.action_key = s->>'action_key'
                       AND e.status_after = 'uncertain' ORDER BY e.revision DESC LIMIT 1), p.revision)::text AS version,
           c.created_at AS at,
           jsonb_build_object('lab', p.group_key, 'project_id', p.project_id, 'project_title', p.title,
             'action_key', s->>'action_key', 'title', s->>'title', 'task_id', s->>'task_id') AS item
    FROM projects p
    JOIN enrichedlab_agent.broker_project_checkpoints c
      ON c.tenant_id = p_tenant AND c.project_id = p.project_id AND c.revision = p.revision
    CROSS JOIN LATERAL jsonb_array_elements(c.state->'plan') s
    WHERE p.steer AND s->>'status' = 'uncertain'
  ),
  -- Papers in a lab's library that no curator has accepted, for the lab's curators; one item per lab.
  paper_acceptance AS (
    SELECT 'paper_acceptance' AS kind, w.group_key AS ref, md5(string_agg(w.paper_id::text, ',' ORDER BY w.paper_id)) AS version,
           max(w.created_at) AS at, jsonb_build_object('lab', w.group_key, 'papers', count(*)) AS item
    FROM (
      SELECT lp.group_key, lp.paper_id, lp.created_at FROM enrichedlab_agent.broker_library_papers lp
      WHERE lp.tenant_id = p_tenant AND enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, lp.group_key)
        AND enrichedlab_agent.broker_library_removal(p_tenant, lp.paper_id) IS NULL
        AND enrichedlab_agent.broker_library_acceptance(p_tenant, lp.paper_id) IS NULL
    ) w
    GROUP BY w.group_key
  ),
  -- Shared notes the lead no longer reads because their review-by date passed or their evidence changed.
  note_attention AS (
    SELECT 'note_attention' AS kind, n.note_id::text AS ref,
           (x.state->>'state') || ':' || (x.state->>'review_by') || coalesce(':' || (x.state->>'task_id'), '') AS version,
           (x.state->>'review_by')::timestamptz AS at,
           jsonb_build_object('lab', n.group_key, 'note_id', n.note_id, 'project_id', n.project_id, 'state', x.state->>'state',
             'cause', x.state->>'cause', 'review_by', x.state->>'review_by') AS item
    FROM enrichedlab_agent.broker_lab_notes n
    CROSS JOIN LATERAL (SELECT enrichedlab_agent.broker_lab_note_state(p_tenant, n) AS state) x
    WHERE n.tenant_id = p_tenant AND enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, n.group_key)
      AND x.state->>'state' IN ('expired', 'paused')
  ),
  items AS (
    SELECT * FROM review_requests UNION ALL SELECT * FROM analysis_reviews UNION ALL SELECT * FROM step_checks
    UNION ALL SELECT * FROM paper_acceptance UNION ALL SELECT * FROM note_attention
  )
  SELECT jsonb_build_object(
    'can_acknowledge', enrichedlab_agent.broker_is_person(p_tenant, p_actor),
    'items', coalesce(jsonb_agg(i.item || jsonb_build_object('kind', i.kind, 'ref', i.ref, 'version', i.version, 'at', i.at,
      'seen_at', a.acknowledged_at) ORDER BY i.at DESC, i.kind, i.ref), '[]'::jsonb))
  FROM items i
  LEFT JOIN enrichedlab_agent.broker_inbox_acknowledgements a
    ON a.tenant_id = p_tenant AND a.principal_id = p_actor AND a.kind = i.kind AND a.item_ref = i.ref AND a.item_version = i.version
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_inbox(text, uuid) FROM PUBLIC;

-- A person marks one of their current items as seen. The same request again returns the same row; an item that is not
-- the caller's now, or a changed version, is refused.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_inbox_acknowledge(p_tenant text, p_actor uuid, p_kind text,
  p_ref text, p_version text, p_request uuid)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_row enrichedlab_agent.broker_inbox_acknowledgements%ROWTYPE;
BEGIN
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:inbox_person_only';
  END IF;
  SELECT * INTO v_row FROM enrichedlab_agent.broker_inbox_acknowledgements
  WHERE tenant_id = p_tenant AND principal_id = p_actor AND request_id = p_request;
  IF FOUND THEN
    IF v_row.kind IS DISTINCT FROM p_kind OR v_row.item_ref IS DISTINCT FROM p_ref OR v_row.item_version IS DISTINCT FROM p_version THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
    RETURN jsonb_build_object('kind', v_row.kind, 'ref', v_row.item_ref, 'version', v_row.item_version,
      'seen_at', v_row.acknowledged_at);
  END IF;
  IF NOT EXISTS (SELECT 1 FROM jsonb_array_elements(enrichedlab_agent.broker_inbox(p_tenant, p_actor)->'items') i
                 WHERE i->>'kind' = p_kind AND i->>'ref' = p_ref AND i->>'version' = p_version) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:inbox_item_not_found';
  END IF;
  INSERT INTO enrichedlab_agent.broker_inbox_acknowledgements (tenant_id, principal_id, kind, item_ref, item_version, request_id)
  VALUES (p_tenant, p_actor, p_kind, p_ref, p_version, p_request)
  ON CONFLICT (tenant_id, principal_id, kind, item_ref, item_version) DO NOTHING;
  SELECT * INTO v_row FROM enrichedlab_agent.broker_inbox_acknowledgements
  WHERE tenant_id = p_tenant AND principal_id = p_actor AND kind = p_kind AND item_ref = p_ref AND item_version = p_version;
  RETURN jsonb_build_object('kind', v_row.kind, 'ref', v_row.item_ref, 'version', v_row.item_version,
    'seen_at', v_row.acknowledged_at);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_inbox_acknowledge(text, uuid, text, text, text, uuid) FROM PUBLIC;

-- Who saw each review request of a project, for anyone who may read it: the person's display name only, as reviews show.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_acknowledgements(p_tenant text, p_actor uuid, p_project uuid)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  RETURN coalesce((
    SELECT jsonb_agg(jsonb_build_object('run_id', r.run_id, 'principal_id', a.principal_id, 'display_name', p.display_name,
      'seen_at', a.acknowledged_at) ORDER BY a.acknowledged_at, a.principal_id)
    FROM enrichedlab_agent.broker_research_runs r
    JOIN enrichedlab_agent.broker_inbox_acknowledgements a
      ON a.tenant_id = r.tenant_id AND a.kind = 'review_request' AND a.item_ref = r.run_id::text
    JOIN enrichedlab_agent.broker_principals p ON p.tenant_id = a.tenant_id AND p.principal_id = a.principal_id
    WHERE r.tenant_id = p_tenant AND r.project_id = p_project), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_acknowledgements(text, uuid, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (37) ON CONFLICT DO NOTHING;
COMMIT;
