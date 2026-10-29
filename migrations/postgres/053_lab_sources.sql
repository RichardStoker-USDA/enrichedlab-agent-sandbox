-- Live lab database sources. A public reference source names a pinned snapshot
-- that the worker confirms before every query. A lab database is live: its imports keep arriving, so a lab source names
-- the relation in which the database states its own version (`snapshot.kind` is `live`, `snapshot.version_relation`
-- that relation), and the worker reads that relation in the same read-only repeatable-read transaction as the result
-- and records the row with the receipt and the task's lineage, so every result says which version of the lab's data it
-- read. A live snapshot names no manifest: its `declaration_sha256`, the digest of the source's declaration, keys its
-- schema cards instead (migration 047), and a worker older than this migration, which needs a manifest, refuses the
-- query before it connects (`snapshot_not_recorded`), so it can never publish a lab result. The class is `lab_database`
-- exactly when the snapshot is live, and a completed receipt of a lab source must carry the version.
-- No lab is registered here: which lab database the agents may read is decided per lab.
-- Rollback: an older broker reads the same rows and refuses every lab source's query.
BEGIN;

ALTER TABLE enrichedlab_agent.broker_sql_sources DROP CONSTRAINT IF EXISTS broker_sql_sources_data_class_check;
ALTER TABLE enrichedlab_agent.broker_sql_sources ADD CONSTRAINT broker_sql_sources_data_class_check
  CHECK (data_class IN ('public_reference', 'lab_database'));
ALTER TABLE enrichedlab_agent.broker_sql_sources DROP CONSTRAINT IF EXISTS broker_sql_sources_snapshot_shape;
ALTER TABLE enrichedlab_agent.broker_sql_sources ADD CONSTRAINT broker_sql_sources_snapshot_shape CHECK (CASE data_class
  WHEN 'public_reference' THEN coalesce(snapshot->>'manifest_sha256', '') ~ '^[0-9a-f]{64}$' AND NOT snapshot ? 'kind'
  WHEN 'lab_database' THEN snapshot->>'kind' = 'live' AND NOT snapshot ? 'manifest_sha256'
    AND coalesce(snapshot->>'declaration_sha256', '') ~ '^[0-9a-f]{64}$'
    AND coalesce(snapshot->>'version_relation', '') ~ '^[a-z][a-z0-9_]{0,62}$'
  ELSE false END);

-- The digest a source's schema cards are keyed by: a public source's manifest or a lab source's declaration.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_sql_source_card_key(p_snapshot jsonb)
RETURNS text LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT coalesce(p_snapshot->>'manifest_sha256', p_snapshot->>'declaration_sha256')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_sql_source_card_key(jsonb) FROM PUBLIC;

ALTER TABLE enrichedlab_agent.broker_query_receipts ADD COLUMN IF NOT EXISTS source_version jsonb;
ALTER TABLE enrichedlab_agent.broker_query_receipts DROP CONSTRAINT IF EXISTS broker_query_receipts_source_version_check;
ALTER TABLE enrichedlab_agent.broker_query_receipts ADD CONSTRAINT broker_query_receipts_source_version_check
  CHECK (source_version IS NULL OR (jsonb_typeof(source_version) = 'object' AND octet_length(source_version::text) <= 2048));

-- Operator: register a source. A lab source states `"data_class": "lab_database"` and a live snapshot; any other source
-- is public reference data, as before.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_register_sql_source(p_tenant text, p_source jsonb)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v enrichedlab_agent.broker_sql_sources%ROWTYPE;
  class text := coalesce(p_source->>'data_class', 'public_reference');
BEGIN
  IF jsonb_typeof(p_source) IS DISTINCT FROM 'object'
    OR EXISTS (SELECT 1 FROM jsonb_object_keys(p_source) k WHERE k NOT IN ('source', 'profile', 'title', 'description',
      'release', 'schema', 'relations', 'functions', 'row_cap', 'statement_timeout_ms', 'connection_ref', 'snapshot',
      'data_class'))
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(p_source->'relations') x
      WHERE jsonb_typeof(x) IS DISTINCT FROM 'object' OR (x->>'name') !~ '^[a-z][a-z0-9_]{0,62}$'
        OR jsonb_typeof(x->'columns') IS DISTINCT FROM 'array')
    OR class NOT IN ('public_reference', 'lab_database') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_invalid';
  END IF;
  -- The version relation is read by the worker, not by the lead's queries, so it is not one of the listed relations.
  IF class = 'lab_database' AND EXISTS (SELECT 1 FROM jsonb_array_elements(p_source->'relations') x
      WHERE x->>'name' = p_source->'snapshot'->>'version_relation') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_invalid';
  END IF;
  SELECT * INTO v FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id = p_tenant AND source_key = p_source->>'source';
  IF FOUND THEN
    IF v.profile_key IS DISTINCT FROM p_source->>'profile' OR v.snapshot IS DISTINCT FROM p_source->'snapshot'
      OR v.relations IS DISTINCT FROM p_source->'relations' OR v.connection_ref IS DISTINCT FROM p_source->>'connection_ref'
      OR v.data_class IS DISTINCT FROM class THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
    RETURN jsonb_build_object('source', v.source_key, 'created_at', v.created_at);
  END IF;
  INSERT INTO enrichedlab_agent.broker_sql_sources (tenant_id, source_key, profile_key, title, description, release,
    schema_name, relations, functions, row_cap, statement_timeout_ms, connection_ref, snapshot, data_class)
  VALUES (p_tenant, p_source->>'source', p_source->>'profile', p_source->>'title', p_source->>'description',
    p_source->>'release', p_source->>'schema', p_source->'relations',
    ARRAY(SELECT jsonb_array_elements_text(p_source->'functions')), (p_source->>'row_cap')::integer,
    (p_source->>'statement_timeout_ms')::integer, p_source->>'connection_ref', p_source->'snapshot', class)
  RETURNING * INTO v;
  RETURN jsonb_build_object('source', v.source_key, 'created_at', v.created_at);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_register_sql_source(text, jsonb) FROM PUBLIC;

-- Worker: one receipt per attempt and lease, under that lease, also while a cancellation the worker is answering waits
-- for it, now with the version a lab source's database stated in the result's own transaction. A completed receipt of a
-- lab source must carry it and one of a public source must not. The same receipt again, every field equal, is
-- accepted; any difference is refused.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_query_receipt(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint, p_status text, p_error text, p_rows integer, p_columns jsonb,
  p_package_sha256 text, p_server_version text, p_started timestamptz, p_finished timestamptz, p_source_version jsonb)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v enrichedlab_agent.broker_query_receipts%ROWTYPE;
  class text;
BEGIN
  SELECT s.data_class INTO class FROM enrichedlab_agent.broker_tasks t
    JOIN enrichedlab_agent.broker_task_queries q USING (tenant_id, task_id)
    JOIN enrichedlab_agent.broker_sql_sources s ON s.tenant_id = q.tenant_id AND s.source_key = q.source_key
    WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
      AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running', 'cancelling')
      AND t.lease_expires_at > CURRENT_TIMESTAMP;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_finished < p_started OR p_started > clock_timestamp() + interval '1 minute' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:query_receipt_invalid';
  END IF;
  IF (class = 'lab_database' AND p_status = 'completed' AND p_source_version IS NULL)
    OR (class = 'public_reference' AND p_source_version IS NOT NULL)
    OR (p_status <> 'completed' AND p_source_version IS NOT NULL) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:query_receipt_version_invalid';
  END IF;
  SELECT * INTO v FROM enrichedlab_agent.broker_query_receipts
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = p_attempt AND lease_generation = p_generation;
  IF FOUND THEN
    IF v.status IS DISTINCT FROM p_status OR v.error_code IS DISTINCT FROM p_error OR v.row_count IS DISTINCT FROM p_rows
      OR v.columns IS DISTINCT FROM p_columns OR v.package_sha256 IS DISTINCT FROM p_package_sha256
      OR v.server_version IS DISTINCT FROM p_server_version OR v.started_at IS DISTINCT FROM p_started
      OR v.finished_at IS DISTINCT FROM p_finished OR v.source_version IS DISTINCT FROM p_source_version THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
  ELSE
    INSERT INTO enrichedlab_agent.broker_query_receipts (tenant_id, task_id, attempt, lease_generation, status,
      error_code, row_count, columns, package_sha256, server_version, started_at, finished_at, source_version)
    VALUES (p_tenant, p_task, p_attempt, p_generation, p_status, p_error, p_rows, p_columns, p_package_sha256,
      p_server_version, p_started, p_finished, p_source_version)
    RETURNING * INTO v;
  END IF;
  RETURN jsonb_build_object('status', v.status, 'recorded_at', v.recorded_at);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_query_receipt(text, uuid, text, integer, bigint, text, text,
  integer, jsonb, text, text, timestamptz, timestamptz, jsonb) FROM PUBLIC;

-- The receipt as migration 038 took it, for a worker older than this migration: no version, so it cannot complete a
-- lab source's query.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_query_receipt(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint, p_status text, p_error text, p_rows integer, p_columns jsonb,
  p_package_sha256 text, p_server_version text, p_started timestamptz, p_finished timestamptz)
RETURNS jsonb
LANGUAGE sql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT enrichedlab_agent.broker_record_query_receipt(p_tenant, p_task, p_worker, p_attempt, p_generation, p_status,
    p_error, p_rows, p_columns, p_package_sha256, p_server_version, p_started, p_finished, NULL::jsonb)
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_query_receipt(text, uuid, text, integer, bigint, text, text,
  integer, jsonb, text, text, timestamptz, timestamptz) FROM PUBLIC;

-- API: a task's query, its source as a scientist reads it (with its class) and every receipt (with the version a lab
-- source's database stated), for anyone who may read the task.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_query_detail(p_tenant text, p_actor uuid, p_task uuid)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tasks WHERE tenant_id = p_tenant AND task_id = p_task
      AND enrichedlab_agent.broker_task_can(p_tenant, p_actor, p_task, 'read')) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  RETURN (SELECT jsonb_build_object('task_id', q.task_id, 'profile', q.profile_key, 'source', s.source_key,
      'title', s.title, 'description', s.description, 'release', s.release, 'snapshot', s.snapshot,
      'data_class', s.data_class,
      'proposed_sql', q.proposed_sql, 'admitted_sql', q.admitted_sql, 'sql_sha256', q.sql_sha256, 'guard', q.guard,
      'relations', to_jsonb(q.relations), 'row_limit', q.row_limit, 'run_id', q.run_id, 'turn', q.turn,
      'created_at', q.created_at, 'validation', 'unvalidated',
      'receipts', coalesce((SELECT jsonb_agg(jsonb_build_object('attempt', r.attempt, 'status', r.status,
          'error_code', r.error_code, 'row_count', r.row_count, 'columns', r.columns, 'package_sha256', r.package_sha256,
          'server_version', r.server_version, 'source_version', r.source_version, 'started_at', r.started_at,
          'finished_at', r.finished_at)
          ORDER BY r.attempt, r.lease_generation)
        FROM enrichedlab_agent.broker_query_receipts r WHERE r.tenant_id = q.tenant_id AND r.task_id = q.task_id), '[]'::jsonb))
    FROM enrichedlab_agent.broker_task_queries q
    JOIN enrichedlab_agent.broker_sql_sources s ON s.tenant_id = q.tenant_id AND s.source_key = q.source_key
    WHERE q.tenant_id = p_tenant AND q.task_id = p_task);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_query_detail(text, uuid, uuid) FROM PUBLIC;

-- Operator: register a card measured for a registered source, as migration 047 defined it, keyed by the source's
-- manifest or, for a lab source, its declaration digest. The card must name the source and the snapshot the source
-- records, and only relations the source lists. Its digest is computed here from the stored form, so the same
-- card registered again returns the stored row.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_register_sql_source_card(p_tenant text, p_card jsonb)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  s enrichedlab_agent.broker_sql_sources%ROWTYPE;
  v enrichedlab_agent.broker_sql_source_cards%ROWTYPE;
  digest text;
BEGIN
  IF jsonb_typeof(p_card) IS DISTINCT FROM 'object'
    OR p_card->>'schema' IS DISTINCT FROM 'enrichedlab.sql-source-card.v1'
    OR jsonb_typeof(p_card->'relations') IS DISTINCT FROM 'array'
    OR jsonb_typeof(p_card->'joins') IS DISTINCT FROM 'array'
    OR jsonb_typeof(p_card->'values') IS DISTINCT FROM 'array'
    OR jsonb_typeof(p_card->'lookups') IS DISTINCT FROM 'array'
    OR octet_length(p_card::text) > 262144 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_invalid';
  END IF;
  SELECT * INTO s FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id = p_tenant AND source_key = p_card->>'source';
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_not_found';
  END IF;
  IF p_card->>'manifest_sha256' IS DISTINCT FROM enrichedlab_agent.broker_sql_source_card_key(s.snapshot) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_snapshot_mismatch';
  END IF;
  -- The shape the coordinator reads, element by element (the arrays themselves were checked above): a card it could not
  -- read whole is refused here rather than left out of every run. Relations and lookups must be the source's own. A
  -- count is a whole number of at most 18 digits written without a fraction or exponent, and a join matches no more
  -- rows than it has, the rule the coordinator's view applies to the same stored form.
  IF EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'relations') r
      WHERE jsonb_typeof(r) IS DISTINCT FROM 'object' OR jsonb_typeof(r->'name') IS DISTINCT FROM 'string'
        OR jsonb_typeof(r->'rows') IS DISTINCT FROM 'number' OR (r->>'rows') !~ '^(0|[1-9][0-9]{0,17})$'
        OR NOT EXISTS (SELECT 1 FROM jsonb_array_elements(s.relations) x WHERE x->>'name' = r->>'name'))
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'joins') j
      WHERE jsonb_typeof(j) IS DISTINCT FROM 'object' OR jsonb_typeof(j->'on') IS DISTINCT FROM 'string'
        OR jsonb_typeof(j->'rows') IS DISTINCT FROM 'number' OR jsonb_typeof(j->'matched') IS DISTINCT FROM 'number'
        OR CASE WHEN (j->>'rows') ~ '^(0|[1-9][0-9]{0,17})$' AND (j->>'matched') ~ '^(0|[1-9][0-9]{0,17})$'
             THEN (j->>'matched')::numeric > (j->>'rows')::numeric ELSE true END)
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'values') vc
      WHERE jsonb_typeof(vc) IS DISTINCT FROM 'object' OR jsonb_typeof(vc->'column') IS DISTINCT FROM 'string'
        OR jsonb_typeof(vc->'values') IS DISTINCT FROM 'array')
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'lookups') l
      WHERE jsonb_typeof(l) IS DISTINCT FROM 'object' OR jsonb_typeof(l->'relation') IS DISTINCT FROM 'string'
        OR jsonb_typeof(l->'columns') IS DISTINCT FROM 'array' OR jsonb_typeof(l->'entries') IS DISTINCT FROM 'array'
        OR NOT EXISTS (SELECT 1 FROM jsonb_array_elements(s.relations) x WHERE x->>'name' = l->>'relation')) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_invalid';
  END IF;
  IF EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'lookups') l
      WHERE jsonb_array_length(l->'columns') < 2
        OR EXISTS (SELECT 1 FROM jsonb_array_elements(l->'columns') c WHERE jsonb_typeof(c) IS DISTINCT FROM 'string')
        OR EXISTS (SELECT 1 FROM jsonb_array_elements(l->'entries') e
          WHERE jsonb_typeof(e) IS DISTINCT FROM 'array'
            OR jsonb_array_length(e) IS DISTINCT FROM jsonb_array_length(l->'columns'))) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_invalid';
  END IF;
  digest := 'sha256:' || encode(sha256(convert_to(p_card::text, 'UTF8')), 'hex');
  SELECT * INTO v FROM enrichedlab_agent.broker_sql_source_cards
  WHERE tenant_id = p_tenant AND source_key = s.source_key AND card_sha256 = digest;
  IF NOT FOUND THEN
    INSERT INTO enrichedlab_agent.broker_sql_source_cards (tenant_id, source_key, card_sha256, manifest_sha256, card)
    VALUES (p_tenant, s.source_key, digest, p_card->>'manifest_sha256', p_card)
    RETURNING * INTO v;
  END IF;
  RETURN jsonb_build_object('source', v.source_key, 'card_sha256', v.card_sha256, 'created_at', v.created_at);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_register_sql_source_card(text, jsonb) FROM PUBLIC;

-- The run's sources, as migration 047 defined them, each with the newest card registered for its manifest or declaration.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_sql_sources(p_tenant text, p_run uuid, p_worker text,
  p_generation bigint)
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
  RETURN coalesce((
    SELECT jsonb_agg(jsonb_build_object('profile', s.profile_key, 'source', s.source_key, 'title', s.title,
      'description', s.description, 'release', s.release, 'relations', s.relations, 'functions', to_jsonb(s.functions),
      'row_cap', s.row_cap, 'schema', s.schema_name)
      || coalesce((SELECT jsonb_build_object('card', c.card, 'card_sha256', c.card_sha256)
        FROM enrichedlab_agent.broker_sql_source_cards c
        WHERE c.tenant_id = s.tenant_id AND c.source_key = s.source_key
          AND c.manifest_sha256 = enrichedlab_agent.broker_sql_source_card_key(s.snapshot)
        ORDER BY c.created_at DESC, c.card_sha256 LIMIT 1), '{}'::jsonb)
      ORDER BY s.profile_key)
    FROM enrichedlab_agent.broker_sql_sources s
    WHERE s.tenant_id = p_tenant AND s.profile_key = ANY (r.allowed_profiles)), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_sql_sources(text, uuid, text, bigint) FROM PUBLIC;

-- A task's lineage, as migration 041 defined it, with a query's snapshot kind and, for a live lab source, the version its
-- newest completed receipt read.
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
  ELSIF fr.task_id IS NOT NULL THEN
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
  tool_class:=CASE WHEN fr.task_id IS NOT NULL THEN 'vetted_service' WHEN q.task_id IS NOT NULL THEN 'guarded_query'
    WHEN g.task_id IS NOT NULL AND g.configuration IS NOT NULL THEN 'configured'
    WHEN g.task_id IS NOT NULL THEN 'agent_built' WHEN tool IS NOT NULL THEN 'vetted_service'
    WHEN has_template THEN 'agent_built' ELSE 'agent_session' END;
  IF has_template AND g.task_id IS NULL AND tool IS NULL THEN gaps:=gaps||'"code_not_recorded"'::jsonb; END IF;
  output_class:=CASE tool_class WHEN 'agent_built' THEN 'agent_built_tool_output' WHEN 'vetted_service' THEN 'vetted_tool_output'
    WHEN 'configured' THEN 'configured_tool_output' WHEN 'guarded_query' THEN 'query_output' ELSE 'agent_session_output' END;
  -- A figure version ran in the render service, so the image it names is the one its newest receipt records, and only a
  -- drawn version's receipt must name one (a refusal comes back before anything runs); any other task ran in the image
  -- its profile named.
  unpinned:=CASE WHEN fr.task_id IS NOT NULL THEN fx.status='completed' AND fx.image_digest IS NULL
    ELSE coalesce(t.profile_snapshot->>'image_ref','') !~ '@sha256:[0-9a-f]{64}$' END;
  IF unpinned THEN
    gaps:=gaps||'"image_not_digest_pinned"'::jsonb; END IF;

  SELECT coalesce(jsonb_agg(jsonb_build_object('artifact_id',a.artifact_id,'role',a.role,'label',a.label,
      'relative_path',a.relative_path,'media_type',a.media_type,'size_bytes',a.size_bytes,'sha256',a.sha256,
      'created_at',a.created_at,'provenance_class',output_class) ORDER BY a.created_at,a.artifact_id),'[]'::jsonb)
    INTO outputs FROM enrichedlab_agent.broker_artifacts a
    WHERE a.tenant_id=p_tenant AND a.task_id=p_task AND a.visibility='researcher';
  -- A figure version that was not drawn has no files by design: the service refused its settings or R failed, and its
  -- receipt says so. Only a drawn version without its files is a gap.
  IF jsonb_array_length(outputs)=0 AND (CASE WHEN fr.task_id IS NOT NULL THEN t.status='completed'
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
      'guard',q.guard,'relations',to_jsonb(q.relations),'row_limit',q.row_limit,'run_id',q.run_id,'turn',q.turn,
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
    'execution',jsonb_build_object('image_ref',t.profile_snapshot->'image_ref','cpu_limit',t.profile_snapshot->'cpu_limit',
      'memory_limit',t.profile_snapshot->'memory_limit','max_run_seconds',t.profile_snapshot->'max_run_seconds',
      'attempts',t.attempt,'scientific_history',(SELECT count(*) FROM enrichedlab_agent.broker_task_scientific_history h
        WHERE h.tenant_id=p_tenant AND h.task_id=p_task)),
    'outputs',outputs,
    'review',jsonb_build_object('state',CASE WHEN tool_class IN ('agent_built','agent_session','configured','guarded_query') THEN 'unvalidated' ELSE 'not_reviewed' END),
    'gaps',gaps);
END $function$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (53) ON CONFLICT DO NOTHING;
COMMIT;
