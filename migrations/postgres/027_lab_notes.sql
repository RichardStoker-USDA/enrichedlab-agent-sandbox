-- Lab memory. What the research lead learned stayed inside one project's saved versions: every
-- project started with no memory, and nothing said whether a note had been checked. A person who is a lab lead or
-- reviewer may now share an observation from a lab-visible project with the lab, when every analysis it rests on
-- has a person's acceptance that covers its current result files. The shared note is an immutable record bound to
-- those files and acceptances, with a review-by date and relevance keys (the methods of its analyses and the data
-- its project started from). Retiring and renewing are appended events. Whether a note is active is decided by a
-- fixed rule each time it is read: retired, expired, or paused when a newer person's review of one of its analyses
-- is not an acceptance or its result files changed. The coordinator reads the active notes relevant to a run through
-- a lease-fenced function, and its results read now names each person's review by id so a turn can record exactly
-- which records its context came from. Additive: the running API, coordinator and worker keep working.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_lab_notes (
  tenant_id text NOT NULL,
  note_id uuid NOT NULL DEFAULT gen_random_uuid(),
  group_key text NOT NULL,
  project_id uuid NOT NULL,
  revision bigint NOT NULL,
  kind text NOT NULL,
  statement text NOT NULL,
  statement_sha256 text NOT NULL,
  -- One entry per analysis, ordered by analysis: its references, title, method, result files and the acceptance.
  evidence jsonb NOT NULL,
  methods text[] NOT NULL,
  data text[] NOT NULL,
  shared_by uuid NOT NULL,
  request_id uuid NOT NULL,
  reason text,
  review_by timestamptz NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, note_id),
  UNIQUE (tenant_id, shared_by, request_id),
  FOREIGN KEY (tenant_id, group_key) REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, project_id, revision)
    REFERENCES enrichedlab_agent.broker_project_checkpoints (tenant_id, project_id, revision),
  FOREIGN KEY (tenant_id, shared_by) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (kind = 'observation'),
  CHECK (char_length(statement) BETWEEN 1 AND 2048),
  CHECK (statement_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (jsonb_typeof(evidence) = 'array' AND jsonb_array_length(evidence) BETWEEN 1 AND 64
    AND octet_length(evidence::text) <= 131072),
  CHECK (cardinality(methods) BETWEEN 1 AND 64),
  CHECK (cardinality(data) <= 64),
  CHECK (reason IS NULL OR char_length(reason) BETWEEN 1 AND 600),
  CHECK (review_by > created_at)
);
CREATE INDEX IF NOT EXISTS broker_lab_notes_group ON enrichedlab_agent.broker_lab_notes (tenant_id, group_key, created_at);
CREATE INDEX IF NOT EXISTS broker_lab_notes_project ON enrichedlab_agent.broker_lab_notes (tenant_id, project_id, statement_sha256);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_lab_note_events (
  tenant_id text NOT NULL,
  event_id uuid NOT NULL DEFAULT gen_random_uuid(),
  note_id uuid NOT NULL,
  kind text NOT NULL,
  actor_id uuid NOT NULL,
  request_id uuid NOT NULL,
  reason text,
  review_by timestamptz,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, event_id),
  UNIQUE (tenant_id, actor_id, request_id),
  FOREIGN KEY (tenant_id, note_id) REFERENCES enrichedlab_agent.broker_lab_notes (tenant_id, note_id),
  FOREIGN KEY (tenant_id, actor_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK ((kind = 'retired' AND reason IS NOT NULL AND char_length(reason) BETWEEN 1 AND 600 AND review_by IS NULL)
    OR (kind = 'renewed' AND reason IS NULL AND review_by IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS broker_lab_note_events_note ON enrichedlab_agent.broker_lab_note_events (tenant_id, note_id, created_at);

DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_lab_notes', 'broker_lab_note_events'] LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON enrichedlab_agent.%I FROM PUBLIC', t);
    EXECUTE format('DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.%I FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation()', t);
  END LOOP;
END $$;

-- A person who is a lead or reviewer of the lab: the only principals who share, renew or (with the sharer) retire.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_lab_curator(p_tenant text, p_actor uuid, p_group text)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = m.tenant_id AND a.principal_id = m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_tenants t ON t.tenant_id = m.tenant_id AND t.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND g.enabled
    WHERE m.tenant_id = p_tenant AND m.group_key = p_group AND m.principal_id = p_actor
      AND m.role IN ('lead', 'reviewer') AND a.issuer <> 'kubernetes://serviceaccount')
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_is_person(p_tenant text, p_actor uuid)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_principals p WHERE p.tenant_id = p_tenant
    AND p.principal_id = p_actor AND p.issuer <> 'kubernetes://serviceaccount')
$$;

-- The fixed rule. Retired before expired before paused; the first analysis that fails decides the reason.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_lab_note_state(p_tenant text, n enrichedlab_agent.broker_lab_notes)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE retired enrichedlab_agent.broker_lab_note_events%ROWTYPE; due timestamptz; e jsonb; newest text;
BEGIN
  SELECT * INTO retired FROM enrichedlab_agent.broker_lab_note_events v
    WHERE v.tenant_id = p_tenant AND v.note_id = n.note_id AND v.kind = 'retired'
    ORDER BY v.created_at, v.event_id LIMIT 1;
  due := coalesce((SELECT v.review_by FROM enrichedlab_agent.broker_lab_note_events v
    WHERE v.tenant_id = p_tenant AND v.note_id = n.note_id AND v.kind = 'renewed'
    ORDER BY v.created_at DESC, v.event_id DESC LIMIT 1), n.review_by);
  IF retired.event_id IS NOT NULL THEN
    RETURN jsonb_build_object('state', 'retired', 'review_by', due, 'retired_at', retired.created_at,
      'reason', retired.reason);
  END IF;
  IF statement_timestamp() >= due THEN
    RETURN jsonb_build_object('state', 'expired', 'review_by', due);
  END IF;
  FOR e IN SELECT value FROM jsonb_array_elements(n.evidence) WITH ORDINALITY AS x(value, i) ORDER BY i LOOP
    IF enrichedlab_agent.broker_task_review_outputs(p_tenant, (e->>'task_id')::uuid) IS DISTINCT FROM e->'outputs' THEN
      RETURN jsonb_build_object('state', 'paused', 'review_by', due, 'cause', 'evidence_files_changed',
        'task_id', e->>'task_id');
    END IF;
    newest := (SELECT rv.decision FROM enrichedlab_agent.broker_task_reviews rv
      JOIN enrichedlab_agent.broker_principals p ON p.tenant_id = rv.tenant_id AND p.principal_id = rv.reviewer_id
      WHERE rv.tenant_id = p_tenant AND rv.task_id = (e->>'task_id')::uuid AND p.issuer <> 'kubernetes://serviceaccount'
      ORDER BY rv.created_at DESC, rv.review_id DESC LIMIT 1);
    IF newest IS DISTINCT FROM 'accepted' THEN
      RETURN jsonb_build_object('state', 'paused', 'review_by', due, 'cause', 'evidence_review_changed',
        'task_id', e->>'task_id');
    END IF;
  END LOOP;
  RETURN jsonb_build_object('state', 'active', 'review_by', due);
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_lab_note_json(p_tenant text, n enrichedlab_agent.broker_lab_notes)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT jsonb_build_object('note_id', n.note_id, 'lab', n.group_key, 'project_id', n.project_id,
    'project_title', (SELECT pr.title FROM enrichedlab_agent.broker_projects pr
      WHERE pr.tenant_id = p_tenant AND pr.project_id = n.project_id),
    'revision', n.revision, 'kind', n.kind, 'statement', n.statement, 'statement_sha256', n.statement_sha256,
    'evidence', n.evidence, 'methods', to_jsonb(n.methods), 'data', to_jsonb(n.data),
    'shared_by', jsonb_build_object('principal_id', n.shared_by, 'name', p.display_name,
      'kind', CASE WHEN p.issuer = 'kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END),
    'reason', n.reason, 'shared_at', n.created_at,
    'status', enrichedlab_agent.broker_lab_note_state(p_tenant, n),
    'events', (SELECT coalesce(jsonb_agg(jsonb_build_object('kind', v.kind, 'actor_name', a.display_name,
        'reason', v.reason, 'review_by', v.review_by, 'recorded_at', v.created_at) ORDER BY v.created_at, v.event_id), '[]'::jsonb)
      FROM enrichedlab_agent.broker_lab_note_events v
      JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = v.tenant_id AND a.principal_id = v.actor_id
      WHERE v.tenant_id = p_tenant AND v.note_id = n.note_id))
  FROM enrichedlab_agent.broker_principals p WHERE p.tenant_id = p_tenant AND p.principal_id = n.shared_by
$$;

-- Share an observation from the project's current saved version. Idempotent per person and request.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_share_lab_note(p_tenant text, p_actor uuid, p_project uuid,
  p_revision bigint, p_statement text, p_evidence_refs jsonb, p_reason text, p_review_days integer, p_request uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE pr enrichedlab_agent.broker_projects%ROWTYPE; existing enrichedlab_agent.broker_lab_notes%ROWTYPE;
  saved enrichedlab_agent.broker_lab_notes%ROWTYPE; st jsonb; clean text; days integer; ref text; task uuid;
  artifact uuid; tasks uuid[] := '{}'; evidence jsonb := '[]'::jsonb; accepted record; outs jsonb; sha text;
BEGIN
  SELECT * INTO pr FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = p_project;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found'; END IF;
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:note_person_only'; END IF;
  IF NOT enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, pr.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:note_forbidden'; END IF;
  clean := nullif(btrim(coalesce(p_reason, ''), E' \t\r\n'), '');
  days := coalesce(p_review_days, 180);
  SELECT * INTO existing FROM enrichedlab_agent.broker_lab_notes
    WHERE tenant_id = p_tenant AND shared_by = p_actor AND request_id = p_request;
  IF FOUND THEN
    IF existing.project_id <> p_project OR existing.revision <> p_revision OR existing.statement IS DISTINCT FROM p_statement
      OR existing.reason IS DISTINCT FROM clean THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict'; END IF;
    RETURN enrichedlab_agent.broker_lab_note_json(p_tenant, existing);
  END IF;
  IF p_request IS NULL OR p_statement IS NULL OR jsonb_typeof(p_evidence_refs) IS DISTINCT FROM 'array'
    OR days NOT BETWEEN 30 AND 730 OR (clean IS NOT NULL AND char_length(clean) > 600) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:note_invalid'; END IF;
  -- Sharing discloses the note to every lab member, so it may only come from a project they can already read.
  IF pr.visibility <> 'lab' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:note_project_private'; END IF;
  -- One share at a time per project, so two people cannot share the same note twice.
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-lab-note:' || p_tenant || ':' || p_project::text, 0));
  IF p_revision IS DISTINCT FROM (SELECT x.revision FROM enrichedlab_agent.broker_projects x
      WHERE x.tenant_id = p_tenant AND x.project_id = p_project) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:note_stale'; END IF;
  SELECT c.state INTO st FROM enrichedlab_agent.broker_project_checkpoints c
    WHERE c.tenant_id = p_tenant AND c.project_id = p_project AND c.revision = p_revision;
  IF NOT EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(st->'lessons', '[]'::jsonb)) l
      WHERE l->>'kind' = 'observation' AND l->>'statement' = p_statement AND l->'evidence_refs' = p_evidence_refs) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:note_invalid'; END IF;
  -- Every reference is this project's result file or the data it started from; at least one is a result.
  FOR ref IN SELECT value FROM jsonb_array_elements_text(p_evidence_refs) LOOP
    IF ref ~ '^artifact://tasks/[0-9a-f-]{36}/[0-9a-f-]{36}$' THEN
      task := split_part(substr(ref, 18), '/', 1)::uuid;
      artifact := split_part(substr(ref, 18), '/', 2)::uuid;
      IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_tasks pt
          JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id = pt.tenant_id AND a.task_id = pt.task_id
          WHERE pt.tenant_id = p_tenant AND pt.project_id = p_project AND pt.task_id = task
            AND a.artifact_id = artifact AND a.visibility = 'researcher') THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:note_invalid'; END IF;
      IF NOT task = ANY(tasks) THEN tasks := tasks || task; END IF;
    ELSIF NOT ref = ANY(ARRAY(SELECT jsonb_array_elements_text(coalesce(st->'workspace'->'input_refs', '[]'::jsonb)))) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:note_invalid';
    END IF;
  END LOOP;
  IF cardinality(tasks) = 0 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:note_invalid'; END IF;
  -- Each analysis's newest person's review is an acceptance of its current result files.
  FOREACH task IN ARRAY ARRAY(SELECT unnest(tasks) ORDER BY 1) LOOP
    outs := enrichedlab_agent.broker_task_review_outputs(p_tenant, task);
    SELECT rv.review_id, rv.decision, rv.outputs INTO accepted FROM enrichedlab_agent.broker_task_reviews rv
      JOIN enrichedlab_agent.broker_principals p ON p.tenant_id = rv.tenant_id AND p.principal_id = rv.reviewer_id
      WHERE rv.tenant_id = p_tenant AND rv.task_id = task AND p.issuer <> 'kubernetes://serviceaccount'
      ORDER BY rv.created_at DESC, rv.review_id DESC LIMIT 1;
    IF accepted.review_id IS NULL OR accepted.decision <> 'accepted' OR accepted.outputs IS DISTINCT FROM outs THEN
      RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:note_evidence_unaccepted'; END IF;
    evidence := evidence || jsonb_build_array(jsonb_build_object('task_id', task,
      'refs', (SELECT jsonb_agg(r ORDER BY r) FROM jsonb_array_elements_text(p_evidence_refs) r
        WHERE r LIKE 'artifact://tasks/' || task::text || '/%'),
      'title', (SELECT s->>'title' FROM jsonb_array_elements(coalesce(st->'plan', '[]'::jsonb)) s
        WHERE s->>'task_id' = task::text LIMIT 1),
      'method', (SELECT t.profile_key FROM enrichedlab_agent.broker_tasks t WHERE t.tenant_id = p_tenant AND t.task_id = task),
      'outputs', outs, 'review_id', accepted.review_id));
  END LOOP;
  sha := encode(sha256(convert_to(p_statement, 'UTF8')), 'hex');
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_lab_notes n WHERE n.tenant_id = p_tenant AND n.project_id = p_project
      AND n.statement_sha256 = sha AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_lab_note_events v
        WHERE v.tenant_id = n.tenant_id AND v.note_id = n.note_id AND v.kind = 'retired')) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:note_already_shared'; END IF;
  INSERT INTO enrichedlab_agent.broker_lab_notes (tenant_id, group_key, project_id, revision, kind, statement,
      statement_sha256, evidence, methods, data, shared_by, request_id, reason, review_by)
    VALUES (p_tenant, pr.group_key, p_project, p_revision, 'observation', p_statement, sha, evidence,
      ARRAY(SELECT DISTINCT e->>'method' FROM jsonb_array_elements(evidence) e ORDER BY 1),
      ARRAY(SELECT DISTINCT d FROM jsonb_array_elements_text(coalesce(st->'workspace'->'input_refs', '[]'::jsonb)) d ORDER BY 1),
      p_actor, p_request, clean, statement_timestamp() + make_interval(days => days))
    RETURNING * INTO saved;
  INSERT INTO enrichedlab_agent.broker_project_audit (tenant_id, project_id, actor_id, kind, detail)
    VALUES (p_tenant, p_project, p_actor, 'note_shared', jsonb_build_object('note_id', saved.note_id));
  RETURN enrichedlab_agent.broker_lab_note_json(p_tenant, saved);
END $$;

-- Retire a shared note: its sharer or a lab lead or reviewer, with a reason. Idempotent per person and request.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_retire_lab_note(p_tenant text, p_actor uuid, p_note uuid,
  p_reason text, p_request uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE n enrichedlab_agent.broker_lab_notes%ROWTYPE; existing enrichedlab_agent.broker_lab_note_events%ROWTYPE; clean text;
BEGIN
  SELECT * INTO n FROM enrichedlab_agent.broker_lab_notes WHERE tenant_id = p_tenant AND note_id = p_note;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, n.project_id, 'read')
    OR NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_get_groups(p_tenant, p_actor) g WHERE g.group_key = n.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:note_not_found'; END IF;
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:note_person_only'; END IF;
  IF n.shared_by <> p_actor AND NOT enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, n.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:note_forbidden'; END IF;
  clean := nullif(btrim(coalesce(p_reason, ''), E' \t\r\n'), '');
  SELECT * INTO existing FROM enrichedlab_agent.broker_lab_note_events
    WHERE tenant_id = p_tenant AND actor_id = p_actor AND request_id = p_request;
  IF FOUND THEN
    IF existing.note_id <> p_note OR existing.kind <> 'retired' OR existing.reason IS DISTINCT FROM clean THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict'; END IF;
    RETURN enrichedlab_agent.broker_lab_note_json(p_tenant, n);
  END IF;
  IF p_request IS NULL OR clean IS NULL OR char_length(clean) > 600 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:note_invalid'; END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-lab-note:' || p_tenant || ':' || n.project_id::text, 0));
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_lab_note_events v WHERE v.tenant_id = p_tenant
      AND v.note_id = p_note AND v.kind = 'retired') THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:note_retired'; END IF;
  INSERT INTO enrichedlab_agent.broker_lab_note_events (tenant_id, note_id, kind, actor_id, request_id, reason)
    VALUES (p_tenant, p_note, 'retired', p_actor, p_request, clean);
  INSERT INTO enrichedlab_agent.broker_project_audit (tenant_id, project_id, actor_id, kind, detail)
    VALUES (p_tenant, n.project_id, p_actor, 'note_retired', jsonb_build_object('note_id', p_note));
  RETURN enrichedlab_agent.broker_lab_note_json(p_tenant, n);
END $$;

-- Renew a shared note's review-by date: a lab lead or reviewer. Idempotent per person and request.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_renew_lab_note(p_tenant text, p_actor uuid, p_note uuid,
  p_review_days integer, p_request uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE n enrichedlab_agent.broker_lab_notes%ROWTYPE; existing enrichedlab_agent.broker_lab_note_events%ROWTYPE;
BEGIN
  SELECT * INTO n FROM enrichedlab_agent.broker_lab_notes WHERE tenant_id = p_tenant AND note_id = p_note;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, n.project_id, 'read')
    OR NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_get_groups(p_tenant, p_actor) g WHERE g.group_key = n.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:note_not_found'; END IF;
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:note_person_only'; END IF;
  IF NOT enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, n.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:note_forbidden'; END IF;
  SELECT * INTO existing FROM enrichedlab_agent.broker_lab_note_events
    WHERE tenant_id = p_tenant AND actor_id = p_actor AND request_id = p_request;
  IF FOUND THEN
    IF existing.note_id <> p_note OR existing.kind <> 'renewed' THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict'; END IF;
    RETURN enrichedlab_agent.broker_lab_note_json(p_tenant, n);
  END IF;
  IF p_request IS NULL OR p_review_days IS NULL OR p_review_days NOT BETWEEN 30 AND 730 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:note_invalid'; END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-lab-note:' || p_tenant || ':' || n.project_id::text, 0));
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_lab_note_events v WHERE v.tenant_id = p_tenant
      AND v.note_id = p_note AND v.kind = 'retired') THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:note_retired'; END IF;
  INSERT INTO enrichedlab_agent.broker_lab_note_events (tenant_id, note_id, kind, actor_id, request_id, review_by)
    VALUES (p_tenant, p_note, 'renewed', p_actor, p_request, statement_timestamp() + make_interval(days => p_review_days));
  INSERT INTO enrichedlab_agent.broker_project_audit (tenant_id, project_id, actor_id, kind, detail)
    VALUES (p_tenant, n.project_id, p_actor, 'note_renewed', jsonb_build_object('note_id', p_note));
  RETURN enrichedlab_agent.broker_lab_note_json(p_tenant, n);
END $$;

-- Every shared note of a lab the caller belongs to, newest first, with whether the caller may share and renew.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_lab_notes(p_tenant text, p_actor uuid, p_group text)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_get_groups(p_tenant, p_actor) g WHERE g.group_key = p_group) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:lab_not_found'; END IF;
  RETURN jsonb_build_object('lab', p_group,
    'can_curate', enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, p_group),
    'notes', (SELECT coalesce(jsonb_agg(enrichedlab_agent.broker_lab_note_json(p_tenant, n) ORDER BY n.created_at DESC, n.note_id), '[]'::jsonb)
      FROM enrichedlab_agent.broker_lab_notes n
      WHERE n.tenant_id = p_tenant AND n.group_key = p_group
        AND enrichedlab_agent.broker_project_can(p_tenant, p_actor, n.project_id, 'read')));
END $$;

-- For the coordinator holding a run's lease: the lab's active shared notes relevant to the run, not from its own
-- project and readable by the run's actor. Relevant means the note's starting data matches this project's or its
-- methods overlap the run's allowed methods. A fixed order: matching data, then more matching methods, then newest
-- shared, then identifier; at most 32. No sharer or reviewer identity.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_lab_notes(
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
  grp text;
  inputs text[];
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  SELECT p.group_key, ARRAY(SELECT jsonb_array_elements_text(coalesce(c.state->'workspace'->'input_refs', '[]'::jsonb)))
    INTO grp, inputs
  FROM enrichedlab_agent.broker_projects p
  JOIN enrichedlab_agent.broker_project_checkpoints c
    ON c.tenant_id = p.tenant_id AND c.project_id = p.project_id AND c.revision = p.revision
  WHERE p.tenant_id = p_tenant AND p.project_id = r.project_id;
  RETURN (SELECT coalesce(jsonb_agg(x.item ORDER BY x.same_data DESC, x.method_matches DESC, x.created_at DESC, x.note_id), '[]'::jsonb)
    FROM (
      SELECT n.note_id, n.created_at, (n.data && inputs) AS same_data,
        cardinality(ARRAY(SELECT unnest(n.methods) INTERSECT SELECT unnest(r.allowed_profiles))) AS method_matches,
        jsonb_build_object('note_id', n.note_id, 'statement', n.statement, 'statement_sha256', n.statement_sha256,
          'methods', to_jsonb(n.methods),
          'matching_methods', to_jsonb(ARRAY(SELECT m FROM (SELECT unnest(n.methods) INTERSECT SELECT unnest(r.allowed_profiles)) s(m) ORDER BY m)),
          'same_data', n.data && inputs,
          'shared_at', to_char(n.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')) AS item
      FROM enrichedlab_agent.broker_lab_notes n
      WHERE n.tenant_id = p_tenant AND n.group_key = grp AND n.project_id <> r.project_id
        AND (n.data && inputs OR n.methods && r.allowed_profiles)
        AND enrichedlab_agent.broker_project_can(p_tenant, r.actor_id, n.project_id, 'read')
        AND enrichedlab_agent.broker_lab_note_state(p_tenant, n)->>'state' = 'active'
      ORDER BY same_data DESC, method_matches DESC, n.created_at DESC, n.note_id
      LIMIT 32
    ) x);
END;
$$;

-- The coordinator's results read, as in migration 025, with each person's review named by id.
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
      'created_at', to_char(a.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
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
  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id', v.task_id, 'review_id', v.review_id, 'decision', v.decision,
      'note', CASE WHEN v.decision <> 'accepted' THEN v.note END, 'recorded_at', v.created_at,
      'covers_current_outputs', v.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, v.task_id))
      ORDER BY v.task_id), '[]'::jsonb) INTO reviews
  FROM (
    SELECT DISTINCT ON (rv.task_id) rv.task_id, rv.review_id, rv.decision, rv.note, rv.created_at, rv.outputs
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

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'enrichedlab_agent' AND p.proname IN ('broker_lab_curator', 'broker_is_person',
      'broker_lab_note_state', 'broker_lab_note_json', 'broker_share_lab_note', 'broker_retire_lab_note',
      'broker_renew_lab_note', 'broker_lab_notes', 'broker_research_run_lab_notes')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (27) ON CONFLICT DO NOTHING;
COMMIT;
