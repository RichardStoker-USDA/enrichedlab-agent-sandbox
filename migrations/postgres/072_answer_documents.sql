-- Answer documents (first slice). A person who may add to a project asks for an Excel workbook,
-- a Word document or PowerPoint slides of a research run's answer: the decision the run ended on for a person's review
-- or a pause. The request pins that decision (its turn and the SHA-256 of its stored record) and every result table
-- its findings bind to and its answer tables give, as each source's current digest lists them (artifact, package and
-- table digests, rows, columns), with the PNG figures those packages hold. The worker's `server-document` route builds
-- the file from those bytes alone: the run's question, the lead's summary (the turn commit already refused one whose
-- numbers no shown cell holds), each finding with its values filled from the cells it binds, each answer table in full,
-- the parts of the question and the provenance; the broker's own writer makes the file (`document_write.rs`, no
-- template, no other program, no model), and one transaction registers its package, records its receipt and finishes
-- the task, so a cancellation lands before all of it or after all of it. The document is a
-- `document_output` in lineage and stays unvalidated until a person accepts it. A run's plan never lists the route and a
-- general submission never creates its tasks. A document is not an analysis: the lab's project list and its review
-- standing, the inbox and the run's parallel slots leave it out, as they leave out figure versions and lab datasets.
-- Rollback: disable the `server-document` profile, then an older broker leaves any queued document task to fail at its
-- dispatch; the tables and functions can stay.
BEGIN;

-- Routes whose tasks only a person's request creates (migration 059), now with answer documents.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_only_route(p_route text)
RETURNS boolean LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT p_route IN ('server-render', 'server-analysis', 'server-dataset', 'server-document')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_request_only_route(text) FROM PUBLIC;

-- Routes a research run may not list (migration 060), now with answer documents.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_run_excluded_route(p_route text)
RETURNS boolean LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT p_route IN ('server-render', 'server-dataset', 'server-document')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_run_excluded_route(text) FROM PUBLIC;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_document_requests (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  project_id uuid NOT NULL,
  run_id uuid NOT NULL,
  turn integer NOT NULL,
  kind text NOT NULL,
  record_sha256 text NOT NULL,
  sources jsonb NOT NULL,
  figures jsonb NOT NULL,
  request jsonb NOT NULL,
  request_sha256 text NOT NULL,
  principal_id uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, run_id, turn) REFERENCES enrichedlab_agent.broker_research_turns (tenant_id, run_id, turn),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (kind IN ('xlsx', 'docx', 'pptx')),
  CHECK (turn >= 0),
  CHECK (record_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (jsonb_typeof(sources) = 'array' AND jsonb_array_length(sources) <= 32 AND octet_length(sources::text) <= 65536),
  CHECK (jsonb_typeof(figures) = 'array' AND jsonb_array_length(figures) <= 8 AND octet_length(figures::text) <= 8192),
  CHECK (jsonb_typeof(request) = 'object')
);

CREATE INDEX IF NOT EXISTS broker_document_requests_run
  ON enrichedlab_agent.broker_document_requests (tenant_id, run_id, created_at DESC);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_document_receipts (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  status text NOT NULL,
  error_code text,
  message text,
  service_version text,
  image_digest text,
  file jsonb,
  counts jsonb,
  notes jsonb NOT NULL,
  package_sha256 text,
  started_at timestamptz NOT NULL,
  finished_at timestamptz NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_document_requests (tenant_id, task_id),
  CHECK (attempt >= 1),
  CHECK (lease_generation > 0),
  CHECK (status IN ('completed', 'refused', 'failed')),
  CHECK ((status = 'completed') = (error_code IS NULL)),
  CHECK (status <> 'completed' OR (package_sha256 IS NOT NULL AND file IS NOT NULL AND counts IS NOT NULL
    AND service_version IS NOT NULL)),
  CHECK (error_code IS NULL OR error_code ~ '^[a-z][a-z0-9_]{2,63}$'),
  CHECK (message IS NULL OR length(message) BETWEEN 1 AND 300),
  CHECK (service_version IS NULL OR service_version ~ '^[0-9A-Za-z][0-9A-Za-z.+-]{0,63}$'),
  CHECK (image_digest IS NULL OR image_digest ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (file IS NULL OR (jsonb_typeof(file) = 'object' AND octet_length(file::text) <= 2048)),
  CHECK (counts IS NULL OR (jsonb_typeof(counts) = 'object' AND octet_length(counts::text) <= 1024)),
  CHECK (jsonb_typeof(notes) = 'array' AND jsonb_array_length(notes) <= 32 AND octet_length(notes::text) <= 8192),
  CHECK (package_sha256 IS NULL OR package_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (finished_at >= started_at)
);

-- Every request id a person's document request used, with the task it returned. A request the content dedupe answers
-- with an earlier task records its own id too, so retrying it returns that task even after the run moves on.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_document_request_keys (
  tenant_id text NOT NULL,
  request_id uuid NOT NULL,
  task_id uuid NOT NULL,
  principal_id uuid NOT NULL,
  project_id uuid NOT NULL,
  run_id uuid NOT NULL,
  kind text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, request_id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_document_requests (tenant_id, task_id),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (kind IN ('xlsx', 'docx', 'pptx'))
);

DO $$ BEGIN
  ALTER TABLE enrichedlab_agent.broker_document_requests ENABLE ROW LEVEL SECURITY;
  ALTER TABLE enrichedlab_agent.broker_document_receipts ENABLE ROW LEVEL SECURITY;
  REVOKE ALL ON enrichedlab_agent.broker_document_requests FROM PUBLIC;
  REVOKE ALL ON enrichedlab_agent.broker_document_receipts FROM PUBLIC;
  ALTER TABLE enrichedlab_agent.broker_document_request_keys ENABLE ROW LEVEL SECURITY;
  REVOKE ALL ON enrichedlab_agent.broker_document_request_keys FROM PUBLIC;
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_document_request_keys;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_document_request_keys
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_document_requests;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_document_requests
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_document_receipts;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_document_receipts
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
END $$;

-- API: a person's request for a document of a run's answer. The run must have ended on a decision for review or a
-- pause, and the project's group must hold exactly one enabled `server-document` method. Each source is a result
-- package of this project's tasks with a summarized digest listing the cited table; a ref, table or package that does
-- not resolve refuses the request rather than leaving a gap in the document. A request id names one task for good: the
-- same id returns it, whatever the run holds now, and the same id for another run, kind or person is an idempotency
-- conflict.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_document(p_tenant text, p_actor uuid, p_project uuid,
  p_run uuid, p_kind text, p_request_id uuid)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  uuid_pattern constant text := '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}';
  v_group text;
  v_run enrichedlab_agent.broker_research_runs%ROWTYPE;
  v_turn enrichedlab_agent.broker_research_turns%ROWTYPE;
  v_proposal jsonb;
  v_record_sha text;
  v_sources jsonb := '[]'::jsonb;
  v_figures jsonb;
  v_artifact enrichedlab_agent.broker_artifacts%ROWTYPE;
  v_digest jsonb;
  v_table jsonb;
  v_ref_task uuid;
  v_ref_artifact uuid;
  v_profile text;
  v_profiles integer;
  v_request jsonb;
  v_sha text;
  v_task uuid;
  v_key enrichedlab_agent.broker_document_request_keys%ROWTYPE;
  s record;
BEGIN
  IF p_kind IS NULL OR p_kind NOT IN ('xlsx', 'docx', 'pptx') OR p_request_id IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_request_invalid';
  END IF;
  SELECT group_key INTO v_group FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = p_project;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'contribute') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-document-request:' || p_tenant || ':' || p_request_id::text, 0));
  SELECT * INTO v_key FROM enrichedlab_agent.broker_document_request_keys
    WHERE tenant_id = p_tenant AND request_id = p_request_id;
  IF FOUND THEN
    IF v_key.principal_id <> p_actor OR v_key.project_id <> p_project OR v_key.run_id <> p_run OR v_key.kind <> p_kind THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
    RETURN v_key.task_id;
  END IF;
  SELECT * INTO v_run FROM enrichedlab_agent.broker_research_runs
    WHERE tenant_id = p_tenant AND run_id = p_run AND project_id = p_project;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:document_run_unknown';
  END IF;
  -- The decision the run ended on: its newest turn, which stopped for a person's review or paused.
  SELECT * INTO v_turn FROM enrichedlab_agent.broker_research_turns
    WHERE tenant_id = p_tenant AND run_id = p_run ORDER BY turn DESC LIMIT 1;
  IF NOT FOUND OR v_run.status NOT IN ('review_required', 'paused')
    OR coalesce(v_turn.record->>'outcome', '') NOT IN ('review_required', 'paused')
    OR jsonb_typeof(v_turn.record->'proposal') IS DISTINCT FROM 'object' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:document_run_not_ended';
  END IF;
  v_proposal := v_turn.record->'proposal';
  v_record_sha := encode(sha256(convert_to(v_turn.record::text, 'UTF8')), 'hex');
  -- Every result table the findings bind to and the answer tables give, as the source's current digest lists it.
  FOR s IN
    SELECT DISTINCT x.ref, x.tbl FROM (
      SELECT b.value->>'ref' AS ref, b.value->>'table' AS tbl
        FROM jsonb_array_elements(CASE WHEN jsonb_typeof(v_proposal->'findings') = 'array'
          THEN v_proposal->'findings' ELSE '[]'::jsonb END) f
        CROSS JOIN LATERAL jsonb_each(CASE WHEN jsonb_typeof(f->'bindings') = 'object'
          THEN f->'bindings' ELSE '{}'::jsonb END) b
      UNION
      SELECT t->>'ref', t->>'table'
        FROM jsonb_array_elements(CASE WHEN jsonb_typeof(v_proposal->'tables') = 'array'
          THEN v_proposal->'tables' ELSE '[]'::jsonb END) t
    ) x
    ORDER BY 1, 2
  LOOP
    IF s.ref IS NULL OR s.tbl IS NULL OR s.ref !~ ('^artifact://tasks/' || uuid_pattern || '/' || uuid_pattern || '$') THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_source_unknown';
    END IF;
    v_ref_task := substring(s.ref FROM 18 FOR 36)::uuid;
    v_ref_artifact := substring(s.ref FROM 55 FOR 36)::uuid;
    SELECT a.* INTO v_artifact FROM enrichedlab_agent.broker_artifacts a
      JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = a.tenant_id AND pt.task_id = a.task_id
      WHERE a.tenant_id = p_tenant AND a.artifact_id = v_ref_artifact AND a.task_id = v_ref_task
        AND pt.project_id = p_project AND a.role = 'result' AND a.visibility = 'researcher';
    IF NOT FOUND THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_source_unknown';
    END IF;
    SELECT d.digest INTO v_digest FROM enrichedlab_agent.broker_artifact_digests d
      WHERE d.tenant_id = p_tenant AND d.artifact_id = v_artifact.artifact_id AND d.artifact_sha256 = v_artifact.sha256
        AND d.status = 'summarized';
    SELECT e INTO v_table FROM jsonb_array_elements(coalesce(v_digest->'tables', '[]'::jsonb)) e
      WHERE e->>'path' = s.tbl LIMIT 1;
    IF v_table IS NULL OR coalesce(v_table->>'sha256', '') !~ '^[0-9a-f]{64}$'
      OR jsonb_typeof(v_table->'columns') IS DISTINCT FROM 'array' OR coalesce(v_table->>'rows', '') !~ '^[0-9]{1,9}$' THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_source_unknown';
    END IF;
    v_sources := v_sources || jsonb_build_array(jsonb_build_object('ref', s.ref, 'task', v_ref_task,
      'artifact', v_ref_artifact, 'package_sha256', v_artifact.sha256, 'table_path', s.tbl,
      'table_sha256', v_table->>'sha256', 'rows', (v_table->>'rows')::bigint, 'columns', v_table->'columns'));
  END LOOP;
  IF jsonb_array_length(v_sources) > 32 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_sources_too_many';
  END IF;
  -- The R-rendered PNG figures the cited packages hold, as their digests list them: at most eight.
  SELECT coalesce(jsonb_agg(jsonb_build_object('ref', f.ref, 'artifact', f.artifact, 'package_sha256', f.package_sha256,
      'path', f.path, 'sha256', f.sha256) ORDER BY f.ref, f.path), '[]'::jsonb)
    INTO v_figures
    FROM (
      SELECT DISTINCT src->>'ref' AS ref, src->>'artifact' AS artifact, src->>'package_sha256' AS package_sha256,
          g->>'path' AS path, g->>'sha256' AS sha256
        FROM jsonb_array_elements(v_sources) src
        JOIN enrichedlab_agent.broker_artifact_digests d ON d.tenant_id = p_tenant
          AND d.artifact_id = (src->>'artifact')::uuid AND d.artifact_sha256 = src->>'package_sha256'
          AND d.status = 'summarized'
        CROSS JOIN LATERAL jsonb_array_elements(coalesce(d.digest->'figures', '[]'::jsonb)) g
        WHERE lower(g->>'path') LIKE '%.png' AND coalesce(g->>'sha256', '') ~ '^[0-9a-f]{64}$'
          AND length(g->>'path') BETWEEN 1 AND 256
        ORDER BY 1, 4
        LIMIT 8
    ) f;
  SELECT count(*), min(p.profile_key) INTO v_profiles, v_profile
    FROM enrichedlab_agent.broker_agent_profiles p
    JOIN enrichedlab_agent.broker_group_profile_grants g ON g.tenant_id = p.tenant_id AND g.profile_key = p.profile_key
    WHERE p.tenant_id = p_tenant AND p.enabled AND p.route = 'server-document' AND g.group_key = v_group;
  IF v_profiles <> 1 THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:document_method_unavailable';
  END IF;
  v_request := jsonb_build_object('schema', 'enrichedlab.document-request.v1', 'project', p_project, 'run', p_run,
    'turn', v_turn.turn, 'kind', p_kind, 'record_sha256', v_record_sha, 'sources', v_sources, 'figures', v_figures);
  v_sha := encode(sha256(convert_to(v_request::text, 'UTF8')), 'hex');
  -- The same document of the same decision is written once: a request whose pinned content matches one already asked
  -- for returns that task, unless it failed or was cancelled. The lock makes two such requests at once agree.
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-document:' || p_tenant || ':' || v_sha, 0));
  SELECT q.task_id INTO v_task FROM enrichedlab_agent.broker_document_requests q
    JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = q.tenant_id AND t.task_id = q.task_id
    WHERE q.tenant_id = p_tenant AND q.run_id = p_run AND q.request_sha256 = v_sha
      AND t.status NOT IN ('failed', 'cancelled', 'cancelling')
    ORDER BY q.created_at DESC, q.task_id LIMIT 1;
  IF v_task IS NOT NULL THEN
    INSERT INTO enrichedlab_agent.broker_document_request_keys (tenant_id, request_id, task_id, principal_id, project_id,
      run_id, kind) VALUES (p_tenant, p_request_id, v_task, p_actor, p_project, p_run, p_kind);
    RETURN v_task;
  END IF;
  -- The person submission refuses request-only routes, so the request submits the project task and its origin itself.
  v_task := enrichedlab_agent.broker_submit_project_task(p_tenant, p_actor, p_project, 'document-' || p_request_id::text,
    NULL, v_group, v_profile, CASE p_kind WHEN 'xlsx' THEN 'Excel workbook' WHEN 'docx' THEN 'Word document'
      ELSE 'PowerPoint slides' END || ' of the answer (turn ' || v_turn.turn::text || ')',
    'document-' || p_request_id::text, v_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id, task_id, origin, basis, principal_id)
    VALUES (p_tenant, v_task, 'person', 'submission', p_actor) ON CONFLICT (tenant_id, task_id) DO NOTHING;
  INSERT INTO enrichedlab_agent.broker_document_requests (tenant_id, task_id, project_id, run_id, turn, kind,
    record_sha256, sources, figures, request, request_sha256, principal_id)
  VALUES (p_tenant, v_task, p_project, p_run, v_turn.turn, p_kind, v_record_sha, v_sources, v_figures, v_request, v_sha,
    p_actor)
  ON CONFLICT (tenant_id, task_id) DO NOTHING;
  INSERT INTO enrichedlab_agent.broker_document_request_keys (tenant_id, request_id, task_id, principal_id, project_id,
    run_id, kind) VALUES (p_tenant, p_request_id, v_task, p_actor, p_project, p_run, p_kind);
  RETURN v_task;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_request_document(text, uuid, uuid, uuid, text, uuid) FROM PUBLIC;

-- Worker: the request of its current lease with the decision, the question as the decision's checkpoint states it and
-- each source package's place. A source whose artifact no longer holds the pinned bytes drops out of `sources`, so the
-- worker compares the counts with `source_count` and `figure_count` and refuses on a difference. The record digest is
-- checked here again, so a changed record yields no request.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_document(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object('kind', r.kind, 'project', r.project_id, 'project_title', p.title, 'run', r.run_id,
    'turn', r.turn, 'decided_at', u.created_at, 'question', coalesce(c.state->>'goal', ''),
    'record_sha256', r.record_sha256, 'request_sha256', r.request_sha256, 'requested_at', r.created_at,
    'proposal', jsonb_build_object('answer', u.record->'proposal'->'answer',
      'summary', u.record->'proposal'->'summary',
      'findings', coalesce(u.record->'proposal'->'findings', '[]'::jsonb),
      'tables', coalesce(u.record->'proposal'->'tables', '[]'::jsonb),
      'parts', coalesce(u.record->'proposal'->'parts', '[]'::jsonb)),
    'source_count', jsonb_array_length(r.sources), 'figure_count', jsonb_array_length(r.figures),
    'sources', (SELECT coalesce(jsonb_agg(s.value || jsonb_build_object('package_path', a.relative_path,
        'package_bytes', a.size_bytes) ORDER BY s.ordinality), '[]'::jsonb)
      FROM jsonb_array_elements(r.sources) WITH ORDINALITY s
      JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id = r.tenant_id
        AND a.artifact_id = (s.value->>'artifact')::uuid AND a.sha256 = s.value->>'package_sha256'),
    'figures', (SELECT coalesce(jsonb_agg(g.value || jsonb_build_object('package_path', a.relative_path,
        'package_bytes', a.size_bytes) ORDER BY g.ordinality), '[]'::jsonb)
      FROM jsonb_array_elements(r.figures) WITH ORDINALITY g
      JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id = r.tenant_id
        AND a.artifact_id = (g.value->>'artifact')::uuid AND a.sha256 = g.value->>'package_sha256'))
  FROM enrichedlab_agent.broker_tasks t
  JOIN enrichedlab_agent.broker_document_requests r USING (tenant_id, task_id)
  JOIN enrichedlab_agent.broker_research_turns u ON u.tenant_id = r.tenant_id AND u.run_id = r.run_id AND u.turn = r.turn
  JOIN enrichedlab_agent.broker_projects p ON p.tenant_id = r.tenant_id AND p.project_id = r.project_id
  LEFT JOIN enrichedlab_agent.broker_project_checkpoints c ON c.tenant_id = r.tenant_id AND c.project_id = r.project_id
    AND c.revision = u.checkpoint_revision
  WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
    AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running') AND t.desired_status = 'run'
    AND t.lease_expires_at > CURRENT_TIMESTAMP
    AND encode(sha256(convert_to(u.record::text, 'UTF8')), 'hex') = r.record_sha256
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_document(text, uuid, text, integer, bigint) FROM PUBLIC;

-- Worker: one receipt per attempt and lease, under that lease. The same receipt again is accepted; a different one is not.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_document_receipt(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint, p_status text, p_error text, p_message text, p_service_version text,
  p_image_digest text, p_file jsonb, p_counts jsonb, p_notes jsonb, p_package_sha256 text, p_started timestamptz,
  p_finished timestamptz)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v enrichedlab_agent.broker_document_receipts%ROWTYPE;
BEGIN
  -- A completed document is recorded only with its package, by broker_complete_document.
  IF p_status = 'completed' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_receipt_invalid';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks t
      JOIN enrichedlab_agent.broker_document_requests r USING (tenant_id, task_id)
      WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
        AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running')
        AND t.lease_expires_at > CURRENT_TIMESTAMP) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_finished < p_started OR p_started > clock_timestamp() + interval '1 minute' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_receipt_invalid';
  END IF;
  SELECT * INTO v FROM enrichedlab_agent.broker_document_receipts
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = p_attempt AND lease_generation = p_generation;
  IF FOUND THEN
    IF v.status IS DISTINCT FROM p_status OR v.error_code IS DISTINCT FROM p_error
      OR v.package_sha256 IS DISTINCT FROM p_package_sha256 THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
  ELSE
    INSERT INTO enrichedlab_agent.broker_document_receipts (tenant_id, task_id, attempt, lease_generation, status,
      error_code, message, service_version, image_digest, file, counts, notes, package_sha256, started_at, finished_at)
    VALUES (p_tenant, p_task, p_attempt, p_generation, p_status, p_error, p_message, p_service_version,
      p_image_digest, p_file, p_counts, coalesce(p_notes, '[]'::jsonb), p_package_sha256, p_started, p_finished)
    RETURNING * INTO v;
  END IF;
  RETURN jsonb_build_object('status', v.status, 'recorded_at', v.recorded_at);
EXCEPTION WHEN check_violation THEN
  RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_receipt_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_document_receipt(text, uuid, text, integer, bigint, text, text,
  text, text, text, jsonb, jsonb, jsonb, text, timestamptz, timestamptz) FROM PUBLIC;

-- Worker: a written document, in one transaction. Under the full lease fence (owner, attempt and generation, a running
-- task nobody asked to cancel, an unexpired lease) it registers the package and its checksum file, records the
-- package's digest and the completed receipt, and finishes the task. A cancellation or a lost lease before it leaves
-- nothing registered; one after it finds a finished task.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_complete_document(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint, p_service_version text, p_image_digest text, p_file jsonb, p_counts jsonb,
  p_notes jsonb, p_package_path text, p_package_bytes bigint, p_package_sha256 text, p_checksum_path text,
  p_checksum_bytes bigint, p_checksum_sha256 text, p_digest_status text, p_digest jsonb, p_started timestamptz,
  p_finished timestamptz)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v enrichedlab_agent.broker_document_receipts%ROWTYPE;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_tasks t
    JOIN enrichedlab_agent.broker_document_requests r USING (tenant_id, task_id)
    WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
      AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running')
      AND t.desired_status = 'run' AND t.lease_expires_at > clock_timestamp()
    FOR UPDATE OF t;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_finished < p_started OR p_started > clock_timestamp() + interval '1 minute'
    OR p_package_path IS NULL OR p_checksum_path IS NULL OR p_package_sha256 IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_receipt_invalid';
  END IF;
  PERFORM enrichedlab_agent.broker_register_artifact(p_tenant, p_task, p_worker, p_generation, 'researcher', 'result',
    'Answer document and its package', p_package_path, 'application/gzip', p_package_bytes, p_package_sha256);
  PERFORM enrichedlab_agent.broker_register_artifact(p_tenant, p_task, p_worker, p_generation, 'researcher', 'evidence',
    'Package checksum', p_checksum_path, 'text/plain', p_checksum_bytes, p_checksum_sha256);
  IF p_digest_status IS NOT NULL THEN
    PERFORM enrichedlab_agent.broker_record_artifact_digest(p_tenant, p_task, p_worker, p_generation, p_package_path,
      p_package_sha256, p_digest_status, p_digest);
  END IF;
  INSERT INTO enrichedlab_agent.broker_document_receipts (tenant_id, task_id, attempt, lease_generation, status,
    error_code, message, service_version, image_digest, file, counts, notes, package_sha256, started_at, finished_at)
  VALUES (p_tenant, p_task, p_attempt, p_generation, 'completed', NULL, NULL, p_service_version, p_image_digest, p_file,
    p_counts, coalesce(p_notes, '[]'::jsonb), p_package_sha256, p_started, p_finished)
  RETURNING * INTO v;
  IF NOT enrichedlab_agent.broker_finish_task(p_tenant, p_task, p_worker, p_generation, true, 'document_written', '') THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  RETURN jsonb_build_object('status', v.status, 'recorded_at', v.recorded_at);
EXCEPTION WHEN check_violation OR unique_violation THEN
  RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:document_receipt_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_complete_document(text, uuid, text, integer, bigint, text, text, jsonb,
  jsonb, jsonb, text, bigint, text, text, bigint, text, text, jsonb, timestamptz, timestamptz) FROM PUBLIC;

-- API: a document's request and every receipt, for anyone who may read the document's task; null for any other task.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_document_detail(p_tenant text, p_actor uuid, p_task uuid)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  r enrichedlab_agent.broker_document_requests%ROWTYPE;
BEGIN
  IF NOT enrichedlab_agent.broker_task_can(p_tenant, p_actor, p_task, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  SELECT * INTO r FROM enrichedlab_agent.broker_document_requests WHERE tenant_id = p_tenant AND task_id = p_task;
  IF NOT FOUND THEN
    RETURN NULL;
  END IF;
  RETURN jsonb_build_object('task_id', r.task_id, 'project_id', r.project_id, 'run_id', r.run_id, 'turn', r.turn,
    'kind', r.kind, 'record_sha256', r.record_sha256, 'sources', r.sources, 'figures', r.figures,
    'request_sha256', r.request_sha256, 'created_at', r.created_at,
    'status', (SELECT t.status FROM enrichedlab_agent.broker_tasks t WHERE t.tenant_id = p_tenant AND t.task_id = p_task),
    'receipts', coalesce((SELECT jsonb_agg(jsonb_build_object('attempt', x.attempt, 'status', x.status,
        'error_code', x.error_code, 'message', x.message, 'service_version', x.service_version,
        'image_digest', x.image_digest, 'file', x.file, 'counts', x.counts, 'notes', x.notes,
        'package_sha256', x.package_sha256, 'started_at', x.started_at, 'finished_at', x.finished_at)
        ORDER BY x.attempt, x.lease_generation)
      FROM enrichedlab_agent.broker_document_receipts x WHERE x.tenant_id = p_tenant AND x.task_id = p_task), '[]'::jsonb));
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_document_detail(text, uuid, uuid) FROM PUBLIC;

-- API: the documents made from one run, newest first, for anyone who may read the run's project. A document the reader
-- may not read is left out.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_run_documents(p_tenant text, p_actor uuid, p_run uuid)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_research_runs r WHERE r.tenant_id = p_tenant AND r.run_id = p_run
      AND enrichedlab_agent.broker_project_can(p_tenant, p_actor, r.project_id, 'read')) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  -- The newest 50: a run's documents are at most three kinds of each of its decisions, plus any asked for again.
  RETURN coalesce((SELECT jsonb_agg(jsonb_build_object('task_id', n.task_id, 'kind', n.kind, 'turn', n.turn,
      'status', n.status, 'created_at', n.created_at, 'finished_at', n.finished_at,
      'receipt', (SELECT jsonb_build_object('status', x.status, 'error_code', x.error_code, 'message', x.message,
          'file', x.file, 'package_sha256', x.package_sha256)
        FROM enrichedlab_agent.broker_document_receipts x WHERE x.tenant_id = p_tenant AND x.task_id = n.task_id
        ORDER BY x.attempt DESC, x.lease_generation DESC LIMIT 1))
      ORDER BY n.created_at DESC, n.task_id)
    FROM (SELECT d.task_id, d.kind, d.turn, t.status, d.created_at, t.finished_at
      FROM enrichedlab_agent.broker_document_requests d
      JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = d.tenant_id AND t.task_id = d.task_id
      WHERE d.tenant_id = p_tenant AND d.run_id = p_run
        AND enrichedlab_agent.broker_task_can(p_tenant, p_actor, d.task_id, 'read')
      ORDER BY d.created_at DESC, d.task_id LIMIT 50) n), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_run_documents(text, uuid, uuid) FROM PUBLIC;

-- The project read's task items (migration 059) with one addition: a document names its kind, run and turn, so a
-- client can keep it apart from the analyses.
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
      CASE WHEN ar.task_id IS NULL THEN NULL ELSE jsonb_build_object('title', t.objective, 'feature', ar.feature,
        'source_task', ar.source_task_id, 'table_path', ar.table_path, 'previous', ar.previous_task_id)
        END AS template_analysis,
      CASE WHEN dr.task_id IS NULL THEN NULL ELSE jsonb_build_object('title', t.objective, 'operation', dr.operation,
        'dataset', dr.dataset_id, 'previous', dr.previous_task_id) END AS dataset,
      CASE WHEN dq.task_id IS NULL THEN NULL ELSE jsonb_build_object('kind', dq.kind, 'run', dq.run_id,
        'turn', dq.turn) END AS document,
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
    LEFT JOIN enrichedlab_agent.broker_analysis_requests ar ON ar.tenant_id = pt.tenant_id AND ar.task_id = pt.task_id
    LEFT JOIN enrichedlab_agent.broker_dataset_requests dr ON dr.tenant_id = pt.tenant_id AND dr.task_id = pt.task_id
    LEFT JOIN enrichedlab_agent.broker_document_requests dq ON dq.tenant_id = pt.tenant_id AND dq.task_id = pt.task_id
    LEFT JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = t.tenant_id AND g.group_key = t.group_key
    LEFT JOIN line ON line.task_id = t.task_id
    WHERE pt.tenant_id = p_tenant AND pt.project_id = p_project
      AND (p_after_submitted IS NULL OR (t.submitted_at, t.task_id) < (p_after_submitted, p_after_task))
    ORDER BY t.submitted_at DESC, t.task_id DESC
    LIMIT p_limit + 1
  ) t;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_task_items(text, uuid, timestamptz, uuid, integer) FROM PUBLIC;

-- The lab's project list (migration 059) with one addition: an answer document is not an analysis, so the project's
-- task counts and review standing leave it out, like figure versions and lab datasets.
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
            WHERE fr.tenant_id=pt.tenant_id AND fr.task_id=pt.task_id)
          AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_dataset_requests dr
            WHERE dr.tenant_id=pt.tenant_id AND dr.task_id=pt.task_id)
          AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_document_requests dq
            WHERE dq.tenant_id=pt.tenant_id AND dq.task_id=pt.task_id)))
    FROM enrichedlab_agent.broker_projects p
    JOIN enrichedlab_agent.broker_principals o ON o.tenant_id=p.tenant_id AND o.principal_id=p.owner_id
    WHERE p.tenant_id=p_tenant AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,'read')
    ORDER BY p.updated_at DESC,p.project_id LIMIT p_limit;
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_projects(text,uuid,integer) FROM PUBLIC;

-- The inbox (migration 059) with one addition: a finished answer document is not an analysis waiting for review.
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
             'reason_source', CASE WHEN t.record->'method_review'->>'decision' IN ('needs_review', 'revise') THEN 'methods_check'
                                   WHEN t.record IS NOT NULL THEN 'lead' END,
             'reason', CASE WHEN t.record->'method_review'->>'decision' IN ('needs_review', 'revise') THEN t.record->'method_review'->>'reason'
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
      -- A figure version redraws results the project already holds, and a lab dataset's intake or preparation readies a
      -- lab's own data for analyses, so neither waits for review.
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr
        WHERE fr.tenant_id = p_tenant AND fr.task_id = t.task_id)
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_dataset_requests dr
        WHERE dr.tenant_id = p_tenant AND dr.task_id = t.task_id)
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_document_requests dq
        WHERE dq.tenant_id = p_tenant AND dq.task_id = t.task_id)
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

-- The turn commit (migration 071) with one addition: a person's answer document, which may still be written when the
-- run continues, does not take one of the run's parallel slots, like figure versions and lab datasets.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_commit_research_turn(p_tenant text, p_run uuid, p_worker text, p_generation bigint, p_expected bigint, p_state jsonb, p_actions jsonb, p_record jsonb, p_outcome text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'enrichedlab_agent'
AS $function$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project enrichedlab_agent.broker_projects%ROWTYPE;
  tmpl enrichedlab_agent.broker_profile_templates%ROWTYPE;
  a jsonb; code jsonb; task uuid; tasks jsonb:='[]'; plan jsonb; revision bigint; pending integer;
  cfg jsonb; chosen jsonb; rendered text; pick record; fill record; described jsonb;
  src enrichedlab_agent.broker_sql_sources%ROWTYPE; q jsonb;
  ta jsonb; ac jsonb; route text; parent uuid; request jsonb;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run FOR UPDATE;
  IF NOT FOUND OR r.status<>'running' OR r.lease_owner IS DISTINCT FROM p_worker OR r.generation<>p_generation OR r.lease_until<=clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  SELECT * INTO project FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=r.project_id FOR UPDATE;
  IF NOT enrichedlab_agent.broker_research_authorized(p_tenant,r.actor_id,r.project_id,r.allowed_profiles) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden'; END IF;
  IF project.revision<>p_expected THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  IF r.turn>=r.max_turns OR p_outcome NOT IN ('delegate','review_required','paused','revise')
    OR jsonb_typeof(p_actions) IS DISTINCT FROM 'array' OR jsonb_array_length(p_actions)>r.max_parallel
    OR (p_outcome='delegate') IS DISTINCT FROM (jsonb_array_length(p_actions)>0)
    OR jsonb_typeof(p_record) IS DISTINCT FROM 'object' OR octet_length(p_record::text)>131072 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  -- A revision keeps the proposal it sent back as `revised_proposal`, never as `proposal`, so no
  -- reader of answers takes it for one; a run is sent back at most twice.
  IF p_outcome='revise' AND (p_record ? 'proposal' OR jsonb_typeof(p_record->'revised_proposal') IS DISTINCT FROM 'object') THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  IF p_outcome='revise' AND (SELECT count(*) FROM enrichedlab_agent.broker_research_turns t
      WHERE t.tenant_id=p_tenant AND t.run_id=p_run AND t.record->>'outcome'='revise') >= 2 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:revision_budget_spent'; END IF;
  SELECT count(*) INTO pending FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND t.status IN ('queued','leased','starting','running','cancelling')
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr WHERE fr.tenant_id=pt.tenant_id AND fr.task_id=pt.task_id)
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_dataset_requests dr WHERE dr.tenant_id=pt.tenant_id AND dr.task_id=pt.task_id)
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_document_requests dq WHERE dq.tenant_id=pt.tenant_id AND dq.task_id=pt.task_id);
  IF pending+jsonb_array_length(p_actions)>r.max_parallel THEN RAISE EXCEPTION USING ERRCODE='53300',MESSAGE='broker:group_queue_full'; END IF;
  FOR a IN SELECT value FROM jsonb_array_elements(p_actions) LOOP
    IF (a->>'profile'=ANY(r.allowed_profiles)) IS NOT TRUE
      OR (a->>'action_key' ~ '^[a-zA-Z0-9_-]{1,100}$') IS NOT TRUE
      OR (a->>'request_sha256' ~ '^[0-9a-f]{64}$') IS NOT TRUE OR NOT EXISTS (
        SELECT 1 FROM jsonb_array_elements(p_state->'plan') s WHERE s->>'action_key'=a->>'action_key' AND s->>'status'='pending' AND s->>'task_id' IS NULL
      ) OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=r.project_id AND action_key=a->>'action_key') THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
    code:=a->'generated_code'; cfg:=NULL; described:=NULL; q:=a->'query';
    ta:=a->'template_analysis'; ac:=NULL; parent:=NULL;
    SELECT f.route INTO route FROM enrichedlab_agent.broker_agent_profiles f WHERE f.tenant_id=p_tenant AND f.profile_key=a->>'profile';
    SELECT * INTO tmpl FROM enrichedlab_agent.broker_profile_templates WHERE tenant_id=p_tenant AND profile_key=a->>'profile';
    IF FOUND THEN
      -- Every comparison is NULL-safe: a missing field must refuse, not pass.
      IF jsonb_typeof(code) IS DISTINCT FROM 'object'
        OR EXISTS (SELECT 1 FROM jsonb_object_keys(code) k WHERE k NOT IN ('template_ref','path','content','sha256','admission','configuration'))
        OR (code->>'template_ref') IS DISTINCT FROM tmpl.template_ref
        OR (code->>'path') IS DISTINCT FROM tmpl.block_path
        OR jsonb_typeof(code->'content') IS DISTINCT FROM 'string'
        OR (octet_length(code->>'content') BETWEEN 1 AND 16384) IS NOT TRUE
        OR (code->>'sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(code->>'content','UTF8')),'hex')
        OR jsonb_typeof(code->'admission') IS DISTINCT FROM 'object'
        OR (code->'admission'->>'result') IS DISTINCT FROM 'accepted'
        OR (code->'admission'->>'block_sha256') IS DISTINCT FROM (code->>'sha256') THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
      -- A configured method carries settings, never code: render its block again from the
      -- template row and refuse any difference. A code template carries no configuration.
      cfg:=tmpl.contract->'configuration';
      IF cfg IS NULL THEN
        IF code ? 'configuration' THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
      ELSE
        chosen:=code->'configuration'->'settings';
        IF jsonb_typeof(code->'configuration') IS DISTINCT FROM 'object'
          OR EXISTS (SELECT 1 FROM jsonb_object_keys(code->'configuration') k WHERE k NOT IN ('schema','block_sha256','settings'))
          OR (code->'configuration'->>'schema') IS DISTINCT FROM 'enrichedlab.configured-block.v1'
          OR (cfg->>'schema') IS DISTINCT FROM 'enrichedlab.configured-block.v1'
          OR jsonb_typeof(cfg->'block') IS DISTINCT FROM 'string'
          OR (cfg->>'block_sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(cfg->>'block','UTF8')),'hex')
          OR (code->'configuration'->>'block_sha256') IS DISTINCT FROM (cfg->>'block_sha256')
          OR jsonb_typeof(chosen) IS DISTINCT FROM 'object'
          OR jsonb_typeof(cfg->'settings') IS DISTINCT FROM 'object'
          OR (SELECT array_agg(k ORDER BY k) FROM jsonb_object_keys(chosen) k)
            IS DISTINCT FROM (SELECT array_agg(k ORDER BY k) FROM jsonb_object_keys(cfg->'settings') k) THEN
          RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
        rendered:=cfg->>'block'; described:='[]'::jsonb;
        FOR pick IN SELECT key,value FROM jsonb_each(chosen) ORDER BY key LOOP
          IF jsonb_typeof(pick.value) IS DISTINCT FROM 'string'
            OR jsonb_typeof(cfg->'settings'->pick.key->'choices'->(pick.value#>>'{}')->'slots') IS DISTINCT FROM 'object' THEN
            RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
          FOR fill IN SELECT key,value FROM jsonb_each_text(cfg->'settings'->pick.key->'choices'->(pick.value#>>'{}')->'slots') LOOP
            rendered:=replace(rendered,'{{'||fill.key||'}}',fill.value);
          END LOOP;
          -- The words the workspace shows come from the template row, never from the proposal.
          described:=described||jsonb_build_array(jsonb_build_object('setting',pick.key,
            'label',cfg->'settings'->pick.key->>'label','choice',pick.value#>>'{}',
            'choice_label',cfg->'settings'->pick.key->'choices'->(pick.value#>>'{}')->>'label'));
        END LOOP;
        IF strpos(rendered,'{{')>0 OR rendered IS DISTINCT FROM (code->>'content') THEN
          RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
      END IF;
    ELSIF code IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid';
    END IF;
    -- A source's method carries the query its guard admitted, and nothing else runs it: no code, and
    -- no query for any other method. The guard ran in the coordinator; the row keeps its text and digest.
    SELECT * INTO src FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id=p_tenant AND profile_key=a->>'profile';
    IF src.source_key IS NOT NULL THEN
      IF code IS NOT NULL OR jsonb_typeof(q) IS DISTINCT FROM 'object'
        OR EXISTS (SELECT 1 FROM jsonb_object_keys(q) k WHERE k NOT IN ('source','proposed_sql','admitted_sql','sha256','guard','relations','row_limit','reviewed_query'))
        OR (q->>'source') IS DISTINCT FROM src.source_key
        OR jsonb_typeof(q->'proposed_sql') IS DISTINCT FROM 'string'
        OR (octet_length(q->>'proposed_sql') BETWEEN 1 AND 8192) IS NOT TRUE
        OR jsonb_typeof(q->'admitted_sql') IS DISTINCT FROM 'string'
        OR (octet_length(q->>'admitted_sql') BETWEEN 1 AND 8300) IS NOT TRUE
        OR (q->>'sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(q->>'admitted_sql','UTF8')),'hex')
        OR (q->>'guard') IS DISTINCT FROM 'guarded-sql/v1'
        OR jsonb_typeof(q->'relations') IS DISTINCT FROM 'array' OR jsonb_array_length(q->'relations')=0
        OR EXISTS (SELECT 1 FROM jsonb_array_elements(q->'relations') x WHERE jsonb_typeof(x) IS DISTINCT FROM 'string'
          OR NOT EXISTS (SELECT 1 FROM jsonb_array_elements(src.relations) s WHERE s->>'name'=x#>>'{}'))
        OR (CASE WHEN (q->>'row_limit') ~ '^[0-9]{1,6}$' THEN (q->>'row_limit')::integer BETWEEN 0 AND src.row_cap ELSE false END) IS NOT TRUE THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:query_invalid'; END IF;
    ELSIF q IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:query_invalid';
    END IF;
    -- The lab's template-analysis method carries the analysis the lead chose and nothing else. The database
    -- runs the checks a person's request runs, as the run's actor, so the lead can name only a completed analysis of this
    -- project, a table its newest summarized digest lists, a feature of the newest catalog and settings it declares.
    IF route='server-analysis' THEN
      -- Three steps fix the order: the key and field tests run only on an object, the cast only on a well-formed id.
      IF code IS NOT NULL OR q IS NOT NULL OR jsonb_typeof(ta) IS DISTINCT FROM 'object' THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:template_analysis_invalid'; END IF;
      IF EXISTS (SELECT 1 FROM jsonb_object_keys(ta) k WHERE k NOT IN ('source_task','artifact','table_path','feature',
          'settings','title','catalog_sha256'))
        OR jsonb_typeof(ta->'source_task') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'artifact') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'table_path') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'feature') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'settings') IS DISTINCT FROM 'object'
        OR jsonb_typeof(ta->'title') IS DISTINCT FROM 'string'
        OR jsonb_typeof(ta->'catalog_sha256') IS DISTINCT FROM 'string' THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:template_analysis_invalid'; END IF;
      IF (ta->>'source_task') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
        OR (ta->>'artifact') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
        OR (ta->>'catalog_sha256') !~ '^[0-9a-f]{64}$' THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:template_analysis_invalid'; END IF;
      -- The table of the result the lead was shown and the catalog it chose from, or the commit is refused.
      ac:=enrichedlab_agent.broker_check_analysis_request(p_tenant,r.actor_id,r.project_id,(ta->>'source_task')::uuid,
        ta->>'table_path',ta->>'feature',ta->'settings',ta->>'title',(ta->>'artifact')::uuid,ta->>'catalog_sha256');
      IF (ac->>'profile') IS DISTINCT FROM (a->>'profile') THEN
        RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:analysis_method_unavailable'; END IF;
      parent:=(ta->>'source_task')::uuid;
    ELSIF ta IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:template_analysis_invalid';
    END IF;
    task:=enrichedlab_agent.broker_submit_project_task(p_tenant,r.actor_id,r.project_id,a->>'action_key',parent,project.group_key,a->>'profile',a->>'objective',
      'research-'||p_run::text||'-'||(r.turn+1)::text||'-'||(a->>'action_key'),a->>'request_sha256');
    IF src.source_key IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_task_queries(tenant_id,task_id,source_key,profile_key,proposed_sql,admitted_sql,
        sql_sha256,guard,relations,row_limit,run_id,turn,reviewed_query)
        VALUES(p_tenant,task,src.source_key,src.profile_key,q->>'proposed_sql',q->>'admitted_sql',q->>'sha256',q->>'guard',
          ARRAY(SELECT jsonb_array_elements_text(q->'relations')),(q->>'row_limit')::integer,p_run,r.turn+1,q->'reviewed_query');
    END IF;
    IF code IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_task_generated_code(tenant_id,task_id,profile_key,template_ref,template_sha256,path,
        content,size_bytes,sha256,admission,run_id,turn,configuration)
        VALUES(p_tenant,task,a->>'profile',tmpl.template_ref,tmpl.template_sha256,tmpl.block_path,code->>'content',
          octet_length(code->>'content'),code->>'sha256',code->'admission',p_run,r.turn+1,
          CASE WHEN cfg IS NULL THEN NULL ELSE (code->'configuration')||jsonb_build_object('described',described) END);
    END IF;
    IF ac IS NOT NULL THEN
      request:=jsonb_build_object('schema','enrichedlab.template-analysis-request.v1','project',r.project_id,
        'source_task',parent,'source_package_sha256',ac->>'package','table_path',ta->>'table_path',
        'table_sha256',ac->>'table_sha256','feature',ta->>'feature','settings',ta->'settings','title',ac->>'title',
        'previous',NULL,'catalog_sha256',ac->>'catalog_sha256',
        'origin',jsonb_build_object('run',p_run,'turn',r.turn+1,'action_key',a->>'action_key'));
      INSERT INTO enrichedlab_agent.broker_analysis_requests (tenant_id,task_id,project_id,source_task_id,
        source_artifact_id,source_package_sha256,table_path,table_sha256,table_rows,table_columns,feature,settings,
        title,previous_task_id,catalog_sha256,request,request_sha256,principal_id)
      VALUES (p_tenant,task,r.project_id,parent,(ac->>'artifact')::uuid,ac->>'package',ta->>'table_path',
        ac->>'table_sha256',(ac->>'rows')::bigint,ac->'columns',ta->>'feature',ta->'settings',ac->>'title',NULL,
        ac->>'catalog_sha256',request,encode(sha256(convert_to(request::text,'UTF8')),'hex'),r.actor_id);
    END IF;
    tasks:=tasks||jsonb_build_array(jsonb_build_object('task_id',task,'action_key',a->>'action_key'));
    SELECT jsonb_agg(CASE WHEN s->>'action_key'=a->>'action_key' THEN s||jsonb_build_object('task_id',task,'status','active') ELSE s END ORDER BY ord)
      INTO plan FROM jsonb_array_elements(p_state->'plan') WITH ORDINALITY AS e(s,ord);
    p_state:=jsonb_set(p_state,'{plan}',plan);
  END LOOP;
  revision:=enrichedlab_agent.broker_write_checkpoint(p_tenant,r.actor_id,r.project_id,p_expected,gen_random_uuid(),p_state,'lead_turn',p_run,r.turn+1,NULL);
  INSERT INTO enrichedlab_agent.broker_research_turns VALUES(p_tenant,p_run,r.turn+1,p_generation,p_expected,revision,
    p_record||jsonb_build_object('submitted_tasks',tasks,'actor_id',r.actor_id,'outcome',p_outcome),clock_timestamp());
  UPDATE enrichedlab_agent.broker_research_runs SET turn=turn+1,failures=0,
    status=CASE WHEN p_outcome='delegate' THEN 'waiting' WHEN p_outcome='revise' THEN 'queued' ELSE p_outcome END,
    code=CASE WHEN p_outcome='delegate' THEN 'waiting_for_tools' ELSE p_outcome END,
    available_at=clock_timestamp()+interval '5 seconds',lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=p_run;
  RETURN jsonb_build_object('revision',revision,'tasks',tasks,'turn',r.turn+1);
END $function$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_commit_research_turn(text,uuid,text,bigint,bigint,jsonb,jsonb,jsonb,text) FROM PUBLIC;

-- The task lineage (migration 071) with one addition: a document names the run, turn and record it restates and its
-- pinned sources, its outputs are `document_output`, and it stays unvalidated until a person accepts it.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_lineage(p_tenant text, p_actor uuid, p_task uuid)
 RETURNS jsonb
 LANGUAGE plpgsql
 STABLE SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'enrichedlab_agent'
AS $function$
DECLARE t enrichedlab_agent.broker_tasks%ROWTYPE; project uuid; action text;
  o enrichedlab_agent.broker_task_origins%ROWTYPE; g enrichedlab_agent.broker_task_generated_code%ROWTYPE;
  m enrichedlab_agent.broker_task_methods%ROWTYPE;
  step jsonb; step_revision bigint; step_now jsonb; step_now_revision bigint; plan jsonb;
  workspace jsonb; workspace_revision bigint; turn_revision bigint; tool jsonb;
  tool_class text; output_class text; gaps jsonb:='[]'; outputs jsonb; has_template boolean;
  q enrichedlab_agent.broker_task_queries%ROWTYPE; src enrichedlab_agent.broker_sql_sources%ROWTYPE;
  classes jsonb:='[]'; classes_by text;
  fr enrichedlab_agent.broker_figure_requests%ROWTYPE; fx enrichedlab_agent.broker_figure_receipts%ROWTYPE;
  ar enrichedlab_agent.broker_analysis_requests%ROWTYPE; ax enrichedlab_agent.broker_analysis_receipts%ROWTYPE;
  dr enrichedlab_agent.broker_dataset_requests%ROWTYPE; dx enrichedlab_agent.broker_dataset_receipts%ROWTYPE;
  ds enrichedlab_agent.broker_datasets%ROWTYPE;
  wr enrichedlab_agent.broker_document_requests%ROWTYPE; wx enrichedlab_agent.broker_document_receipts%ROWTYPE;
  account_name text; account_kind text; unpinned boolean;
BEGIN
  SELECT * INTO t FROM enrichedlab_agent.broker_tasks WHERE tenant_id=p_tenant AND task_id=p_task;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_task_can(p_tenant,p_actor,p_task,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:task_not_found'; END IF;
  SELECT pt.project_id,pt.action_key INTO project,action FROM enrichedlab_agent.broker_project_tasks pt
    WHERE pt.tenant_id=p_tenant AND pt.task_id=p_task;
  SELECT * INTO o FROM enrichedlab_agent.broker_task_origins WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO g FROM enrichedlab_agent.broker_task_generated_code WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO m FROM enrichedlab_agent.broker_task_methods WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO q FROM enrichedlab_agent.broker_task_queries WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO fr FROM enrichedlab_agent.broker_figure_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF fr.task_id IS NOT NULL THEN
    SELECT * INTO fx FROM enrichedlab_agent.broker_figure_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
  END IF;
  SELECT * INTO ar FROM enrichedlab_agent.broker_analysis_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF ar.task_id IS NOT NULL THEN
    SELECT * INTO ax FROM enrichedlab_agent.broker_analysis_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
  END IF;
  SELECT * INTO dr FROM enrichedlab_agent.broker_dataset_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF dr.task_id IS NOT NULL THEN
    SELECT * INTO dx FROM enrichedlab_agent.broker_dataset_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
    SELECT * INTO ds FROM enrichedlab_agent.broker_datasets WHERE tenant_id=p_tenant AND dataset_id=dr.dataset_id;
  END IF;
  SELECT * INTO wr FROM enrichedlab_agent.broker_document_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF wr.task_id IS NOT NULL THEN
    SELECT * INTO wx FROM enrichedlab_agent.broker_document_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
  END IF;
  IF q.task_id IS NOT NULL THEN
    SELECT * INTO src FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id=p_tenant AND source_key=q.source_key;
  END IF;
  -- The display name only: a task reader may not hold the capability that shows emails.
  SELECT a.display_name,CASE WHEN a.principal_class <> 'person' THEN 'service_account' ELSE 'person' END
    INTO account_name,account_kind FROM enrichedlab_agent.broker_principals a
    WHERE a.tenant_id=p_tenant AND a.principal_id=coalesce(o.principal_id,t.principal_id);
  has_template:=m.template_ref IS NOT NULL;
  SELECT jsonb_build_object('tool_id',j.tool_id,'catalog_digest',j.catalog_digest,'input_sha256',j.input_sha256)
    INTO tool FROM enrichedlab_agent.broker_task_platform_tool_jobs j
    WHERE j.tenant_id=p_tenant AND j.task_id=p_task ORDER BY j.attempt DESC,j.lease_generation DESC LIMIT 1;

  IF o.task_id IS NULL THEN gaps:=gaps||'"origin_not_recorded"'::jsonb;
  ELSIF o.origin='unrecorded' THEN gaps:=gaps||'"origin_before_records"'::jsonb; END IF;
  IF m.task_id IS NULL THEN gaps:=gaps||'"method_not_recorded"'::jsonb;
  ELSIF m.basis='recorded_later' THEN gaps:=gaps||'"method_recorded_later"'::jsonb; END IF;

  IF project IS NULL THEN
    gaps:=gaps||'"not_in_project"'::jsonb;
  ELSIF fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL OR dr.task_id IS NOT NULL OR wr.task_id IS NOT NULL THEN
    NULL;
  ELSE
    IF o.origin='research_run' THEN
      SELECT u.checkpoint_revision INTO turn_revision FROM enrichedlab_agent.broker_research_turns u
        WHERE u.tenant_id=p_tenant AND u.run_id=o.run_id AND u.turn=o.turn;
    END IF;
    SELECT c.state->'workspace',c.revision INTO workspace,workspace_revision FROM enrichedlab_agent.broker_project_checkpoints c
      WHERE c.tenant_id=p_tenant AND c.project_id=project
        AND (CASE WHEN turn_revision IS NOT NULL THEN c.revision=turn_revision ELSE c.created_at<=t.submitted_at END)
      ORDER BY c.revision DESC LIMIT 1;
    IF workspace IS NULL OR jsonb_typeof(workspace)<>'object' THEN
      workspace:=NULL; gaps:=gaps||'"inputs_not_recorded"'::jsonb;
    END IF;
    -- The step as first recorded for this task, and the newest revision that names it.
    SELECT s,c.revision INTO step,step_revision FROM enrichedlab_agent.broker_project_checkpoints c
      CROSS JOIN LATERAL jsonb_array_elements(coalesce(c.state->'plan','[]'::jsonb)) s
      WHERE c.tenant_id=p_tenant AND c.project_id=project AND s->>'task_id'=p_task::text
      ORDER BY c.revision ASC LIMIT 1;
    SELECT s,c.revision INTO step_now,step_now_revision FROM enrichedlab_agent.broker_project_checkpoints c
      CROSS JOIN LATERAL jsonb_array_elements(coalesce(c.state->'plan','[]'::jsonb)) s
      WHERE c.tenant_id=p_tenant AND c.project_id=project AND s->>'task_id'=p_task::text
      ORDER BY c.revision DESC LIMIT 1;
    IF step IS NULL THEN gaps:=gaps||'"plan_step_not_recorded"'::jsonb; END IF;
  END IF;
  -- What kind of data the task read: a query's source as registered, otherwise the classes the input custodian recorded
  -- for the latest attempt's sealed copies. With neither, a task that had inputs names the gap.
  IF fr.task_id IS NOT NULL THEN
    classes:='["prior_output"]'::jsonb; classes_by:='render_request';
  ELSIF ar.task_id IS NOT NULL THEN
    classes:='["prior_output"]'::jsonb; classes_by:='analysis_request';
  ELSIF dr.task_id IS NOT NULL THEN
    -- An intake reads the file a person sent, as the guard rebuilt it; a preparation reads the intake's kept grid.
    classes:=CASE WHEN dr.operation='intake' THEN '["object_data"]'::jsonb ELSE '["prior_output"]'::jsonb END;
    classes_by:='dataset_request';
  ELSIF wr.task_id IS NOT NULL THEN
    -- A document restates a decision from the result tables it pins.
    classes:='["prior_output"]'::jsonb; classes_by:='document_request';
  ELSIF src.source_key IS NOT NULL THEN
    classes:=jsonb_build_array(src.data_class); classes_by:='query_source';
  ELSE
    SELECT coalesce(jsonb_agg(c ORDER BY c),'[]'::jsonb) INTO classes FROM (SELECT DISTINCT i.input_class c
      FROM enrichedlab_agent.broker_attempt_inputs i WHERE i.tenant_id=p_tenant AND i.task_id=p_task AND i.attempt=t.attempt) x;
    IF jsonb_array_length(classes)>0 THEN classes_by:='input_custodian'; END IF;
  END IF;
  IF (workspace IS NOT NULL OR tool IS NOT NULL) AND jsonb_array_length(classes)=0 THEN
    gaps:=gaps||'"input_class_not_recorded"'::jsonb; END IF;

  -- What ran decides the class first: a block rendered from a configured method's settings,
  -- admitted model-written code, then a reviewed platform tool's job. A template the task was
  -- submitted under, with none of these, ran no model-written code, which is a gap.
  tool_class:=CASE WHEN fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL OR dr.task_id IS NOT NULL OR wr.task_id IS NOT NULL
      THEN 'vetted_service'
    WHEN q.reviewed_query IS NOT NULL THEN 'reviewed_query' WHEN q.task_id IS NOT NULL THEN 'guarded_query'
    WHEN g.task_id IS NOT NULL AND g.configuration IS NOT NULL THEN 'configured'
    WHEN g.task_id IS NOT NULL THEN 'agent_built' WHEN tool IS NOT NULL THEN 'vetted_service'
    WHEN has_template THEN 'agent_built' ELSE 'agent_session' END;
  IF has_template AND g.task_id IS NULL AND tool IS NULL THEN gaps:=gaps||'"code_not_recorded"'::jsonb; END IF;
  output_class:=CASE WHEN wr.task_id IS NOT NULL THEN 'document_output' ELSE CASE tool_class WHEN 'agent_built' THEN 'agent_built_tool_output' WHEN 'vetted_service' THEN 'vetted_tool_output'
    WHEN 'configured' THEN 'configured_tool_output' WHEN 'reviewed_query' THEN 'query_output' WHEN 'guarded_query' THEN 'query_output' ELSE 'agent_session_output' END END;
  -- A figure version or a template analysis ran in the render service, so the image it names is the one its newest
  -- receipt records, and only a completed one's receipt must name one (a refusal comes back before anything runs). The
  -- broker writes a document itself, so a completed one's receipt names the writer's version and the source revision
  -- the release build compiled in. Any other task ran in the image its profile named.
  unpinned:=CASE WHEN fr.task_id IS NOT NULL THEN fx.status='completed' AND fx.image_digest IS NULL
    WHEN ar.task_id IS NOT NULL THEN ax.status='completed' AND ax.image_digest IS NULL
    WHEN wr.task_id IS NOT NULL THEN wx.status='completed' AND coalesce(wx.service_version, '') !~ '^writer[.][0-9]+[+][0-9a-f]{40}$'
    ELSE coalesce(t.profile_snapshot->>'image_ref','') !~ '@sha256:[0-9a-f]{64}$' END;
  IF unpinned THEN
    gaps:=gaps||'"image_not_digest_pinned"'::jsonb; END IF;

  SELECT coalesce(jsonb_agg(jsonb_build_object('artifact_id',a.artifact_id,'role',a.role,'label',a.label,
      'relative_path',a.relative_path,'media_type',a.media_type,'size_bytes',a.size_bytes,'sha256',a.sha256,
      'created_at',a.created_at,'provenance_class',output_class) ORDER BY a.created_at,a.artifact_id),'[]'::jsonb)
    INTO outputs FROM enrichedlab_agent.broker_artifacts a
    WHERE a.tenant_id=p_tenant AND a.task_id=p_task AND a.visibility='researcher';
  -- A figure version or template analysis that did not complete has no files by design: the service refused its
  -- settings or R failed, and its receipt says so. Only a completed one without its files is a gap.
  IF jsonb_array_length(outputs)=0 AND (CASE WHEN fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL OR dr.task_id IS NOT NULL
      OR wr.task_id IS NOT NULL
      THEN t.status='completed'
      ELSE t.status IN ('completed','failed','cancelled','review_required') END) THEN
    gaps:=gaps||'"no_outputs_recorded"'::jsonb; END IF;

  IF step IS NOT NULL THEN
    plan:=jsonb_build_object('action_key',step->>'action_key','title',step->>'title','rationale',step->>'rationale',
      'status',step_now->>'status','revision',step_revision,
      -- A step first recorded after the checkpoint the task was submitted against was added to the plan later.
      'added_after_submission',workspace_revision IS NOT NULL AND step_revision>workspace_revision,
      'current',CASE WHEN step_now_revision<>step_revision AND (step_now->>'title' IS DISTINCT FROM step->>'title'
          OR step_now->>'rationale' IS DISTINCT FROM step->>'rationale')
        THEN jsonb_build_object('title',step_now->>'title','rationale',step_now->>'rationale','revision',step_now_revision) END);
  END IF;

  RETURN jsonb_build_object(
    'schema','enrichedlab.task-lineage.v1',
    'task',jsonb_build_object('task_id',t.task_id,'project_id',project,'action_key',action,'objective',t.objective,
      'status',t.status,'result_class',t.result_class,'error_code',t.error_code,'attempt',t.attempt,
      'submitted_at',t.submitted_at,'started_at',t.started_at,'finished_at',t.finished_at),
    'origin',CASE WHEN o.task_id IS NULL THEN jsonb_build_object('origin','unrecorded','basis',NULL,'principal_id',t.principal_id)
      ELSE jsonb_build_object('origin',o.origin,'basis',o.basis,'principal_id',o.principal_id,'run_id',o.run_id,
        'turn',o.turn,'recorded_at',o.recorded_at) END
      ||jsonb_build_object('principal_name',account_name,'principal_kind',account_kind),
    'plan_step',plan,
    'inputs',jsonb_build_object('workspace',workspace,'checkpoint_revision',workspace_revision,'platform_tool',tool,
      'classes',classes,'classes_recorded_by',classes_by),
    'method',jsonb_build_object('profile',t.profile_key,'profile_name',m.profile_name,'tool_class',tool_class,
      'template_ref',coalesce(g.template_ref,m.template_ref),'template_sha256',coalesce(g.template_sha256,m.template_sha256),
      'recorded',m.basis,'harness',t.profile_snapshot->'harness',
      'role',t.profile_snapshot->'role','tool_bundle',t.profile_snapshot->'tool_bundle'),
    'code',CASE WHEN g.task_id IS NULL THEN NULL ELSE jsonb_build_object('path',g.path,'sha256',g.sha256,
      'size_bytes',g.size_bytes,'admission',g.admission,'configuration',g.configuration,'run_id',g.run_id,'turn',g.turn,
      'created_at',g.created_at) END,
    'query',CASE WHEN q.task_id IS NULL THEN NULL ELSE jsonb_build_object('source',q.source_key,'sha256',q.sql_sha256,
      'reviewed_query',q.reviewed_query,'guard',q.guard,'relations',to_jsonb(q.relations),'row_limit',q.row_limit,'run_id',q.run_id,'turn',q.turn,
      'created_at',q.created_at,'title',src.title,'release',src.release,
      'snapshot_sha256',src.snapshot->>'manifest_sha256','data_class',src.data_class,
      'snapshot_kind',src.snapshot->>'kind','source_version',(SELECT r.source_version FROM enrichedlab_agent.broker_query_receipts r
        WHERE r.tenant_id=p_tenant AND r.task_id=p_task AND r.status='completed'
        ORDER BY r.attempt DESC,r.lease_generation DESC LIMIT 1)) END,
    'render',CASE WHEN fr.task_id IS NULL THEN NULL ELSE jsonb_build_object('source_task',fr.source_task_id,
      'table_path',fr.table_path,'table_sha256',fr.table_sha256,'source_package_sha256',fr.source_package_sha256,
      'feature',fr.feature,'controls',fr.controls,'title',fr.title,'previous',fr.previous_task_id,
      'request_sha256',fr.request_sha256,'created_at',fr.created_at,'service_version',fx.service_version,
      'image_digest',fx.image_digest) END,
    'analysis',CASE WHEN ar.task_id IS NULL THEN NULL ELSE jsonb_build_object('source_task',ar.source_task_id,
      'table_path',ar.table_path,'table_sha256',ar.table_sha256,'source_package_sha256',ar.source_package_sha256,
      'feature',ar.feature,'settings',ar.settings,'title',ar.title,'previous',ar.previous_task_id,
      'request_sha256',ar.request_sha256,'created_at',ar.created_at,'service_version',ax.service_version,
      'image_digest',ax.image_digest,'method',ax.method) END,
    'dataset',CASE WHEN dr.task_id IS NULL THEN NULL ELSE jsonb_build_object('dataset',dr.dataset_id,
      'operation',dr.operation,'file_name',ds.file_name,'file_sha256',ds.sha256,'guard_intake',ds.guard_intake,
      'guard_policy',ds.guard_policy,'grid_sha256',ds.grid_sha256,'spec_sha256',dr.spec_sha256,
      'previous',dr.previous_task_id,'title',dr.title,'request_sha256',dr.request_sha256,'created_at',dr.created_at,
      'executor',dx.executor,'executor_version',dx.executor_version,'table_sha256',dx.table_sha256) END,
    'document',CASE WHEN wr.task_id IS NULL THEN NULL ELSE jsonb_build_object('run_id',wr.run_id,'turn',wr.turn,
      'kind',wr.kind,'record_sha256',wr.record_sha256,'sources',wr.sources,'figures',wr.figures,
      'request_sha256',wr.request_sha256,'created_at',wr.created_at,'service_version',wx.service_version,
      'image_digest',wx.image_digest,'file',wx.file) END,
    'execution',jsonb_build_object('image_ref',t.profile_snapshot->'image_ref','cpu_limit',t.profile_snapshot->'cpu_limit',
      'memory_limit',t.profile_snapshot->'memory_limit','max_run_seconds',t.profile_snapshot->'max_run_seconds',
      'attempts',t.attempt,'scientific_history',(SELECT count(*) FROM enrichedlab_agent.broker_task_scientific_history h
        WHERE h.tenant_id=p_tenant AND h.task_id=p_task)),
    'outputs',outputs,
    'review',jsonb_build_object('state',CASE WHEN wr.task_id IS NOT NULL
      OR tool_class IN ('agent_built','agent_session','configured','guarded_query','reviewed_query') THEN 'unvalidated' ELSE 'not_reviewed' END),
    'gaps',gaps);
END $function$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_lineage(text,uuid,uuid) FROM PUBLIC;

-- Existing coordinator, worker and API signatures keep their role grants; the new functions' grants are in
-- grant_broker_runtime.psql.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (72) ON CONFLICT DO NOTHING;
COMMIT;
