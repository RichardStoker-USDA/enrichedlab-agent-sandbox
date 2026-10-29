-- The broker's history before the research record existed, imported into the record as `imported` records through the
-- one write path, evidence.append, so the chains, receipts, grants and immutability are the record's own.
--
-- What is imported: every row of the 55 append-guarded broker tables of schema 77, one record per row, in primary-key
-- order. The producer is the broker producer `broker:history`, which only the role that owns these functions (the
-- migration owner) can append as. evidence.import_begin creates it. evidence.import_seal does the import in one
-- transaction: share locks on the 55 tables and the relations scopes and times are read from, refusal while a research
-- run is queued, running or waiting, a full pass of evidence.import_history batches, every record checked against its
-- row as re-derived under those locks (evidence.import_verify), every stream it wrote checked (evidence.import_chains),
-- and only then the producer's grants removed and the producer disabled, so a failed check appends nothing and seals
-- nothing. evidence.import_history alone appends one
-- batch of one table; the dry run and the qualification use it.
--
-- What a record holds (event type `import.<table without the broker_ prefix>`, idempotency key
-- `import:<the same>:<SHA-256 of the key>`, actor role `broker`): the row's primary key, the SHA-256 of the whole row
-- (`row_sha256`, over the row's canonical JSON as these functions render it: time zone UTC, bytea as hex, shortest
-- exact floats), and its structural columns copied whole. A column is copied only when its type is a uuid, number,
-- boolean or time, or when it is text named as an identifier, enumeration, digest, reference or version and its value
-- is a single token of at most 256 bytes (letters, digits and `._:/+@=-`, no spaces); a text column named as a time is
-- copied only when it is a valid RFC 3339 time. Every other column, and any key value that fails the same test, is
-- recorded by its SHA-256, size, encoding and class and named in `normalised`; its bytes stay in the append-guarded
-- broker table, which the digests bind. Model prompts, answers, reasoning, generated code and model SQL are class
-- `model-text`: the record never carries their text.
-- A payload that would still exceed 64 KiB keeps the key, the row digest and the column digests, and is `truncated`.
--
-- Scope fails closed. Each scope identifier the row has (run, project, task, campaign, paper, note, lab) must be well
-- formed, must resolve, and all must agree on one lab and at most one project; the record goes to the most specific of
-- them (run, then project, then lab). A row with an identifier that does not resolve or that disagrees is not imported:
-- it is counted and reported as `unresolved`. Only a row with no scope identifier goes standalone, in the lab the
-- operator names. Source time: the row's first valid time column in a fixed order, else its parent's (`time: parent`),
-- else the tenant's creation time (`time: missing`).
--
-- Rerunning a batch appends nothing new: evidence.append answers an identical request with the original receipt. A row
-- that changed since its import is refused (evidence:idempotency_conflict).
--
-- Rollback: the functions may be dropped; records the import appended stay, as every record does.
BEGIN;

-- The role that owns these functions: the migration owner. Only a session as that role may import, and the history
-- producer appends as it.
CREATE OR REPLACE FUNCTION evidence.import_owner() RETURNS name LANGUAGE sql STABLE
SET search_path = pg_catalog AS $$
  SELECT r.rolname FROM pg_proc p JOIN pg_roles r ON r.oid = p.proowner
    WHERE p.pronamespace = 'evidence'::regnamespace AND p.proname = 'import_owner'
$$;

-- Which part of a column a record may carry, by table, column and type: `model-text`, `copy` (a candidate for copying;
-- its value must still pass evidence.import_token) or `content`.
CREATE OR REPLACE FUNCTION evidence.import_column_class(p_table text, p_column text, p_type text)
RETURNS text LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT CASE
    WHEN (p_table, p_column) IN (
        ('broker_research_contexts', 'content'), ('broker_campaign_planner_texts', 'content'),
        ('broker_research_turns', 'record'), ('broker_research_orientations', 'record'),
        ('broker_literature_reviews', 'record'), ('broker_task_generated_code', 'content'),
        ('broker_task_queries', 'proposed_sql'), ('broker_task_queries', 'admitted_sql'),
        ('broker_plan_events', 'title'), ('broker_plan_events', 'rationale'),
        ('provenance_events', 'restricted_content'))
      THEN 'model-text'
    WHEN p_type IN ('uuid', 'integer', 'bigint', 'smallint', 'boolean', 'numeric', 'real', 'double precision',
        'timestamp with time zone', 'date')
      THEN 'copy'
    WHEN p_type = 'text' AND p_column ~ ('^(tenant_id|group_key|kind|status|state|code|error_code|role|source|origin|'
        'decision|action|outcome_code|stream|disclosure_scope|data_class|model|engine|provider|parser|harness|'
        'visibility|evidence_role|subject|actor|executor|release|schema_name|guard|guard_policy|guard_outcome|'
        'lease_owner|input_class|custody_transaction|media_type|operation|feature|item_ref|item_version|namespace|'
        'doi|profile_key|tool_key|source_key|action_key|kind_id|checked_kind_id|recorded_by|'
        'service_version|server_version|status_before|status_after|task_status|session_id|run_id|occurred_at|'
        'source_event_id|parent_source_event_id|connection_ref)$|(_id|_ref|_key|_sha256|_digest|_version|_path)$|'
        '^(sha256|digest|hash|previous_hash|relative_path|path|table_path|block_path|image_digest|registry_digest)$')
      THEN 'copy'
    ELSE 'content'
  END
$$;

-- Whether a value may be copied whole: a typed scalar (uuid, number, boolean, time), or a text value that is one token
-- of at most 256 bytes over letters, digits and `._:/+@=-`.
CREATE OR REPLACE FUNCTION evidence.import_token(p_value jsonb, p_type text) RETURNS boolean LANGUAGE sql IMMUTABLE
SET search_path = pg_catalog AS $$
  SELECT CASE
    WHEN p_value IS NULL OR jsonb_typeof(p_value) = 'null' THEN true
    WHEN p_type IN ('uuid', 'integer', 'bigint', 'smallint', 'boolean', 'numeric', 'real', 'double precision',
        'timestamp with time zone', 'date') THEN jsonb_typeof(p_value) IN ('string', 'number', 'boolean')
    WHEN p_type = 'text' AND jsonb_typeof(p_value) = 'string' THEN
      (p_value #>> '{}') ~ '^[A-Za-z0-9][A-Za-z0-9._:/+@=-]{0,255}$'
    ELSE false
  END
$$;

-- One column's digest entry: {sha256, size, encoding, class}.
CREATE OR REPLACE FUNCTION evidence.import_digest(p_value jsonb, p_type text, p_class text) RETURNS jsonb
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog AS $$
DECLARE
  raw bytea;
  encoding text;
BEGIN
  IF p_type = 'bytea' THEN
    raw := decode(substr(p_value #>> '{}', 3), 'hex');
    encoding := 'bytes';
  ELSIF jsonb_typeof(p_value) = 'string' THEN
    raw := convert_to(p_value #>> '{}', 'UTF8');
    encoding := 'utf8-text';
  ELSE
    raw := convert_to(p_value::text, 'UTF8');
    encoding := 'jsonb-text';
  END IF;
  RETURN jsonb_build_object('sha256', encode(sha256(raw), 'hex'), 'size', octet_length(raw), 'encoding', encoding,
    'class', p_class);
END;
$$;

-- A row's key as the record names it: each key column's value when it may be copied, else its digest entry.
CREATE OR REPLACE FUNCTION evidence.import_key(p_row jsonb, p_key_columns text[], p_types jsonb) RETURNS jsonb
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, evidence AS $$
  SELECT jsonb_object_agg(c, CASE WHEN evidence.import_token(p_row->c, p_types->>c) THEN p_row->c
                                  ELSE evidence.import_digest(p_row->c, p_types->>c, 'content') END)
  FROM unnest(p_key_columns) AS c
$$;

CREATE OR REPLACE FUNCTION evidence.import_event_type(p_table text) RETURNS text LANGUAGE sql IMMUTABLE
SET search_path = pg_catalog AS $$
  SELECT 'import.' || regexp_replace(p_table, '^broker_', '')
$$;

CREATE OR REPLACE FUNCTION evidence.import_idempotency_key(p_table text, p_key jsonb) RETURNS text LANGUAGE sql
IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT 'import:' || regexp_replace(p_table, '^broker_', '') || ':' || encode(sha256(convert_to(p_key::text, 'UTF8')), 'hex')
$$;

-- The lab and scope a row belongs to, as (group_key, scope_kind, project_id, run_id, label). Label `unresolved` (and no
-- scope) when a scope identifier is malformed, does not resolve, or disagrees with another; every project the row
-- reaches must be in the same lab as the rest.
CREATE OR REPLACE FUNCTION evidence.import_scope(p_tenant text, p_table text, p_row jsonb, p_standalone_group text)
RETURNS TABLE (group_key text, scope_kind text, project_id uuid, run_id uuid, label text)
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
DECLARE
  uuid_pattern constant text := '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$';
  groups text[] := '{}';
  projects uuid[] := '{}';
  v_run uuid;
  v_group text;
  v_project uuid;
  present boolean := false;
  id text;
  name text;
BEGIN
  FOREACH name IN ARRAY ARRAY['run_id', 'project_id', 'task_id', 'campaign_id', 'paper_id', 'note_id'] LOOP
    IF NOT (p_row ? name) OR jsonb_typeof(p_row->name) = 'null' THEN CONTINUE; END IF;
    present := true;
    id := p_row->>name;
    IF id !~ uuid_pattern THEN
      RETURN QUERY SELECT NULL::text, NULL::text, NULL::uuid, NULL::uuid, 'unresolved'::text;
      RETURN;
    END IF;
    v_group := NULL;
    v_project := NULL;
    IF name = 'run_id' THEN
      SELECT p.group_key, r.project_id, r.run_id INTO v_group, v_project, v_run
        FROM enrichedlab_agent.broker_research_runs r
        JOIN enrichedlab_agent.broker_projects p ON p.tenant_id = r.tenant_id AND p.project_id = r.project_id
        WHERE r.tenant_id = p_tenant AND r.run_id = id::uuid;
    ELSIF name = 'project_id' THEN
      SELECT p.group_key, p.project_id INTO v_group, v_project FROM enrichedlab_agent.broker_projects p
        WHERE p.tenant_id = p_tenant AND p.project_id = id::uuid;
    ELSIF name = 'task_id' THEN
      SELECT t.group_key, pt.project_id INTO v_group, v_project FROM enrichedlab_agent.broker_tasks t
        LEFT JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = t.tenant_id AND pt.task_id = t.task_id
        WHERE t.tenant_id = p_tenant AND t.task_id = id::uuid;
    ELSIF name = 'campaign_id' THEN
      SELECT c.group_key INTO v_group FROM enrichedlab_agent.broker_campaigns c
        WHERE c.tenant_id = p_tenant AND c.campaign_id = id::uuid;
    ELSIF name = 'paper_id' THEN
      SELECT lp.group_key INTO v_group FROM enrichedlab_agent.broker_library_papers lp
        WHERE lp.tenant_id = p_tenant AND lp.paper_id = id::uuid;
    ELSIF name = 'note_id' THEN
      -- A note's revisions share its lab and project (enforced when a note is revised); all must agree.
      SELECT min(n.group_key), (array_agg(DISTINCT n.project_id))[1] INTO v_group, v_project
        FROM enrichedlab_agent.broker_lab_notes n WHERE n.tenant_id = p_tenant AND n.note_id = id::uuid
        HAVING count(DISTINCT n.group_key) = 1 AND count(DISTINCT n.project_id) <= 1;
    END IF;
    IF v_group IS NULL THEN
      RETURN QUERY SELECT NULL::text, NULL::text, NULL::uuid, NULL::uuid, 'unresolved'::text;
      RETURN;
    END IF;
    groups := groups || v_group;
    IF v_project IS NOT NULL THEN projects := projects || v_project; END IF;
  END LOOP;
  IF p_row ? 'group_key' AND jsonb_typeof(p_row->'group_key') <> 'null' THEN
    present := true;
    PERFORM 1 FROM enrichedlab_agent.broker_research_groups g
      WHERE g.tenant_id = p_tenant AND g.group_key = p_row->>'group_key';
    IF NOT FOUND THEN
      RETURN QUERY SELECT NULL::text, NULL::text, NULL::uuid, NULL::uuid, 'unresolved'::text;
      RETURN;
    END IF;
    groups := groups || (p_row->>'group_key');
  END IF;
  IF NOT present THEN
    RETURN QUERY SELECT p_standalone_group, 'standalone'::text, NULL::uuid, NULL::uuid, 'standalone'::text;
    RETURN;
  END IF;
  -- Every project the row reaches (its own, its run's, its task's or its note's) contributes that project's lab.
  FOREACH v_project IN ARRAY projects LOOP
    SELECT p.group_key INTO v_group FROM enrichedlab_agent.broker_projects p
      WHERE p.tenant_id = p_tenant AND p.project_id = v_project;
    IF NOT FOUND THEN
      RETURN QUERY SELECT NULL::text, NULL::text, NULL::uuid, NULL::uuid, 'unresolved'::text;
      RETURN;
    END IF;
    groups := groups || v_group;
  END LOOP;
  IF (SELECT count(DISTINCT g) FROM unnest(groups) g) <> 1 OR (SELECT count(DISTINCT pj) FROM unnest(projects) pj) > 1 THEN
    RETURN QUERY SELECT NULL::text, NULL::text, NULL::uuid, NULL::uuid, 'unresolved'::text;
    RETURN;
  END IF;
  IF v_run IS NOT NULL THEN
    RETURN QUERY SELECT groups[1], 'project'::text, projects[1], v_run, 'run'::text;
  ELSIF cardinality(projects) > 0 THEN
    RETURN QUERY SELECT groups[1], 'project'::text, projects[1], NULL::uuid, 'project'::text;
  ELSE
    RETURN QUERY SELECT groups[1], 'lab'::text, NULL::uuid, NULL::uuid, 'lab'::text;
  END IF;
END;
$$;

-- A text time: the value as timestamptz when it is RFC 3339 with a zone and a valid calendar time, else NULL.
CREATE OR REPLACE FUNCTION evidence.import_text_time(p_value text) RETURNS timestamptz LANGUAGE plpgsql IMMUTABLE
SET search_path = pg_catalog SET TimeZone = 'UTC' AS $$
BEGIN
  IF p_value IS NULL OR p_value !~ '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,9})?(Z|[+-]\d{2}:\d{2})$' THEN
    RETURN NULL;
  END IF;
  RETURN p_value::timestamptz;
EXCEPTION WHEN datetime_field_overflow OR invalid_datetime_format OR invalid_time_zone_displacement_value THEN
  RETURN NULL;
END;
$$;

-- The row's source time and how it was found: (source_time, 'row' | 'parent' | 'missing'). A time column counts only
-- when it is a timestamp column, or text that evidence.import_text_time accepts.
CREATE OR REPLACE FUNCTION evidence.import_source_time(p_tenant text, p_table text, p_row jsonb, p_types jsonb)
RETURNS TABLE (source_time timestamptz, label text)
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
DECLARE
  column_name text;
  found_time timestamptz;
BEGIN
  FOREACH column_name IN ARRAY ARRAY['occurred_at', 'recorded_at', 'created_at', 'started_at', 'called_at',
      'submitted_at', 'archived_at', 'acknowledged_at', 'closed_at', 'recovered_at', 'at'] LOOP
    IF NOT (p_row ? column_name) OR jsonb_typeof(p_row->column_name) <> 'string' THEN CONTINUE; END IF;
    IF p_types->>column_name = 'timestamp with time zone' THEN
      found_time := (p_row->>column_name)::timestamptz;
    ELSIF p_types->>column_name = 'text' THEN
      found_time := evidence.import_text_time(p_row->>column_name);
    END IF;
    IF found_time IS NOT NULL THEN
      RETURN QUERY SELECT found_time, 'row'::text;
      RETURN;
    END IF;
  END LOOP;
  IF p_table = 'broker_plan_events' THEN
    SELECT pr.recorded_at INTO found_time FROM enrichedlab_agent.broker_plan_revisions pr
      WHERE pr.tenant_id = p_tenant AND pr.project_id = (p_row->>'project_id')::uuid
        AND pr.revision = (p_row->>'revision')::bigint;
  ELSIF p_table = 'broker_project_tasks' THEN
    SELECT t.submitted_at INTO found_time FROM enrichedlab_agent.broker_tasks t
      WHERE t.tenant_id = p_tenant AND t.task_id = (p_row->>'task_id')::uuid;
  ELSIF p_row->>'run_id' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
    SELECT r.created_at INTO found_time FROM enrichedlab_agent.broker_research_runs r
      WHERE r.tenant_id = p_tenant AND r.run_id = (p_row->>'run_id')::uuid;
  END IF;
  IF found_time IS NOT NULL THEN
    RETURN QUERY SELECT found_time, 'parent'::text;
    RETURN;
  END IF;
  SELECT t.created_at INTO found_time FROM enrichedlab_agent.broker_tenants t WHERE t.tenant_id = p_tenant;
  RETURN QUERY SELECT found_time, 'missing'::text;
END;
$$;

-- One row's payload: canonical JSON text of {origin, table, key, row_sha256, fields, digests, normalised, time, scope}.
CREATE OR REPLACE FUNCTION evidence.import_payload(p_table text, p_row jsonb, p_key jsonb, p_types jsonb,
  p_time_label text, p_scope_label text)
RETURNS text LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, evidence AS $$
DECLARE
  column_name text;
  value jsonb;
  class text;
  fields jsonb := '{}'::jsonb;
  digests jsonb := '{}'::jsonb;
  normalised text[] := '{}';
  payload jsonb;
  out text;
BEGIN
  FOR column_name IN SELECT k FROM jsonb_object_keys(p_row) AS k ORDER BY k LOOP
    value := p_row->column_name;
    IF jsonb_typeof(value) = 'null' THEN
      fields := fields || jsonb_build_object(column_name, value);
      CONTINUE;
    END IF;
    class := evidence.import_column_class(p_table, column_name, p_types->>column_name);
    -- A text column named as a time is copied only when it is a valid time.
    IF class = 'copy' AND p_types->>column_name = 'text' AND column_name ~ '(^at|_at)$'
      AND evidence.import_text_time(value #>> '{}') IS NULL THEN
      class := 'content';
    END IF;
    IF class = 'copy' AND evidence.import_token(value, p_types->>column_name) THEN
      fields := fields || jsonb_build_object(column_name, value);
      CONTINUE;
    END IF;
    IF class = 'copy' THEN class := 'content'; END IF;
    digests := digests || jsonb_build_object(column_name, evidence.import_digest(value, p_types->>column_name, class));
    normalised := normalised || column_name;
  END LOOP;
  payload := jsonb_build_object('origin', 'imported', 'table', p_table, 'key', p_key,
    'row_sha256', encode(sha256(convert_to(p_row::text, 'UTF8')), 'hex'), 'fields', fields, 'digests', digests,
    'normalised', to_jsonb(normalised), 'time', p_time_label, 'scope', p_scope_label);
  out := payload::text;
  IF octet_length(out) > 65536 THEN
    out := (payload - 'fields' || jsonb_build_object('fields', NULL, 'truncated', true))::text;
  END IF;
  RETURN out;
END;
$$;

-- The tables an import covers: the 55 append-guarded broker tables of schema 77 (their rows are never updated or
-- deleted), with each one's primary key columns in key order and its column types. A table added later is imported
-- only once it is listed here.
CREATE OR REPLACE FUNCTION evidence.import_tables()
RETURNS TABLE (table_name text, key_columns text[], column_types jsonb)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
  SELECT c.relname::text,
    (SELECT array_agg(a.attname::text ORDER BY k.ord) FROM pg_index i
       CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord)
       JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
       WHERE i.indrelid = c.oid AND i.indisprimary),
    (SELECT jsonb_object_agg(a.attname, format_type(a.atttypid, NULL)) FROM pg_attribute a
       WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped)
  FROM (VALUES
    ('broker_admin_audit'), ('broker_analysis_receipts'), ('broker_analysis_requests'),
    ('broker_artifact_digests'), ('broker_artifact_kinds'), ('broker_artifact_recoveries'),
    ('broker_attempt_executions'), ('broker_attempt_inputs'), ('broker_attempt_outputs'),
    ('broker_campaign_events'), ('broker_campaign_planner_call_starts'), ('broker_campaign_planner_calls'),
    ('broker_campaign_planner_texts'), ('broker_dataset_receipts'), ('broker_dataset_requests'),
    ('broker_datasets'), ('broker_document_receipts'), ('broker_document_request_keys'),
    ('broker_document_requests'), ('broker_figure_manuscript_events'), ('broker_figure_receipts'),
    ('broker_figure_requests'), ('broker_inbox_acknowledgements'), ('broker_lab_note_events'),
    ('broker_lab_notes'), ('broker_library_label_events'), ('broker_library_paper_events'),
    ('broker_library_papers'), ('broker_literature_reviews'), ('broker_plan_archived_keys'),
    ('broker_plan_events'), ('broker_plan_pages'), ('broker_plan_revisions'), ('broker_profile_templates'),
    ('broker_project_audit'), ('broker_project_checkpoints'), ('broker_project_packages'), ('broker_project_tasks'),
    ('broker_query_receipts'), ('broker_research_contexts'), ('broker_research_orientations'),
    ('broker_research_run_events'), ('broker_research_turns'), ('broker_reviewed_queries'),
    ('broker_sql_source_cards'), ('broker_sql_sources'), ('broker_task_events'), ('broker_task_generated_code'),
    ('broker_task_methods'), ('broker_task_origins'), ('broker_task_output_closures'), ('broker_task_output_pieces'),
    ('broker_task_queries'), ('broker_task_reviews'), ('provenance_events')) AS listed(name)
  JOIN pg_class c ON c.relname = listed.name AND c.relkind IN ('r', 'p')
  JOIN pg_namespace n ON n.oid = c.relnamespace AND n.nspname = 'enrichedlab_agent'
  ORDER BY 1
$$;

-- Creates (or re-opens, while still enabled) the history producer for one tenant. Refuses after the import is sealed.
CREATE OR REPLACE FUNCTION evidence.import_begin(p_tenant text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  held evidence.producers%ROWTYPE;
BEGIN
  IF session_user <> evidence.import_owner() THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:import_not_migration_role';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO held FROM evidence.producers WHERE tenant_id = p_tenant AND identity = 'broker:history';
  IF FOUND THEN
    IF NOT held.enabled THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:import_finished';
    END IF;
    RETURN held.producer_id;
  END IF;
  INSERT INTO evidence.producers (tenant_id, identity, kind, database_role)
    VALUES (p_tenant, 'broker:history', 'broker', evidence.import_owner())
    RETURNING producer_id INTO held.producer_id;
  RETURN held.producer_id;
END;
$$;

-- Appends one batch of one table: up to p_limit rows from p_offset in primary-key order. Returns the counts and the
-- key digest of every row not imported because its scope did not resolve.
CREATE OR REPLACE FUNCTION evidence.import_history(p_tenant text, p_table text, p_standalone_group text,
  p_offset bigint, p_limit integer)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' SET TimeZone = 'UTC' SET bytea_output = 'hex' SET extra_float_digits = 1 AS $$
DECLARE
  meta record;
  producer evidence.producers%ROWTYPE;
  row_value jsonb;
  key jsonb;
  scope record;
  timing record;
  payload text;
  receipt jsonb;
  event_types text[];
  appended integer := 0;
  replayed integer := 0;
  truncated integer := 0;
  digested integer := 0;
  unresolved integer := 0;
  unresolved_keys text[] := '{}';
  seen integer := 0;
BEGIN
  IF session_user <> evidence.import_owner() THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:import_not_migration_role';
  END IF;
  IF p_limit NOT BETWEEN 1 AND 5000 OR p_offset < 0 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:import_batch_invalid';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO producer FROM evidence.producers
    WHERE tenant_id = p_tenant AND identity = 'broker:history' AND enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:import_not_begun'; END IF;
  SELECT * INTO meta FROM evidence.import_tables() it WHERE it.table_name = p_table;
  IF NOT FOUND OR meta.key_columns IS NULL OR NOT ('tenant_id' = ANY (meta.key_columns)) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:import_table_unknown';
  END IF;
  SELECT array_agg(evidence.import_event_type(it.table_name) ORDER BY it.table_name) INTO event_types
    FROM evidence.import_tables() it;

  FOR row_value IN EXECUTE format('SELECT to_jsonb(t) FROM enrichedlab_agent.%I t WHERE t.tenant_id = $1 ORDER BY %s '
      'OFFSET $2 LIMIT $3', p_table, (SELECT string_agg(format('t.%I', c), ', ') FROM unnest(meta.key_columns) AS c))
    USING p_tenant, p_offset, p_limit LOOP
    seen := seen + 1;
    key := evidence.import_key(row_value, meta.key_columns, meta.column_types);
    SELECT * INTO scope FROM evidence.import_scope(p_tenant, p_table, row_value, p_standalone_group);
    IF scope.label = 'unresolved' THEN
      unresolved := unresolved + 1;
      unresolved_keys := unresolved_keys || encode(sha256(convert_to(key::text, 'UTF8')), 'hex');
      CONTINUE;
    END IF;
    SELECT * INTO timing FROM evidence.import_source_time(p_tenant, p_table, row_value, meta.column_types);
    payload := evidence.import_payload(p_table, row_value, key, meta.column_types, timing.label, scope.label);
    IF payload::jsonb ? 'truncated' THEN truncated := truncated + 1; END IF;
    IF jsonb_array_length(payload::jsonb->'normalised') > 0 THEN digested := digested + 1; END IF;
    INSERT INTO evidence.producer_grants (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, all_runs,
        event_types, actor_role)
      VALUES (p_tenant, producer.producer_id, scope.group_key, scope.scope_kind, scope.project_id, NULL,
        scope.scope_kind = 'project', event_types, 'broker')
      ON CONFLICT (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, all_runs) DO NOTHING;
    receipt := evidence.append(p_tenant, 'broker:history', scope.group_key, scope.scope_kind, scope.project_id,
      scope.run_id, evidence.import_event_type(p_table), evidence.import_idempotency_key(p_table, key),
      timing.source_time, payload, encode(sha256(convert_to(payload, 'UTF8')), 'hex'));
    IF (receipt->>'replayed')::boolean THEN replayed := replayed + 1; ELSE appended := appended + 1; END IF;
  END LOOP;
  RETURN jsonb_build_object('table', p_table, 'offset', p_offset, 'rows', seen, 'appended', appended,
    'replayed', replayed, 'digested', digested, 'truncated', truncated, 'unresolved', unresolved,
    'unresolved_keys', to_jsonb(unresolved_keys));
END;
$$;

-- Every row of every table against the record, for one tenant, by re-deriving each row's record: per table, the rows,
-- the rows whose record is exactly what the row derives now (payload, lab, scope kind, project, run, source time and
-- event type), rows whose record differs, rows not imported because their scope is unresolved, rows missing a record
-- otherwise, records whose key matches no row, and duplicate records. p_standalone_group is the lab given for rows
-- with no scope identifier.
CREATE OR REPLACE FUNCTION evidence.import_verify(p_tenant text, p_standalone_group text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' SET TimeZone = 'UTC' SET bytea_output = 'hex' SET extra_float_digits = 1 AS $$
DECLARE
  meta record;
  producer uuid;
  result jsonb := '{}'::jsonb;
  row_value jsonb;
  key jsonb;
  ik text;
  scope record;
  timing record;
  rec record;
  n_rows bigint;
  n_matched bigint;
  n_differs bigint;
  n_unresolved bigint;
  n_missing bigint;
  n_orphans bigint;
  n_duplicates bigint;
BEGIN
  IF session_user <> evidence.import_owner() THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:import_not_migration_role';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT producer_id INTO producer FROM evidence.producers WHERE tenant_id = p_tenant AND identity = 'broker:history';
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:import_not_begun'; END IF;
  FOR meta IN SELECT * FROM evidence.import_tables() LOOP
    n_rows := 0; n_matched := 0; n_differs := 0; n_unresolved := 0; n_missing := 0;
    FOR row_value IN EXECUTE format('SELECT to_jsonb(t) FROM enrichedlab_agent.%I t WHERE t.tenant_id = $1',
        meta.table_name) USING p_tenant LOOP
      n_rows := n_rows + 1;
      key := evidence.import_key(row_value, meta.key_columns, meta.column_types);
      ik := evidence.import_idempotency_key(meta.table_name, key);
      SELECT * INTO scope FROM evidence.import_scope(p_tenant, meta.table_name, row_value, p_standalone_group);
      SELECT rr.payload, rr.group_key, rr.scope_kind, rr.project_id, rr.run_id, rr.source_time, rr.event_type INTO rec
        FROM evidence.receipts rc JOIN evidence.records rr ON rr.tenant_id = rc.tenant_id
          AND rr.stream_id = rc.stream_id AND rr.sequence = rc.sequence
        WHERE rc.tenant_id = p_tenant AND rc.producer_id = producer AND rc.idempotency_key = ik;
      IF NOT FOUND THEN
        IF scope.label = 'unresolved' THEN n_unresolved := n_unresolved + 1; ELSE n_missing := n_missing + 1; END IF;
        CONTINUE;
      END IF;
      IF scope.label = 'unresolved' THEN
        n_differs := n_differs + 1;
        CONTINUE;
      END IF;
      SELECT * INTO timing FROM evidence.import_source_time(p_tenant, meta.table_name, row_value, meta.column_types);
      IF rec.payload = evidence.import_payload(meta.table_name, row_value, key, meta.column_types, timing.label,
           scope.label)
        AND rec.group_key = scope.group_key AND rec.scope_kind = scope.scope_kind
        AND rec.project_id IS NOT DISTINCT FROM scope.project_id AND rec.run_id IS NOT DISTINCT FROM scope.run_id
        AND rec.source_time = timing.source_time AND rec.event_type = evidence.import_event_type(meta.table_name) THEN
        n_matched := n_matched + 1;
      ELSE
        n_differs := n_differs + 1;
      END IF;
    END LOOP;
    EXECUTE format($q$
      WITH keyed AS (
        SELECT evidence.import_idempotency_key(%L, evidence.import_key(to_jsonb(t), $2, $3)) AS ik
        FROM enrichedlab_agent.%I t WHERE t.tenant_id = $1
      ), rec AS (
        SELECT rc.idempotency_key AS ik FROM evidence.receipts rc JOIN evidence.records rr ON rr.tenant_id = rc.tenant_id
          AND rr.stream_id = rc.stream_id AND rr.sequence = rc.sequence
        WHERE rc.tenant_id = $1 AND rc.producer_id = $4 AND rr.event_type = %L
      )
      SELECT (SELECT count(*) FROM rec WHERE NOT EXISTS (SELECT 1 FROM keyed k WHERE k.ik = rec.ik)),
             (SELECT count(*) - count(DISTINCT ik) FROM rec)
    $q$, meta.table_name, meta.table_name, evidence.import_event_type(meta.table_name))
    INTO n_orphans, n_duplicates USING p_tenant, meta.key_columns, meta.column_types, producer;
    result := result || jsonb_build_object(meta.table_name, jsonb_build_object('rows', n_rows, 'matched', n_matched,
      'differs', n_differs, 'unresolved', n_unresolved, 'missing', n_missing, 'orphan_records', n_orphans,
      'duplicate_records', n_duplicates));
  END LOOP;
  RETURN result;
END;
$$;

-- Every stream the history producer wrote in one tenant, checked as evidence.verify_stream checks one (sequence, links,
-- record hashes, payload digests, head), up to each stream's head as read: {streams, broken}.
CREATE OR REPLACE FUNCTION evidence.import_chains(p_tenant text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  head evidence.streams%ROWTYPE;
  r record;
  expected bigint;
  previous text;
  streams integer := 0;
  broken integer := 0;
  bad boolean;
BEGIN
  IF session_user <> evidence.import_owner() THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:import_not_migration_role';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  FOR head IN SELECT st.* FROM evidence.streams st WHERE st.tenant_id = p_tenant AND EXISTS (
      SELECT 1 FROM evidence.records rr JOIN evidence.producers p ON p.tenant_id = rr.tenant_id
        AND p.producer_id = rr.producer_id
      WHERE rr.tenant_id = st.tenant_id AND rr.stream_id = st.stream_id AND p.identity = 'broker:history') LOOP
    streams := streams + 1;
    expected := 1;
    previous := repeat('0', 64);
    bad := false;
    FOR r IN SELECT rec.*, p.identity FROM evidence.records rec
        JOIN evidence.producers p ON p.tenant_id = rec.tenant_id AND p.producer_id = rec.producer_id
        WHERE rec.tenant_id = p_tenant AND rec.stream_id = head.stream_id AND rec.sequence <= head.head_sequence
        ORDER BY rec.sequence LOOP
      IF r.sequence <> expected OR r.previous_hash <> previous
        OR encode(sha256(convert_to(evidence.record_material_v1(r.tenant_id, r.stream_id, r.sequence, r.record_id,
             r.group_key, r.scope_kind, r.project_id, r.run_id, r.identity, r.event_type, r.actor_role, r.source_time,
             r.ingested_at, r.payload_sha256, r.previous_hash), 'UTF8')), 'hex') <> r.record_hash
        OR encode(sha256(convert_to(r.payload, 'UTF8')), 'hex') <> r.payload_sha256 THEN
        bad := true;
        EXIT;
      END IF;
      previous := r.record_hash;
      expected := expected + 1;
    END LOOP;
    IF bad OR expected - 1 <> head.head_sequence OR previous <> head.head_hash THEN broken := broken + 1; END IF;
  END LOOP;
  RETURN jsonb_build_object('streams', streams, 'broken', broken);
END;
$$;

-- Imports and seals one tenant's history in one transaction: share locks on the 55 tables and on every relation a
-- scope or source time is read from (writers wait until the transaction ends), no research run queued, running or
-- waiting, a full pass over every table, every record verified against its row as re-derived now, every stream it
-- wrote checked, and only then the producer's grants removed and the producer disabled. Raises, and so leaves nothing appended and nothing sealed, when
-- any check fails. Returns the verification and the counts by event type.
CREATE OR REPLACE FUNCTION evidence.import_seal(p_tenant text, p_standalone_group text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' SET TimeZone = 'UTC' SET bytea_output = 'hex' SET extra_float_digits = 1 AS $$
DECLARE
  meta record;
  r jsonb;
  offset_at bigint;
  appended integer := 0;
  verified jsonb;
  failing text;
  producer evidence.producers%ROWTYPE;
  removed integer;
  counts jsonb;
  unresolved_keys jsonb := '{}'::jsonb;
  chains jsonb;
BEGIN
  IF session_user <> evidence.import_owner() THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:import_not_migration_role';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  FOR meta IN SELECT * FROM evidence.import_tables() LOOP
    EXECUTE format('LOCK TABLE enrichedlab_agent.%I IN SHARE MODE', meta.table_name);
  END LOOP;
  LOCK TABLE enrichedlab_agent.broker_research_runs, enrichedlab_agent.broker_projects, enrichedlab_agent.broker_tasks,
    enrichedlab_agent.broker_campaigns, enrichedlab_agent.broker_research_groups, enrichedlab_agent.broker_tenants
    IN SHARE MODE;
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_research_runs
             WHERE tenant_id = p_tenant AND status IN ('queued', 'running', 'waiting')) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:import_run_active';
  END IF;
  FOR meta IN SELECT * FROM evidence.import_tables() LOOP
    offset_at := 0;
    LOOP
      r := evidence.import_history(p_tenant, meta.table_name, p_standalone_group, offset_at, 1000);
      appended := appended + (r->>'appended')::int;
      IF jsonb_array_length(r->'unresolved_keys') > 0 THEN
        unresolved_keys := unresolved_keys || jsonb_build_object(meta.table_name,
          coalesce(unresolved_keys->meta.table_name, '[]'::jsonb) || (r->'unresolved_keys'));
      END IF;
      EXIT WHEN (r->>'rows')::int < 1000;
      offset_at := offset_at + 1000;
    END LOOP;
  END LOOP;
  verified := evidence.import_verify(p_tenant, p_standalone_group);
  SELECT string_agg(key, ', ') INTO failing FROM jsonb_each(verified) AS v(key, value)
    WHERE (value->>'missing')::int <> 0 OR (value->>'differs')::int <> 0
       OR (value->>'orphan_records')::int <> 0 OR (value->>'duplicate_records')::int <> 0
       OR (value->>'matched')::int + (value->>'unresolved')::int <> (value->>'rows')::int;
  -- The receipt names every unresolved row: one key digest per unresolved row of each table.
  SELECT coalesce(string_agg(key, ', ') FILTER (WHERE (value->>'unresolved')::int
           <> coalesce(jsonb_array_length(unresolved_keys->key), 0)), failing) INTO failing
    FROM jsonb_each(verified) AS v(key, value);
  IF failing IS NOT NULL THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:import_verification_failed', DETAIL = failing;
  END IF;
  chains := evidence.import_chains(p_tenant);
  IF (chains->>'broken')::int <> 0 THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:import_chain_broken', DETAIL = chains::text;
  END IF;
  SELECT * INTO producer FROM evidence.producers WHERE tenant_id = p_tenant AND identity = 'broker:history' AND enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:import_not_begun'; END IF;
  DELETE FROM evidence.producer_grants WHERE tenant_id = p_tenant AND producer_id = producer.producer_id;
  GET DIAGNOSTICS removed = ROW_COUNT;
  UPDATE evidence.producers SET enabled = false WHERE tenant_id = p_tenant AND producer_id = producer.producer_id;
  SELECT coalesce(jsonb_object_agg(event_type, n), '{}') INTO counts FROM (
    SELECT rr.event_type, count(*) AS n FROM evidence.records rr
      WHERE rr.tenant_id = p_tenant AND rr.producer_id = producer.producer_id GROUP BY rr.event_type) x;
  RETURN jsonb_build_object('appended', appended, 'grants_removed', removed, 'verified', verified,
    'records', counts, 'unresolved_keys', unresolved_keys, 'chains', chains);
END;
$$;

REVOKE ALL ON FUNCTION evidence.import_owner() FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_column_class(text, text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_token(jsonb, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_digest(jsonb, text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_key(jsonb, text[], jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_event_type(text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_idempotency_key(text, jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_scope(text, text, jsonb, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_source_time(text, text, jsonb, jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_payload(text, jsonb, jsonb, jsonb, text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_tables() FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_begin(text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_history(text, text, text, bigint, integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_verify(text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_text_time(text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_seal(text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.import_chains(text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (78) ON CONFLICT DO NOTHING;
COMMIT;
