-- What the workspace needs to open on a person's own work: each
-- person's last look at a project, the project's analyses past the newest 200 a page at a time, and its history read
-- forward from a revision. broker_get_project keeps its migration 040 output, since other functions embed it; the API's
-- project read is broker_read_project, which adds the new fields. broker_list_projects is migration 040's version plus
-- last_look.
BEGIN;

-- One row per person and project: the revision they last looked at and when. It moves forward only.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_project_looks (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  principal_id uuid NOT NULL,
  revision bigint NOT NULL CHECK (revision >= 0),
  looked_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, project_id, principal_id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects (tenant_id, project_id),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id)
);
ALTER TABLE enrichedlab_agent.broker_project_looks ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_project_looks FROM PUBLIC;

-- The caller's last look, or nulls when they have not looked. No access check: callers check read first.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_last_look(p_tenant text, p_project uuid, p_actor uuid)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT coalesce((SELECT jsonb_build_object('revision', l.revision, 'at', l.looked_at)
    FROM enrichedlab_agent.broker_project_looks l
    WHERE l.tenant_id = p_tenant AND l.project_id = p_project AND l.principal_id = p_actor),
    jsonb_build_object('revision', NULL, 'at', NULL));
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_last_look(text, uuid, uuid) FROM PUBLIC;

-- A person records that they looked at a project at a revision no newer than its current one. A lower revision than
-- the stored look leaves it unchanged (a stale page cannot move it back); the same or a higher one moves its time to
-- now, so analyses that finished since then read as new. Service accounts do not look.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_look(p_tenant text, p_actor uuid, p_project uuid,
  p_revision bigint)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  v_current bigint;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:look_person_only';
  END IF;
  SELECT revision INTO v_current FROM enrichedlab_agent.broker_projects
  WHERE tenant_id = p_tenant AND project_id = p_project;
  IF p_revision IS NULL OR p_revision < 0 OR p_revision > v_current THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:look_revision_invalid';
  END IF;
  INSERT INTO enrichedlab_agent.broker_project_looks AS l (tenant_id, project_id, principal_id, revision)
  VALUES (p_tenant, p_project, p_actor, p_revision)
  ON CONFLICT (tenant_id, project_id, principal_id) DO UPDATE
    SET revision = excluded.revision, looked_at = clock_timestamp()
    WHERE l.revision <= excluded.revision;
  RETURN enrichedlab_agent.broker_project_last_look(p_tenant, p_project, p_actor);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_look(text, uuid, uuid, bigint) FROM PUBLIC;

-- The analyses cursor names the last item of a page by its submission time (microseconds, UTC) and task. Clients treat
-- it as opaque; the format may change with a new prefix.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_analyses_cursor(p_submitted timestamptz, p_task uuid)
RETURNS text LANGUAGE sql STABLE SET search_path = pg_catalog AS $$
  SELECT 'a1.' || to_char(p_submitted AT TIME ZONE 'UTC', 'YYYYMMDD"T"HH24MISS.US') || '.' || p_task::text;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_analyses_cursor(timestamptz, uuid) FROM PUBLIC;

-- The analyses after a cursor (or the newest when it is null), newest first, in the project read's item shape, at most
-- p_limit plus one so the caller can tell whether another page exists. No access check: callers check read first.
--
-- A queued analysis that is to run carries its place in line. `ahead` counts the site's runnable queued analyses
-- submitted before it, in the claim's order (migration 010: the claim takes the oldest one whose lab and person have a
-- free slot); it is null when the analysis cannot start as things stand (its lab, person, method or project access no
-- longer allows it, or its attempts are spent). `lab_full` and `person_full` say whether the lab's or the person's
-- active analyses are at their limit now, which holds it back whatever its place. Counts only; no other analysis is
-- named.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_task_items(p_tenant text, p_project uuid,
  p_after_submitted timestamptz, p_after_task uuid, p_limit integer)
RETURNS SETOF jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
  WITH line AS (
    SELECT q.task_id, row_number() OVER (ORDER BY q.submitted_at, q.task_id) - 1 AS ahead
    FROM enrichedlab_agent.broker_tasks q
    JOIN enrichedlab_agent.broker_research_groups qg ON qg.tenant_id = q.tenant_id AND qg.group_key = q.group_key
    WHERE q.tenant_id = p_tenant AND q.status = 'queued' AND q.desired_status = 'run'
      AND q.attempt < q.max_task_attempts AND qg.enabled
      AND enrichedlab_agent.broker_execution_allowed(q.tenant_id, q.task_id)
  )
  SELECT to_jsonb(t) FROM (
    SELECT t.task_id, pt.action_key, pt.parent_task_id, t.profile_key AS profile, t.status, t.attempt, t.updated_at,
      t.submitted_at,
      CASE WHEN fr.task_id IS NULL THEN NULL ELSE jsonb_build_object('title', t.objective, 'feature', fr.feature,
        'source_task', fr.source_task_id, 'table_path', fr.table_path, 'previous', fr.previous_task_id) END AS figure,
      CASE WHEN t.status = 'queued' AND t.desired_status = 'run' THEN jsonb_build_object(
        'ahead', line.ahead,
        'lab_full', coalesce((SELECT count(*) FROM enrichedlab_agent.broker_tasks a
          WHERE a.tenant_id = t.tenant_id AND a.group_key = t.group_key
            AND a.status IN ('leased', 'starting', 'running', 'cancelling') AND a.lease_expires_at >= now())
          >= g.max_active_agents, false),
        'person_full', coalesce((SELECT count(*) FROM enrichedlab_agent.broker_tasks a
          WHERE a.tenant_id = t.tenant_id AND a.group_key = t.group_key AND a.principal_id = t.principal_id
            AND a.status IN ('leased', 'starting', 'running', 'cancelling') AND a.lease_expires_at >= now())
          >= g.max_active_agents_per_principal, false)) END AS queue
    FROM enrichedlab_agent.broker_project_tasks pt
    JOIN enrichedlab_agent.broker_tasks t USING (tenant_id, task_id)
    LEFT JOIN enrichedlab_agent.broker_figure_requests fr ON fr.tenant_id = pt.tenant_id AND fr.task_id = pt.task_id
    LEFT JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = t.tenant_id AND g.group_key = t.group_key
    LEFT JOIN line ON line.task_id = t.task_id
    WHERE pt.tenant_id = p_tenant AND pt.project_id = p_project
      AND (p_after_submitted IS NULL OR (t.submitted_at, t.task_id) < (p_after_submitted, p_after_task))
    ORDER BY t.submitted_at DESC, t.task_id DESC
    LIMIT p_limit + 1
  ) t;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_task_items(text, uuid, timestamptz, uuid, integer) FROM PUBLIC;

-- The API's project read: migration 040's read with the newest 200 analyses in the item shape above, the caller's last
-- look, the number of analyses and the cursor for the analyses past the 200 it holds.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_read_project(p_tenant text, p_actor uuid, p_id uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  v_project jsonb;
  v_tasks jsonb;
  v_total bigint;
  v_last jsonb;
BEGIN
  v_project := enrichedlab_agent.broker_get_project(p_tenant, p_actor, p_id);
  v_tasks := coalesce((SELECT jsonb_agg(i ORDER BY (i->>'submitted_at')::timestamptz DESC, (i->>'task_id')::uuid DESC)
    FROM enrichedlab_agent.broker_project_task_items(p_tenant, p_id, NULL, NULL, 199) i), '[]'::jsonb);
  v_total := (v_project->>'task_count')::bigint;
  v_last := v_tasks->-1;
  RETURN v_project || jsonb_build_object(
    'tasks', v_tasks,
    'last_look', enrichedlab_agent.broker_project_last_look(p_tenant, p_id, p_actor),
    'analyses_total', v_total,
    'analyses_next', CASE WHEN v_total > jsonb_array_length(v_tasks) AND v_last IS NOT NULL
      THEN enrichedlab_agent.broker_analyses_cursor((v_last->>'submitted_at')::timestamptz, (v_last->>'task_id')::uuid)
      END);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_read_project(text, uuid, uuid) FROM PUBLIC;

-- One page of a project's analyses, newest first, after the cursor the project read or the previous page returned.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_analyses(p_tenant text, p_actor uuid, p_id uuid,
  p_cursor text, p_limit integer)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  v_parts text[];
  v_after_submitted timestamptz;
  v_after_task uuid;
  v_items jsonb;
  v_count integer;
  v_last jsonb;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_id, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF p_limit IS NULL OR p_limit NOT BETWEEN 1 AND 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:list_limit_invalid';
  END IF;
  IF p_cursor IS NOT NULL THEN
    v_parts := string_to_array(p_cursor, '.');
    IF cardinality(v_parts) <> 4 OR v_parts[1] <> 'a1' OR v_parts[2] !~ '^[0-9]{8}T[0-9]{6}$'
       OR v_parts[3] !~ '^[0-9]{6}$'
       OR v_parts[4] !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analyses_cursor_invalid';
    END IF;
    -- An explicit Z keeps the parse independent of the session time zone.
    BEGIN
      v_after_submitted := (substr(v_parts[2], 1, 4) || '-' || substr(v_parts[2], 5, 2) || '-' || substr(v_parts[2], 7, 2)
        || 'T' || substr(v_parts[2], 10, 2) || ':' || substr(v_parts[2], 12, 2) || ':' || substr(v_parts[2], 14, 2)
        || '.' || v_parts[3] || 'Z')::timestamptz;
    EXCEPTION WHEN others THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:analyses_cursor_invalid';
    END;
    v_after_task := v_parts[4]::uuid;
  END IF;
  SELECT coalesce(jsonb_agg(i ORDER BY n), '[]'::jsonb), count(*) INTO v_items, v_count
  FROM enrichedlab_agent.broker_project_task_items(p_tenant, p_id, v_after_submitted, v_after_task, p_limit)
    WITH ORDINALITY AS r(i, n);
  IF v_count > p_limit THEN
    v_items := v_items - p_limit;
  END IF;
  v_last := v_items->-1;
  RETURN jsonb_build_object(
    'analyses', v_items,
    'next', CASE WHEN v_count > p_limit THEN enrichedlab_agent.broker_analyses_cursor(
      (v_last->>'submitted_at')::timestamptz, (v_last->>'task_id')::uuid) END,
    'total', (SELECT count(*) FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id = p_tenant AND project_id = p_id));
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_analyses(text, uuid, uuid, text, integer) FROM PUBLIC;

-- A project's checkpoints after a revision, oldest first, so a page that holds revision n reads only what is newer.
-- next_after is the revision to pass next when another page exists, else null.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_history_after(p_tenant text, p_actor uuid, p_id uuid,
  p_after bigint, p_limit integer)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  v_items jsonb;
  v_count integer;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_id, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF p_limit IS NULL OR p_limit NOT BETWEEN 1 AND 100 OR p_after IS NULL OR p_after < 0 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:list_limit_invalid';
  END IF;
  SELECT coalesce(jsonb_agg(to_jsonb(c) - 'tenant_id' ORDER BY c.revision), '[]'::jsonb), count(*)
  INTO v_items, v_count
  FROM (SELECT * FROM enrichedlab_agent.broker_project_checkpoints c
        WHERE c.tenant_id = p_tenant AND c.project_id = p_id AND c.revision > p_after
        ORDER BY c.revision LIMIT p_limit + 1) c;
  IF v_count > p_limit THEN
    v_items := v_items - p_limit;
  END IF;
  RETURN jsonb_build_object('checkpoints', v_items,
    'next_after', CASE WHEN v_count > p_limit THEN (v_items->-1->>'revision')::bigint END);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_history_after(text, uuid, uuid, bigint, integer) FROM PUBLIC;

-- Migration 040's list with each project's last look by the caller.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_projects(p_tenant text,p_actor uuid,p_limit integer)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF p_limit NOT BETWEEN 1 AND 200 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:list_limit_invalid'; END IF;
  RETURN QUERY SELECT (to_jsonb(p)-'tenant_id')||jsonb_build_object(
      'owner',jsonb_build_object('principal_id',o.principal_id,'display_name',o.display_name,
        'kind',CASE WHEN o.issuer='kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END),
      'question',(SELECT c.state->>'goal' FROM enrichedlab_agent.broker_project_checkpoints c
        WHERE c.tenant_id=p.tenant_id AND c.project_id=p.project_id AND c.revision=p.revision),
      'capabilities',(SELECT coalesce(jsonb_agg(k),'[]'::jsonb)
        FROM unnest(ARRAY['read','review','contribute','steer','cancel','manage']) k
        WHERE enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,k)),
      'last_look',enrichedlab_agent.broker_project_last_look(p.tenant_id,p.project_id,p_actor),
      'latest_run',(SELECT jsonb_build_object('run_id',r.run_id,'status',r.status,'code',r.code,'turn',r.turn,
          'max_turns',r.max_turns,'updated_at',r.updated_at)
        FROM enrichedlab_agent.broker_research_runs r
        WHERE r.tenant_id=p.tenant_id AND r.project_id=p.project_id ORDER BY r.created_at DESC,r.run_id LIMIT 1),
      'task_counts',(SELECT jsonb_build_object('total',count(*),
          'active',count(*) FILTER (WHERE t.status IN ('queued','leased','starting','running','cancelling')),
          'finished',count(*) FILTER (WHERE t.status IN ('completed','review_required')),
          'failed',count(*) FILTER (WHERE t.status='failed'),
          -- A finished analysis whose newest review by a person still covers its result files.
          'reviewed',count(*) FILTER (WHERE t.status IN ('completed','review_required') AND v.covers),
          'rejected',count(*) FILTER (WHERE t.status IN ('completed','review_required') AND v.covers AND v.decision='rejected'),
          'changes',count(*) FILTER (WHERE t.status IN ('completed','review_required') AND v.covers AND v.decision='needs_changes'))
        FROM enrichedlab_agent.broker_project_tasks pt
        JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=pt.tenant_id AND t.task_id=pt.task_id
        LEFT JOIN LATERAL (
          SELECT rv.decision, rv.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, rv.task_id) AS covers
          FROM enrichedlab_agent.broker_task_reviews rv
          JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=rv.tenant_id AND a.principal_id=rv.reviewer_id
          WHERE rv.tenant_id=pt.tenant_id AND rv.task_id=pt.task_id AND a.issuer<>'kubernetes://serviceaccount'
          ORDER BY rv.created_at DESC, rv.review_id DESC LIMIT 1) v ON true
        WHERE pt.tenant_id=p.tenant_id AND pt.project_id=p.project_id
          AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr
            WHERE fr.tenant_id=pt.tenant_id AND fr.task_id=pt.task_id)))
    FROM enrichedlab_agent.broker_projects p
    JOIN enrichedlab_agent.broker_principals o ON o.tenant_id=p.tenant_id AND o.principal_id=p.owner_id
    WHERE p.tenant_id=p_tenant AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,'read')
    ORDER BY p.updated_at DESC,p.project_id LIMIT p_limit;
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_projects(text,uuid,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (54) ON CONFLICT DO NOTHING;
COMMIT;
