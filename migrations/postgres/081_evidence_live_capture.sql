-- The broker's rows into the research record as they are written. Migration 078 imported every append-guarded row that
-- existed when it sealed; this migration records each row written after that.
--
-- Capture is a transactional outbox. An ordinary row trigger on each captured table (the backfill's 55 and migration
-- 080's three tool group tables) writes the row, as written, to evidence.capture_log in the writing transaction, so the
-- log entry commits exactly when the row does and a rolled-back row leaves none. Three append-guarded tables permit one
-- update each (a tool group grant's revocation, a dataset's intake task, a reviewed query's retirement); each is logged
-- as its own change. The broker transaction does nothing else: it takes no lock on a stream, a producer or a scope, so
-- capture cannot change a broker transaction's lock order or make it wait.
--
-- The recorder, evidence.capture_record, runs in its own transaction (the evidence append service calls it on a
-- timer). One recorder runs at a time; it takes committed log entries in order, derives each record with 078's
-- functions (key, scope from the committed rows, source time, payload: model text by digest only, other text digested
-- unless it is one safe token, a 64 KiB bound) with origin `live` and change `insert`, `revoke`, `intake` or `retire`,
-- prepares each scope (the capture producer, its grant and the stream) before locking any stream, appends in one global
-- stream order, and deletes the entries it recorded in the same transaction. Event type `broker.<table without
-- broker_>` for an insert and `broker.tool_group_grant_revoked`, `broker.dataset_intake_bound` or
-- `broker.reviewed_query_retired` for an update; idempotency key `live:<table>:<SHA-256 of the key>` for an insert and
-- `<change>:<table>:<...>` for an update. A row with no time of its own takes its writing transaction's start time,
-- labelled `captured`.
--
-- Producer: one capture producer per tenant, `broker:capture`, that no session can name: evidence.append refuses it,
-- and it appends only through evidence.append_as, which runtime roles cannot execute, called by these functions
-- (owned by the migration owner). Capture therefore adds records the broker's own functions wrote and gives the API,
-- worker, coordinator, director and custodian no append authority they did not have. The log has no grants: no
-- runtime session can read, change or remove an entry.
--
-- A row that cannot be placed (its scope identifiers do not resolve or disagree, its tenant or lab is disabled, its time
-- is out of the record's range) is recorded as an entry in evidence.capture_gaps: owner-only and immutable, holding the
-- table, key and row digests, the intended event and idempotency key, the reason, the scope when one resolved, and the
-- role that wrote the row; never the row's values.
--
-- Opt-in per tenant: evidence.capture_tenants, written only by the migration owner (evidence.capture_enable). Every
-- captured row is logged whatever its tenant; the recorder records the entries of opted-in tenants and discards the
-- rest, whose rows stay in the broker's tables for a later catch-up. Every row written after this migration commits is
-- logged (CREATE TRIGGER waits for writers holding the tables, and later statements see the trigger), and catch-up
-- records the rows from the backfill's seal to here, so no window needs a drain. Cutover: enable, run the recorder
-- until the log holds none of the tenant's entries, one evidence.capture_catchup (rows written between the backfill's
-- seal and the opt-in, with origin `caught_up`), then evidence.capture_missing, which counts every row with neither a
-- record, a gap entry nor a pending log entry and must report none; it stays the standing check that capture is
-- complete.
--
-- Rollback: delete the tenant's capture_tenants row (its entries are then discarded; records stay, as every record
-- does). The triggers may then be dropped; evidence.append keeps its contract either way.
BEGIN;

-- evidence.append in two parts. evidence.append_as appends for a producer named by id, after the argument, tenant,
-- time, scope, grant and payload checks of migration 079, unchanged; only functions of the migration owner execute
-- it. evidence.append authenticates the session as the producer or a bound adapter, as before, and calls it.
CREATE OR REPLACE FUNCTION evidence.append_as(
  p_tenant text, p_producer_id uuid, p_group text, p_scope_kind text, p_project uuid, p_run uuid, p_event_type text,
  p_idempotency_key text, p_source_time timestamptz, p_payload text, p_payload_sha256 text)
RETURNS jsonb LANGUAGE plpgsql SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  producer evidence.producers%ROWTYPE;
  stream evidence.streams%ROWTYPE;
  held evidence.receipts%ROWTYPE;
  request text;
  next_sequence bigint;
  new_record uuid := uuidv7();
  now_at timestamptz := clock_timestamp();
  material text;
  new_hash text;
  granted_role text;
  shape evidence.event_schemas%ROWTYPE;
  schema_ref text;
BEGIN
  IF p_tenant IS NULL OR p_producer_id IS NULL OR p_group IS NULL OR p_scope_kind IS NULL OR p_event_type IS NULL
    OR p_idempotency_key IS NULL OR p_source_time IS NULL OR p_payload IS NULL OR p_payload_sha256 IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:argument_missing';
  END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_tenants t WHERE t.tenant_id = p_tenant AND t.enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:tenant_disabled'; END IF;
  -- Canonical UTC text is unambiguous only for these years; it also keeps a skewed clock from writing the future.
  IF p_source_time < '1970-01-01T00:00:00Z'::timestamptz OR p_source_time > clock_timestamp() + interval '1 day' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:source_time_invalid';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);

  -- Migration 081: the producer is named by id and was authenticated by the caller: evidence.append for every session,
  -- or the migration owner's capture path for the capture producer.
  SELECT * INTO producer FROM evidence.producers
    WHERE tenant_id = p_tenant AND producer_id = p_producer_id AND enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_unknown'; END IF;
  IF p_event_type ~ '^(review|validation|acceptance)(\.|$)' AND producer.kind <> 'broker' THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:event_type_reserved';
  END IF;

  -- The scope must be real and consistent: an enabled lab, a project of that lab, a run of that project.
  IF p_scope_kind NOT IN ('project', 'lab', 'standalone') OR (p_scope_kind = 'project') <> (p_project IS NOT NULL)
    OR (p_run IS NOT NULL AND p_scope_kind <> 'project') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:scope_invalid';
  END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_research_groups g
    WHERE g.tenant_id = p_tenant AND g.group_key = p_group AND g.enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:scope_invalid'; END IF;
  IF p_project IS NOT NULL THEN
    PERFORM 1 FROM enrichedlab_agent.broker_projects pr
      WHERE pr.tenant_id = p_tenant AND pr.project_id = p_project AND pr.group_key = p_group;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:scope_invalid'; END IF;
  END IF;
  IF p_run IS NOT NULL THEN
    PERFORM 1 FROM enrichedlab_agent.broker_research_runs r
      WHERE r.tenant_id = p_tenant AND r.run_id = p_run AND r.project_id = p_project;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:scope_invalid'; END IF;
  END IF;

  -- The producer's grant names this lab, scope kind, project and run, and lists the event type. A run's own grant
  -- decides the actor role before a project-wide one.
  SELECT pg.actor_role INTO granted_role FROM evidence.producer_grants pg
    WHERE pg.tenant_id = p_tenant AND pg.producer_id = producer.producer_id AND pg.group_key = p_group
      AND pg.scope_kind = p_scope_kind AND pg.project_id IS NOT DISTINCT FROM p_project
      AND (pg.run_id = p_run OR (pg.run_id IS NULL AND (p_run IS NULL OR pg.all_runs)))
      AND p_event_type = ANY (pg.event_types)
    ORDER BY pg.run_id IS NULL, pg.grant_id
    LIMIT 1;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:grant_missing'; END IF;

  IF octet_length(p_payload) NOT BETWEEN 2 AND 65536 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:payload_size';
  END IF;
  IF encode(sha256(convert_to(p_payload, 'UTF8')), 'hex') IS DISTINCT FROM p_payload_sha256 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:payload_digest_mismatch';
  END IF;
  BEGIN
    IF jsonb_typeof(p_payload::jsonb) IS DISTINCT FROM 'object' THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:payload_not_object';
    END IF;
  EXCEPTION WHEN invalid_text_representation THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:payload_not_json';
  END;
  -- Migration 079: a payload names each member once, so the record keeps only text the checks below saw.
  IF evidence.json_duplicate_members(p_payload::json) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:payload_duplicate_member';
  END IF;

  -- The stream's head row serialises appends to it.
  INSERT INTO evidence.streams (tenant_id, group_key, scope_kind, project_id, run_id)
    VALUES (p_tenant, p_group, p_scope_kind, p_project, p_run) ON CONFLICT DO NOTHING;
  SELECT * INTO stream FROM evidence.streams s
    WHERE s.tenant_id = p_tenant AND s.group_key = p_group AND s.scope_kind = p_scope_kind
      AND s.project_id IS NOT DISTINCT FROM p_project AND s.run_id IS NOT DISTINCT FROM p_run
    FOR UPDATE;

  request := encode(sha256(convert_to(concat_ws(E'\n', 'enrichedlab.evidence.request.v1', p_tenant, producer.identity,
    p_group, p_scope_kind, coalesce(p_project::text, '-'), coalesce(p_run::text, '-'), p_event_type,
    evidence.utc_text(p_source_time), p_payload_sha256), 'UTF8')), 'hex');
  SELECT * INTO held FROM evidence.receipts rc
    WHERE rc.tenant_id = p_tenant AND rc.producer_id = producer.producer_id AND rc.stream_id = stream.stream_id
      AND rc.idempotency_key = p_idempotency_key;
  IF FOUND THEN
    IF held.request_sha256 <> request THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'evidence:idempotency_conflict';
    END IF;
    RETURN jsonb_build_object('record_id', held.record_id, 'stream_id', held.stream_id, 'sequence', held.sequence,
      'record_hash', held.record_hash, 'ingested_at', evidence.utc_text(held.ingested_at), 'replayed', true);
  END IF;

  -- Migration 079: a new record from an untrusted producer matches its event type's newest registered schema (an
  -- identical retry was answered above, so a later schema never refuses a record already written).
  IF producer.kind <> 'broker' THEN
    SELECT * INTO shape FROM evidence.event_schemas s WHERE s.tenant_id = p_tenant AND s.event_type = p_event_type
      ORDER BY s.version DESC LIMIT 1;
    IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:event_schema_missing'; END IF;
    IF NOT evidence.shape_ok(p_payload::jsonb - 'lineage', shape.schema) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:payload_not_metadata';
    END IF;
    schema_ref := p_event_type || '@' || shape.version || ':' || shape.schema_sha256;
  END IF;

  next_sequence := stream.head_sequence + 1;
  material := evidence.record_material_v1(p_tenant, stream.stream_id, next_sequence, new_record, p_group, p_scope_kind,
    p_project, p_run, producer.identity, p_event_type, granted_role, p_source_time, now_at, p_payload_sha256, stream.head_hash);
  new_hash := encode(sha256(convert_to(material, 'UTF8')), 'hex');

  INSERT INTO evidence.receipts (tenant_id, stream_id, sequence, record_id, producer_id, idempotency_key, request_sha256,
      record_hash, ingested_at, payload_schema)
    VALUES (p_tenant, stream.stream_id, next_sequence, new_record, producer.producer_id, p_idempotency_key, request,
      new_hash, now_at, schema_ref);
  INSERT INTO evidence.records (tenant_id, stream_id, sequence, ingested_at, record_id, group_key, scope_kind,
      project_id, run_id, producer_id, event_type, actor_role, source_time, payload, payload_sha256, previous_hash,
      record_hash, hash_version)
    VALUES (p_tenant, stream.stream_id, next_sequence, now_at, new_record, p_group, p_scope_kind, p_project, p_run,
      producer.producer_id, p_event_type, granted_role, p_source_time, p_payload, p_payload_sha256, stream.head_hash,
      new_hash, 1);
  UPDATE evidence.streams SET head_sequence = next_sequence, head_hash = new_hash, updated_at = now_at
    WHERE tenant_id = p_tenant AND stream_id = stream.stream_id;

  -- Migration 076: the payload's lineage block becomes registry rows and edges in this transaction.
  IF p_payload::jsonb ? 'lineage' THEN
    PERFORM evidence.write_lineage(p_tenant, producer.producer_id, producer.kind, stream.stream_id, p_group, p_project,
      new_record, now_at, p_source_time, p_payload::jsonb -> 'lineage');
  END IF;

  -- Migration 079: a project's acceptance sets its records class and asks for every reference's retention.
  IF p_event_type = 'acceptance.project' THEN
    IF producer.kind <> 'broker' OR p_project IS NULL OR p_run IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:event_type_reserved';
    END IF;
    PERFORM evidence.custody_accept_project(p_tenant, p_group, p_project, new_record, p_payload::jsonb ->> 'records_class');
  END IF;

  RETURN jsonb_build_object('record_id', new_record, 'stream_id', stream.stream_id, 'sequence', next_sequence,
    'record_hash', new_hash, 'ingested_at', evidence.utc_text(now_at), 'replayed', false);
END;
$$;

CREATE OR REPLACE FUNCTION evidence.append(
  p_tenant text, p_producer text, p_group text, p_scope_kind text, p_project uuid, p_run uuid, p_event_type text,
  p_idempotency_key text, p_source_time timestamptz, p_payload text, p_payload_sha256 text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  producer evidence.producers%ROWTYPE;
  role_name text;
BEGIN
  IF p_tenant IS NULL OR p_producer IS NULL OR p_group IS NULL OR p_scope_kind IS NULL OR p_event_type IS NULL
    OR p_idempotency_key IS NULL OR p_source_time IS NULL OR p_payload IS NULL OR p_payload_sha256 IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:argument_missing';
  END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_tenants t WHERE t.tenant_id = p_tenant AND t.enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:tenant_disabled'; END IF;
  -- Canonical UTC text is unambiguous only for these years; it also keeps a skewed clock from writing the future.
  IF p_source_time < '1970-01-01T00:00:00Z'::timestamptz OR p_source_time > clock_timestamp() + interval '1 day' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:source_time_invalid';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);

  -- The caller is the producer, or an adapter bound to it.
  SELECT * INTO producer FROM evidence.producers WHERE tenant_id = p_tenant AND identity = p_producer AND enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_unknown'; END IF;
  -- Migration 081: the capture producer records broker rows only through the capture path; no session may name it.
  IF producer.identity = 'broker:capture' THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_not_caller';
  END IF;
  role_name := session_user;
  IF producer.database_role IS NOT NULL THEN
    IF producer.database_role <> role_name THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_not_caller';
    END IF;
  ELSIF NOT EXISTS (SELECT 1 FROM evidence.adapter_bindings b WHERE b.tenant_id = p_tenant
      AND b.producer_id = producer.producer_id AND b.role_name = session_user) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_not_caller';
  END IF;
  RETURN evidence.append_as(p_tenant, producer.producer_id, p_group, p_scope_kind, p_project, p_run, p_event_type,
    p_idempotency_key, p_source_time, p_payload, p_payload_sha256);
END;
$$;

REVOKE ALL ON FUNCTION evidence.append_as(text, uuid, text, text, uuid, uuid, text, text, timestamptz, text, text)
  FROM PUBLIC;

-- Catch-up and the completeness check look a row's record up by its idempotency key in its tenant.
CREATE INDEX IF NOT EXISTS receipts_tenant_key ON evidence.receipts (tenant_id, idempotency_key);

CREATE TABLE IF NOT EXISTS evidence.capture_tenants (
  tenant_id text PRIMARY KEY REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  standalone_group text NOT NULL,
  enabled_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
ALTER TABLE evidence.capture_tenants ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON evidence.capture_tenants FROM PUBLIC;

-- Objects of earlier drafts of this migration (capture at commit through a queue, flush markers, scope and statement
-- triggers, a fence and a drain), which the log replaces. A database that ran a draft loses them here.
DO $$ DECLARE r record; BEGIN
  FOR r IN SELECT t.tgname, c.relname FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
      WHERE c.relnamespace = 'enrichedlab_agent'::regnamespace AND NOT t.tgisinternal
        AND t.tgname LIKE 'evidence\_capture%' AND t.tgname NOT IN ('evidence_capture', 'evidence_capture_update') LOOP
    EXECUTE format('DROP TRIGGER %I ON enrichedlab_agent.%I', r.tgname, r.relname);
  END LOOP;
  FOR r IN SELECT p.oid::regprocedure AS f FROM pg_proc p WHERE p.pronamespace = 'evidence'::regnamespace
      AND p.proname IN ('capture_append', 'capture_drained', 'capture_fence', 'capture_flush', 'capture_gap',
        'capture_lock', 'capture_prepared', 'capture_queue', 'capture_row', 'capture_scope', 'capture_statement_end',
        'capture_prepare_tenant') LOOP
    EXECUTE format('DROP FUNCTION %s', r.f);
  END LOOP;
END $$;
DROP TABLE IF EXISTS evidence.capture_queue, evidence.capture_flushed;
ALTER TABLE evidence.capture_tenants DROP COLUMN IF EXISTS enabled_xact, DROP COLUMN IF EXISTS fenced_at;

-- The recorder's lock: one owner-only row. Locking a row needs UPDATE on its table, which no runtime role has, so no
-- other session can hold the recorder off (any session can take an advisory lock key).
CREATE TABLE IF NOT EXISTS evidence.capture_recorder (one boolean PRIMARY KEY DEFAULT true CHECK (one));
INSERT INTO evidence.capture_recorder VALUES (true) ON CONFLICT DO NOTHING;

-- Rows that could not be placed in the record, and when a later catch-up placed them. Owner-only and immutable.
CREATE TABLE IF NOT EXISTS evidence.capture_gaps (
  tenant_id text NOT NULL,
  gap_id uuid NOT NULL DEFAULT uuidv7(),
  table_name text NOT NULL,
  change text NOT NULL CHECK (change IN ('insert', 'revoke', 'intake', 'retire')),
  key_sha256 text NOT NULL CHECK (key_sha256 ~ '^[0-9a-f]{64}$'),
  row_sha256 text NOT NULL CHECK (row_sha256 ~ '^[0-9a-f]{64}$'),
  event_type text NOT NULL,
  idempotency_key text NOT NULL,
  reason text NOT NULL CHECK (reason IN ('scope_unresolved', 'tenant_disabled', 'lab_disabled',
    'source_time_out_of_range', 'scope_unprepared')),
  group_key text,
  scope_kind text,
  project_id uuid,
  run_id uuid,
  origin text NOT NULL CHECK (origin IN ('live', 'caught_up')),
  caller_role name NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, gap_id),
  UNIQUE (tenant_id, idempotency_key)
);
CREATE TABLE IF NOT EXISTS evidence.capture_gap_resolutions (
  tenant_id text NOT NULL,
  gap_id uuid NOT NULL,
  record_id uuid NOT NULL,
  resolved_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, gap_id),
  FOREIGN KEY (tenant_id, gap_id) REFERENCES evidence.capture_gaps (tenant_id, gap_id),
  FOREIGN KEY (tenant_id, record_id) REFERENCES evidence.receipts (tenant_id, record_id)
);
-- scope_unprepared is no longer written; an earlier draft wrote it, and its entries stay valid.
ALTER TABLE evidence.capture_gaps DROP CONSTRAINT IF EXISTS capture_gaps_reason_check;
ALTER TABLE evidence.capture_gaps ADD CONSTRAINT capture_gaps_reason_check
  CHECK (reason IN ('scope_unresolved', 'tenant_disabled', 'lab_disabled', 'source_time_out_of_range',
    'scope_unprepared'));
CREATE OR REPLACE FUNCTION evidence.capture_immutable() RETURNS trigger LANGUAGE plpgsql
SET search_path = pg_catalog AS $$
BEGIN
  RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:immutable_record';
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_immutable() FROM PUBLIC;
DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['capture_gaps', 'capture_gap_resolutions', 'capture_recorder'] LOOP
    EXECUTE format('ALTER TABLE evidence.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON evidence.%I FROM PUBLIC', t);
    EXECUTE format('DROP TRIGGER IF EXISTS immutable_record ON evidence.%I', t);
    EXECUTE format('CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON evidence.%I
      FOR EACH ROW EXECUTE FUNCTION evidence.capture_immutable()', t);
    EXECUTE format('DROP TRIGGER IF EXISTS no_truncate ON evidence.%I', t);
    EXECUTE format('CREATE TRIGGER no_truncate BEFORE TRUNCATE ON evidence.%I
      FOR EACH STATEMENT EXECUTE FUNCTION evidence.capture_immutable()', t);
  END LOOP;
END $$;

-- The captured tables: the backfill's list and the tool group tables of migration 080, each with its key columns and
-- column types, read from the catalog.
CREATE OR REPLACE FUNCTION evidence.capture_tables()
RETURNS TABLE (table_name text, key_columns text[], column_types jsonb)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
  SELECT it.table_name, it.key_columns, it.column_types FROM evidence.import_tables() it
  UNION ALL
  SELECT c.relname::text,
    (SELECT array_agg(a.attname::text ORDER BY k.ord) FROM pg_index i
       CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord)
       JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
       WHERE i.indrelid = c.oid AND i.indisprimary),
    (SELECT jsonb_object_agg(a.attname, format_type(a.atttypid, NULL)) FROM pg_attribute a
       WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped)
  FROM (VALUES ('broker_tool_groups'), ('broker_tool_group_members'), ('broker_tool_group_grants')) AS listed(name)
  JOIN pg_class c ON c.relname = listed.name AND c.relkind IN ('r', 'p')
  JOIN pg_namespace n ON n.oid = c.relnamespace AND n.nspname = 'enrichedlab_agent'
$$;
REVOKE ALL ON FUNCTION evidence.capture_tables() FROM PUBLIC;

-- The one update each of three append-guarded tables permits, by the change it makes.
CREATE OR REPLACE FUNCTION evidence.capture_update_of(p_table text) RETURNS text LANGUAGE sql IMMUTABLE
SET search_path = pg_catalog AS $$
  SELECT CASE p_table WHEN 'broker_tool_group_grants' THEN 'revoke' WHEN 'broker_datasets' THEN 'intake'
                      WHEN 'broker_reviewed_queries' THEN 'retire' END
$$;
REVOKE ALL ON FUNCTION evidence.capture_update_of(text) FROM PUBLIC;
-- Whether a row as it stands shows its table's permitted update.
CREATE OR REPLACE FUNCTION evidence.capture_update_shown(p_table text, p_row jsonb) RETURNS boolean LANGUAGE sql
IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT CASE p_table
    WHEN 'broker_tool_group_grants' THEN p_row->'revoked_at' IS DISTINCT FROM 'null'::jsonb
    WHEN 'broker_datasets' THEN p_row->'intake_task_id' IS DISTINCT FROM 'null'::jsonb
    WHEN 'broker_reviewed_queries' THEN p_row->'definition'->>'state' = 'retired'
    ELSE false END
$$;
REVOKE ALL ON FUNCTION evidence.capture_update_shown(text, jsonb) FROM PUBLIC;
-- The change an update made, when it is its table's permitted one.
CREATE OR REPLACE FUNCTION evidence.capture_update_change(p_table text, p_old jsonb, p_new jsonb) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, evidence AS $$
  SELECT CASE WHEN NOT evidence.capture_update_shown(p_table, p_old) AND evidence.capture_update_shown(p_table, p_new)
    THEN evidence.capture_update_of(p_table) END
$$;
REVOKE ALL ON FUNCTION evidence.capture_update_change(text, jsonb, jsonb) FROM PUBLIC;

-- A captured change's event type and idempotency key.
CREATE OR REPLACE FUNCTION evidence.capture_event_type(p_table text, p_change text) RETURNS text LANGUAGE sql IMMUTABLE
SET search_path = pg_catalog AS $$
  SELECT CASE p_change WHEN 'insert' THEN 'broker.' || regexp_replace(p_table, '^broker_', '')
                       WHEN 'revoke' THEN 'broker.tool_group_grant_revoked'
                       WHEN 'intake' THEN 'broker.dataset_intake_bound'
                       WHEN 'retire' THEN 'broker.reviewed_query_retired' END
$$;
REVOKE ALL ON FUNCTION evidence.capture_event_type(text, text) FROM PUBLIC;
CREATE OR REPLACE FUNCTION evidence.capture_idempotency_key(p_table text, p_change text, p_key jsonb) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT CASE WHEN p_change = 'insert' THEN 'live' ELSE p_change END || ':'
    || regexp_replace(p_table, '^broker_', '') || ':' || encode(sha256(convert_to(p_key::text, 'UTF8')), 'hex')
$$;
REVOKE ALL ON FUNCTION evidence.capture_idempotency_key(text, text, jsonb) FROM PUBLIC;

-- Concurrency. Broker transactions only log. The recorder and catch-up are the only capture writers of streams; each
-- prepares its scopes first (preparing inserts rows whose foreign keys share-lock lab, project and run rows, so it may
-- wait for a broker transaction holding one, but no broker transaction waits for it), then locks streams in the order
-- of (tenant, lab, scope kind, project, run). One recorder runs at a time, and it and a tenant's catch-up exclude each
-- other through the tenant's opt-in row, locked in tenant order. The append service appends one record per call.

-- The tenant's capture producer, registered on first use, with a grant for this scope (an existing row is neither
-- changed nor locked; a later migration that adds event types updates the grants itself). Refuses when it was disabled,
-- so a disabled capture producer stops writes rather than leaving them unrecorded; capture is turned off by deleting
-- the tenant's capture_tenants row.
CREATE OR REPLACE FUNCTION evidence.capture_producer(p_tenant text, p_group text, p_scope_kind text, p_project uuid)
RETURNS uuid LANGUAGE plpgsql SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
DECLARE
  held evidence.producers%ROWTYPE;
  event_types text[];
BEGIN
  INSERT INTO evidence.producers (tenant_id, identity, kind, database_role)
    VALUES (p_tenant, 'broker:capture', 'broker', 'evidence_capture')
    ON CONFLICT (tenant_id, identity) DO NOTHING;
  SELECT * INTO held FROM evidence.producers WHERE tenant_id = p_tenant AND identity = 'broker:capture';
  IF NOT held.enabled OR held.kind <> 'broker' THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:capture_producer_disabled';
  END IF;
  -- Every insert and update event; a grant lists at most 64.
  SELECT array_agg(evidence.capture_event_type(ct.table_name, 'insert') ORDER BY ct.table_name)
      || ARRAY['broker.tool_group_grant_revoked', 'broker.dataset_intake_bound', 'broker.reviewed_query_retired']
    INTO event_types FROM evidence.capture_tables() ct;
  INSERT INTO evidence.producer_grants (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, all_runs,
      event_types, actor_role)
    VALUES (p_tenant, held.producer_id, p_group, p_scope_kind, p_project, NULL, p_scope_kind = 'project', event_types,
      'broker')
    ON CONFLICT (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, all_runs) DO NOTHING;
  RETURN held.producer_id;
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_producer(text, text, text, uuid) FROM PUBLIC;

-- Capture never waits for a lock that a broker transaction may be waiting behind it for long enough to be chosen as a
-- deadlock victim (a broker transaction checks for a deadlock after deadlock_timeout, 1 s here). Row locks capture
-- takes are NOWAIT or SKIP LOCKED; the only waits left are a new scope's rows meeting another session's uncommitted
-- insert of the same row, at most lock_timeout (200 ms) each, inside a recorder or preparation statement the caller
-- bounds below deadlock_timeout minus that (capture_bounded), so no broker transaction waits behind capture for a
-- whole deadlock_timeout. Catch-up holds its locks longer and so never waits: it refuses a scope not yet prepared.
CREATE OR REPLACE FUNCTION evidence.capture_bounded() RETURNS boolean LANGUAGE sql STABLE
SET search_path = pg_catalog AS $$
  SELECT extract(epoch FROM current_setting('statement_timeout')::interval) > 0
    AND extract(epoch FROM current_setting('statement_timeout')::interval) + 0.2
      <= extract(epoch FROM current_setting('deadlock_timeout')::interval)
$$;
REVOKE ALL ON FUNCTION evidence.capture_bounded() FROM PUBLIC;

-- Whether a scope is ready to record into: the enabled capture producer, its grant for the scope and the stream.
CREATE OR REPLACE FUNCTION evidence.capture_scope_ready(p_tenant text, p_group text, p_scope_kind text, p_project uuid,
  p_run uuid)
RETURNS boolean LANGUAGE sql STABLE SET search_path = pg_catalog, evidence AS $$
  SELECT EXISTS (SELECT 1 FROM evidence.producers p JOIN evidence.producer_grants g
      ON g.tenant_id = p.tenant_id AND g.producer_id = p.producer_id
    WHERE p.tenant_id = p_tenant AND p.identity = 'broker:capture' AND p.enabled AND p.kind = 'broker'
      AND g.group_key = p_group AND g.scope_kind = p_scope_kind AND g.project_id IS NOT DISTINCT FROM p_project
      AND g.run_id IS NULL AND g.all_runs = (p_scope_kind = 'project'))
    AND EXISTS (SELECT 1 FROM evidence.streams s WHERE s.tenant_id = p_tenant AND s.group_key = p_group
      AND s.scope_kind = p_scope_kind AND s.project_id IS NOT DISTINCT FROM p_project
      AND s.run_id IS NOT DISTINCT FROM p_run)
$$;
REVOKE ALL ON FUNCTION evidence.capture_scope_ready(text, text, text, uuid, uuid) FROM PUBLIC;

-- A scope ready to record into: the capture producer, its grant and the stream, created when missing. A ready scope is
-- returned without a lock. Creating one references its lab, project and run rows, which are share-locked first without
-- waiting (lock_not_available when a broker transaction holds one), so a broker transaction holding several of them
-- never waits on capture while capture waits on it. Returns the producer.
CREATE OR REPLACE FUNCTION evidence.capture_prepare(p_tenant text, p_group text, p_scope_kind text, p_project uuid,
  p_run uuid)
RETURNS uuid LANGUAGE plpgsql SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
BEGIN
  IF evidence.capture_scope_ready(p_tenant, p_group, p_scope_kind, p_project, p_run) THEN
    RETURN (SELECT p.producer_id FROM evidence.producers p WHERE p.tenant_id = p_tenant AND p.identity = 'broker:capture');
  END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_research_groups g WHERE g.tenant_id = p_tenant AND g.group_key = p_group
    FOR KEY SHARE NOWAIT;
  IF p_project IS NOT NULL THEN
    PERFORM 1 FROM enrichedlab_agent.broker_projects pr WHERE pr.tenant_id = p_tenant AND pr.project_id = p_project
      FOR KEY SHARE NOWAIT;
  END IF;
  IF p_run IS NOT NULL THEN
    PERFORM 1 FROM enrichedlab_agent.broker_research_runs ru WHERE ru.tenant_id = p_tenant AND ru.run_id = p_run
      FOR KEY SHARE NOWAIT;
  END IF;
  INSERT INTO evidence.streams (tenant_id, group_key, scope_kind, project_id, run_id)
    VALUES (p_tenant, p_group, p_scope_kind, p_project, p_run) ON CONFLICT DO NOTHING;
  RETURN evidence.capture_producer(p_tenant, p_group, p_scope_kind, p_project);
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_prepare(text, text, text, uuid, uuid) FROM PUBLIC;

-- Prepares up to p_limit of one tenant's scopes that are not ready (its standalone lab, then each lab, project and run),
-- in a statement the caller bounds (capture_bounded), and returns {prepared, remaining}. Migration owner only; run it
-- until nothing remains before a catch-up, which refuses a scope not yet prepared. The recorder prepares the scopes it
-- meets itself.
CREATE OR REPLACE FUNCTION evidence.capture_prepare_scopes(p_tenant text, p_standalone_group text, p_limit integer)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent, pg_temp
SET enrichedlab.tenant_id = '' SET lock_timeout = '200ms' AS $$
DECLARE
  r record;
  n integer := 0;
  left_over integer := 0;
BEGIN
  IF session_user <> evidence.import_owner() THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:capture_not_migration_role';
  END IF;
  IF NOT evidence.capture_bounded() THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:capture_statement_unbounded';
  END IF;
  IF p_limit IS NULL OR p_limit < 1 OR p_limit > 1000 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:capture_limit_invalid';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  FOR r IN SELECT x.grp, x.kind, x.project, x.run FROM (
        SELECT 0 AS step, p_standalone_group AS grp, 'standalone' AS kind, NULL::uuid AS project, NULL::uuid AS run
        UNION ALL SELECT 1, g.group_key, 'lab', NULL, NULL FROM enrichedlab_agent.broker_research_groups g
          WHERE g.tenant_id = p_tenant
        UNION ALL SELECT 2, pr.group_key, 'project', pr.project_id, NULL FROM enrichedlab_agent.broker_projects pr
          WHERE pr.tenant_id = p_tenant
        UNION ALL SELECT 3, pr.group_key, 'project', ru.project_id, ru.run_id FROM enrichedlab_agent.broker_research_runs ru
          JOIN enrichedlab_agent.broker_projects pr ON pr.tenant_id = ru.tenant_id AND pr.project_id = ru.project_id
          WHERE ru.tenant_id = p_tenant) x
      WHERE NOT evidence.capture_scope_ready(p_tenant, x.grp, x.kind, x.project, x.run)
      ORDER BY x.step, x.grp, x.project NULLS FIRST, x.run NULLS FIRST LOOP
    IF n >= p_limit THEN
      left_over := left_over + 1;
    ELSE
      PERFORM evidence.capture_prepare(p_tenant, r.grp, r.kind, r.project, r.run);
      n := n + 1;
    END IF;
  END LOOP;
  RETURN jsonb_build_object('prepared', n, 'remaining', left_over);
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_prepare_scopes(text, text, integer) FROM PUBLIC;

-- The capture log: each captured change as written, until the recorder records or discards it. A regular, logged table
-- owned by the migration owner, with no grants and row security on, so no runtime session can read, change or remove an
-- entry; updates and truncation are refused for every role. written_at is the writing transaction's start time and
-- written_by its session role, carried into the record and the gap entries as if recorded at commit.
CREATE TABLE IF NOT EXISTS evidence.capture_log (
  n bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  tenant_id text NOT NULL,
  table_name text NOT NULL,
  change text NOT NULL CHECK (change IN ('insert', 'revoke', 'intake', 'retire')),
  row_value jsonb NOT NULL,
  written_at timestamptz NOT NULL,
  written_by name NOT NULL
);
CREATE INDEX IF NOT EXISTS capture_log_tenant ON evidence.capture_log (tenant_id, table_name);
ALTER TABLE evidence.capture_log ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON evidence.capture_log FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON evidence.capture_log;
CREATE TRIGGER immutable_record BEFORE UPDATE ON evidence.capture_log
  FOR EACH ROW EXECUTE FUNCTION evidence.capture_immutable();
DROP TRIGGER IF EXISTS no_truncate ON evidence.capture_log;
CREATE TRIGGER no_truncate BEFORE TRUNCATE ON evidence.capture_log
  FOR EACH STATEMENT EXECUTE FUNCTION evidence.capture_immutable();

-- One change's derivation: its record's scope, event, key, time and payload, or the reason it cannot be placed.
-- p_origin is `live` (the recorder) or `caught_up` (catch-up); p_change is `insert` or the table's permitted update;
-- p_written_at, for a live change, is its writing transaction's start time.
CREATE OR REPLACE FUNCTION evidence.capture_derive(p_tenant text, p_table text, p_row jsonb, p_origin text,
  p_change text, p_standalone_group text, p_written_at timestamptz)
RETURNS jsonb LANGUAGE plpgsql SET search_path = pg_catalog, evidence, enrichedlab_agent, pg_temp
SET TimeZone = 'UTC' SET bytea_output = 'hex' SET extra_float_digits = 1 AS $$
DECLARE
  meta record;
  key jsonb;
  scope record;
  timing record;
  source timestamptz;
  time_label text;
  reason text;
  body jsonb;
  out text;
BEGIN
  IF p_origin NOT IN ('live', 'caught_up') OR p_change IS NULL
    OR (p_change <> 'insert' AND p_change IS DISTINCT FROM evidence.capture_update_of(p_table)) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:capture_change_invalid';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO meta FROM evidence.capture_tables() ct WHERE ct.table_name = p_table;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:capture_table_unknown'; END IF;
  key := evidence.import_key(p_row, meta.key_columns, meta.column_types);
  SELECT * INTO scope FROM evidence.import_scope(p_tenant, p_table, p_row, p_standalone_group);
  IF scope.label = 'unresolved' THEN
    reason := 'scope_unresolved';
  ELSIF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tenants t WHERE t.tenant_id = p_tenant AND t.enabled) THEN
    -- A disabled tenant's cleanup (a task finished as not allowed) still commits.
    reason := 'tenant_disabled';
  ELSIF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_research_groups g
      WHERE g.tenant_id = p_tenant AND g.group_key = scope.group_key AND g.enabled) THEN
    reason := 'lab_disabled';
  END IF;
  SELECT * INTO timing FROM evidence.import_source_time(p_tenant, p_table, p_row, meta.column_types);
  source := timing.source_time;
  time_label := timing.label;
  IF time_label = 'missing' AND p_origin = 'live' AND p_written_at IS NOT NULL THEN
    source := p_written_at;
    time_label := 'captured';
  END IF;
  IF reason IS NULL AND (source IS NULL OR source < '1970-01-01T00:00:00Z'::timestamptz
      OR source > clock_timestamp() + interval '1 day') THEN
    reason := 'source_time_out_of_range';
  END IF;
  IF reason IS NOT NULL THEN
    RETURN jsonb_build_object('reason', reason, 'table', p_table, 'change', p_change, 'origin', p_origin,
      'key_sha256', encode(sha256(convert_to(key::text, 'UTF8')), 'hex'),
      'row_sha256', encode(sha256(convert_to(p_row::text, 'UTF8')), 'hex'),
      'event', evidence.capture_event_type(p_table, p_change),
      'idempotency', evidence.capture_idempotency_key(p_table, p_change, key),
      'group', CASE WHEN reason <> 'scope_unresolved' THEN scope.group_key END,
      'scope_kind', CASE WHEN reason <> 'scope_unresolved' THEN scope.scope_kind END,
      'project', CASE WHEN reason <> 'scope_unresolved' THEN scope.project_id END,
      'run', CASE WHEN reason <> 'scope_unresolved' THEN scope.run_id END);
  END IF;
  body := evidence.import_payload(p_table, p_row, key, meta.column_types, time_label, scope.label)::jsonb
    || jsonb_build_object('origin', p_origin, 'change', p_change);
  out := body::text;
  IF octet_length(out) > 65536 THEN
    out := (body - 'fields' || jsonb_build_object('fields', NULL, 'truncated', true))::text;
  END IF;
  RETURN jsonb_build_object('group', scope.group_key, 'scope_kind', scope.scope_kind, 'project', scope.project_id,
    'run', scope.run_id, 'event', evidence.capture_event_type(p_table, p_change),
    'idempotency', evidence.capture_idempotency_key(p_table, p_change, key),
    'source_time', source, 'payload', out);
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_derive(text, text, jsonb, text, text, text, timestamptz) FROM PUBLIC;

-- Records a batch of changes, each {tenant_id, table_name, change, row_value, standalone_group, written_at,
-- written_by}, in that order within each stream: derives every change and writes the gap entries, prepares every scope
-- before any stream is locked, then appends in the global stream order. A placed change with an earlier gap entry gains
-- its resolution. Returns {appended, gaps}.
CREATE OR REPLACE FUNCTION evidence.capture_batch(p_items jsonb[], p_origin text)
RETURNS jsonb LANGUAGE plpgsql SET search_path = pg_catalog, evidence, enrichedlab_agent, pg_temp
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  item jsonb;
  d jsonb;
  ready jsonb[] := '{}';
  position bigint := 0;
  r record;
  receipt jsonb;
  gaps integer := 0;
  appended integer := 0;
BEGIN
  FOREACH item IN ARRAY p_items LOOP
    position := position + 1;
    d := evidence.capture_derive(item->>'tenant_id', item->>'table_name', item->'row_value', p_origin, item->>'change',
      item->>'standalone_group', (item->>'written_at')::timestamptz);
    IF d ? 'reason' THEN
      PERFORM set_config('enrichedlab.tenant_id', item->>'tenant_id', true);
      INSERT INTO evidence.capture_gaps (tenant_id, table_name, change, key_sha256, row_sha256, event_type,
          idempotency_key, reason, group_key, scope_kind, project_id, run_id, origin, caller_role)
        VALUES (item->>'tenant_id', d->>'table', d->>'change', d->>'key_sha256', d->>'row_sha256', d->>'event',
          d->>'idempotency', d->>'reason', d->>'group', d->>'scope_kind', (d->>'project')::uuid, (d->>'run')::uuid,
          p_origin, coalesce(item->>'written_by', session_user))
        ON CONFLICT (tenant_id, idempotency_key) DO NOTHING;
      gaps := gaps + 1;
    ELSE
      ready := ready || (d || jsonb_build_object('tenant_id', item->>'tenant_id', 'position', position));
    END IF;
  END LOOP;
  -- Every scope is prepared before any stream is locked (catch-up, which never waits, refuses one not yet prepared),
  -- then every stream is locked without waiting, in the global stream order.
  FOR r IN SELECT DISTINCT x->>'tenant_id' AS tenant, x->>'group' AS grp, x->>'scope_kind' AS kind,
      (x->>'project')::uuid AS project, (x->>'run')::uuid AS run FROM unnest(ready) AS x
      ORDER BY 1, 2, 3, 4 NULLS FIRST, 5 NULLS FIRST LOOP
    PERFORM set_config('enrichedlab.tenant_id', r.tenant, true);
    IF p_origin = 'caught_up' AND NOT evidence.capture_scope_ready(r.tenant, r.grp, r.kind, r.project, r.run) THEN
      RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:capture_scope_unprepared';
    END IF;
    PERFORM evidence.capture_prepare(r.tenant, r.grp, r.kind, r.project, r.run);
  END LOOP;
  FOR r IN SELECT DISTINCT x->>'tenant_id' AS tenant, x->>'group' AS grp, x->>'scope_kind' AS kind,
      (x->>'project')::uuid AS project, (x->>'run')::uuid AS run FROM unnest(ready) AS x
      ORDER BY 1, 2, 3, 4 NULLS FIRST, 5 NULLS FIRST LOOP
    PERFORM 1 FROM evidence.streams st WHERE st.tenant_id = r.tenant AND st.group_key = r.grp AND st.scope_kind = r.kind
      AND st.project_id IS NOT DISTINCT FROM r.project AND st.run_id IS NOT DISTINCT FROM r.run FOR UPDATE NOWAIT;
  END LOOP;
  -- Then appended in the global stream order, and in batch order within a stream.
  FOR r IN SELECT x FROM unnest(ready) AS x
      ORDER BY x->>'tenant_id', x->>'group', x->>'scope_kind', (x->>'project')::uuid NULLS FIRST,
        (x->>'run')::uuid NULLS FIRST, (x->>'position')::bigint LOOP
    PERFORM set_config('enrichedlab.tenant_id', r.x->>'tenant_id', true);
    receipt := evidence.append_as(r.x->>'tenant_id',
      (SELECT p.producer_id FROM evidence.producers p WHERE p.tenant_id = r.x->>'tenant_id' AND p.identity = 'broker:capture'),
      r.x->>'group', r.x->>'scope_kind', (r.x->>'project')::uuid, (r.x->>'run')::uuid, r.x->>'event',
      r.x->>'idempotency', (r.x->>'source_time')::timestamptz, r.x->>'payload',
      encode(sha256(convert_to(r.x->>'payload', 'UTF8')), 'hex'));
    INSERT INTO evidence.capture_gap_resolutions (tenant_id, gap_id, record_id)
      SELECT g.tenant_id, g.gap_id, (receipt->>'record_id')::uuid FROM evidence.capture_gaps g
      WHERE g.tenant_id = r.x->>'tenant_id' AND g.idempotency_key = r.x->>'idempotency'
      ON CONFLICT DO NOTHING;
    appended := appended + 1;
  END LOOP;
  RETURN jsonb_build_object('appended', appended, 'gaps', gaps);
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_batch(jsonb[], text) FROM PUBLIC;

-- The row trigger: logs the inserted row (or a permitted update) whatever its tenant. The row is read with the record's
-- canonical settings (UTC times, hex bytes, shortest exact floats), so its digest and copied fields do not depend on
-- the writing session. The captured table's name is the trigger's argument.
CREATE OR REPLACE FUNCTION evidence.capture_note() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent, pg_temp
SET TimeZone = 'UTC' SET bytea_output = 'hex' SET extra_float_digits = 1 AS $$
DECLARE
  row_value jsonb := to_jsonb(NEW);
  v_change text := 'insert';
BEGIN
  IF TG_NARGS <> 1 THEN RAISE EXCEPTION USING ERRCODE = '39P01', MESSAGE = 'evidence:capture_trigger_invalid'; END IF;
  IF TG_OP = 'UPDATE' THEN
    v_change := evidence.capture_update_change(TG_ARGV[0], to_jsonb(OLD), row_value);
    IF v_change IS NULL THEN RETURN NULL; END IF;
  END IF;
  INSERT INTO evidence.capture_log (tenant_id, table_name, change, row_value, written_at, written_by)
    VALUES (row_value->>'tenant_id', TG_ARGV[0], v_change, row_value, now(), session_user);
  RETURN NULL;
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_note() FROM PUBLIC;

DO $$ DECLARE t text; BEGIN
  FOR t IN SELECT table_name FROM evidence.capture_tables() LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS evidence_capture ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER evidence_capture AFTER INSERT ON enrichedlab_agent.%I '
      'FOR EACH ROW EXECUTE FUNCTION evidence.capture_note(%L)', t, t);
  END LOOP;
  FOREACH t IN ARRAY ARRAY['broker_tool_group_grants', 'broker_datasets', 'broker_reviewed_queries'] LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS evidence_capture_update ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER evidence_capture_update AFTER UPDATE ON enrichedlab_agent.%I '
      'FOR EACH ROW EXECUTE FUNCTION evidence.capture_note(%L)', t, t);
  END LOOP;
END $$;

-- Opts a tenant in (migration owner only). The recorder records the tenant's log entries from now, preparing the scopes
-- it meets; capture_prepare_scopes prepares the rest before catch-up records the rows written before.
CREATE OR REPLACE FUNCTION evidence.capture_enable(p_tenant text, p_standalone_group text)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent, pg_temp
SET enrichedlab.tenant_id = '' AS $$
BEGIN
  IF session_user <> evidence.import_owner() THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:capture_not_migration_role';
  END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_research_groups g
    WHERE g.tenant_id = p_tenant AND g.group_key = p_standalone_group AND g.enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'evidence:capture_lab_unknown'; END IF;
  INSERT INTO evidence.capture_tenants (tenant_id, standalone_group) VALUES (p_tenant, p_standalone_group)
    ON CONFLICT (tenant_id) DO UPDATE SET standalone_group = EXCLUDED.standalone_group;
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_enable(text, text) FROM PUBLIC;

-- A row's change's record, as its payload: the capture producer's under the capture key and event type, or for an
-- insert the history import's under the import key and event type. Another producer's receipt under the same key is
-- not this change's record.
CREATE OR REPLACE FUNCTION evidence.capture_found(p_tenant text, p_table text, p_change text, p_key jsonb)
RETURNS jsonb LANGUAGE sql STABLE SET search_path = pg_catalog, evidence AS $$
  SELECT rc.payload::jsonb FROM evidence.receipts r
    JOIN evidence.records rc ON rc.tenant_id = r.tenant_id AND rc.stream_id = r.stream_id AND rc.sequence = r.sequence
    JOIN evidence.producers p ON p.tenant_id = r.tenant_id AND p.producer_id = r.producer_id
    WHERE r.tenant_id = p_tenant
      AND ((p.identity = 'broker:capture' AND r.idempotency_key = evidence.capture_idempotency_key(p_table, p_change, p_key)
            AND rc.event_type = evidence.capture_event_type(p_table, p_change))
        OR (p_change = 'insert' AND p.identity = 'broker:history'
            AND r.idempotency_key = evidence.import_idempotency_key(p_table, p_key)
            AND rc.event_type = evidence.import_event_type(p_table)))
    LIMIT 1
$$;
REVOKE ALL ON FUNCTION evidence.capture_found(text, text, text, jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION evidence.capture_recorded(p_tenant text, p_table text, p_change text, p_key jsonb)
RETURNS boolean LANGUAGE sql STABLE SET search_path = pg_catalog, evidence AS $$
  SELECT evidence.capture_found(p_tenant, p_table, p_change, p_key) IS NOT NULL
$$;
REVOKE ALL ON FUNCTION evidence.capture_recorded(text, text, text, jsonb) FROM PUBLIC;

-- The changes of a row, as it stands, that have no record: its insert; and its table's permitted update when the row
-- shows it, no update record exists, and the row's digest differs from the one its insert or import record holds (the
-- update came after that record). A row first recorded now, from its current state, needs no separate update record.
CREATE OR REPLACE FUNCTION evidence.capture_changes_due(p_tenant text, p_table text, p_row jsonb, p_key jsonb)
RETURNS text[] LANGUAGE plpgsql STABLE SET search_path = pg_catalog, evidence AS $$
DECLARE
  due text[] := '{}';
  update_change text := evidence.capture_update_of(p_table);
  recorded_digest text;
BEGIN
  recorded_digest := evidence.capture_found(p_tenant, p_table, 'insert', p_key)->>'row_sha256';
  IF recorded_digest IS NULL THEN
    RETURN ARRAY['insert'];
  END IF;
  IF update_change IS NOT NULL AND evidence.capture_update_shown(p_table, p_row)
    AND recorded_digest <> encode(sha256(convert_to(p_row::text, 'UTF8')), 'hex')
    AND NOT evidence.capture_recorded(p_tenant, p_table, update_change, p_key) THEN
    due := due || update_change;
  END IF;
  RETURN due;
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_changes_due(text, text, jsonb, jsonb) FROM PUBLIC;

-- Catch-up (migration owner only): every change of the given tables' rows (every captured table by default) that has
-- no record gets one with origin `caught_up`, or a gap entry, in one batch: every scope must already be prepared
-- (capture_prepare_scopes; one that is not is refused), then the streams are locked without waiting and appended in
-- the global stream order. It waits for no lock: one another session holds raises lock_not_available, and the caller
-- runs it again. One catch-up per transaction, so its stream locks follow that order; a second call in the same
-- transaction is refused. A tenant's catch-ups and the recorder's batches holding its entries run one
-- at a time (the tenant's opt-in row, locked). A change the log still holds is left to the recorder, which records it
-- as it was written (catch-up sees only the row as it stands now), and is counted as pending. Rerunning appends nothing
-- new. Returns the counts.
DROP FUNCTION IF EXISTS evidence.capture_catchup(text, text);
CREATE OR REPLACE FUNCTION evidence.capture_catchup(p_tenant text, p_tables text[] DEFAULT NULL)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent, pg_temp
SET enrichedlab.tenant_id = '' SET TimeZone = 'UTC' SET bytea_output = 'hex' SET extra_float_digits = 1
SET lock_timeout = '200ms' AS $$
DECLARE
  setting evidence.capture_tenants%ROWTYPE;
  meta record;
  row_value jsonb;
  key jsonb;
  v_change text;
  items jsonb[] := '{}';
  table_items jsonb[];
  item jsonb;
  logged text[];
  n_pending integer := 0;
  rows_seen jsonb := '{}';
  seen integer;
  result jsonb := '{"appended": 0, "gaps": 0}';
BEGIN
  IF session_user <> evidence.import_owner() THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:capture_not_migration_role';
  END IF;
  IF current_setting('evidence.capture_catchup_ran', true) = 'yes' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:capture_catchup_once_per_transaction';
  END IF;
  PERFORM set_config('evidence.capture_catchup_ran', 'yes', true);
  -- The tenant's opt-in row is its catch-up lock (the recorder takes it too).
  SELECT * INTO setting FROM evidence.capture_tenants WHERE tenant_id = p_tenant FOR NO KEY UPDATE NOWAIT;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:capture_not_enabled'; END IF;
  IF p_tables IS NOT NULL AND EXISTS (SELECT 1 FROM unnest(p_tables) t
      WHERE t NOT IN (SELECT ct.table_name FROM evidence.capture_tables() ct)) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:capture_table_unknown';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  FOR meta IN SELECT * FROM evidence.capture_tables() ct
      WHERE p_tables IS NULL OR ct.table_name = ANY (p_tables) ORDER BY ct.table_name LOOP
    seen := 0;
    table_items := '{}';
    FOR row_value IN EXECUTE format('SELECT to_jsonb(t) FROM enrichedlab_agent.%I t WHERE t.tenant_id = $1 ORDER BY %s',
        meta.table_name, (SELECT string_agg(format('t.%I', c), ', ') FROM unnest(meta.key_columns) AS c))
      USING p_tenant LOOP
      seen := seen + 1;
      key := evidence.import_key(row_value, meta.key_columns, meta.column_types);
      FOREACH v_change IN ARRAY evidence.capture_changes_due(p_tenant, meta.table_name, row_value, key) LOOP
        table_items := table_items || jsonb_build_object('tenant_id', p_tenant, 'table_name', meta.table_name,
          'change', v_change, 'row_value', row_value, 'standalone_group', setting.standalone_group, 'key', key);
      END LOOP;
    END LOOP;
    -- The changes the log holds, read after the rows: a row the scan saw was logged by the transaction that wrote it,
    -- and no recorder batch of this tenant commits while this catch-up holds the tenant's row.
    IF cardinality(table_items) > 0 THEN
      SELECT coalesce(array_agg(DISTINCT l.change || ' '
          || evidence.import_key(l.row_value, meta.key_columns, meta.column_types)::text), '{}')
        INTO logged FROM evidence.capture_log l WHERE l.tenant_id = p_tenant AND l.table_name = meta.table_name;
      FOREACH item IN ARRAY table_items LOOP
        IF (item->>'change') || ' ' || (item->'key')::text = ANY (logged) THEN
          n_pending := n_pending + 1;
        ELSE
          items := items || (item - 'key');
        END IF;
      END LOOP;
    END IF;
    rows_seen := rows_seen || jsonb_build_object(meta.table_name, seen);
  END LOOP;
  IF cardinality(items) > 0 THEN result := evidence.capture_batch(items, 'caught_up'); END IF;
  RETURN jsonb_build_object('rows', rows_seen, 'pending', n_pending) || result;
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_catchup(text, text[]) FROM PUBLIC;

-- Seconds since the oldest logged entry's writing transaction started; 0 when the log is empty. Owner only.
CREATE OR REPLACE FUNCTION evidence.capture_log_age() RETURNS integer LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, evidence AS $$
  SELECT coalesce((SELECT floor(extract(epoch FROM clock_timestamp() - l.written_at))::integer
    FROM evidence.capture_log l ORDER BY l.n LIMIT 1), 0)
$$;
REVOKE ALL ON FUNCTION evidence.capture_log_age() FROM PUBLIC;

-- The recorder: records up to p_limit committed log entries in log order and deletes them, in one transaction. Entries
-- of a tenant that has not opted in are discarded (its rows stay in the broker's tables for a catch-up); an entry whose
-- change already has its record (catch-up recorded it first) is removed without a second record. One recorder runs at
-- a time (it locks the row of evidence.capture_recorder); a second call returns at once with busy. It locks each batch
-- tenant's opt-in row in tenant order, as catch-up does, before preparing a scope or locking a stream, and treats only
-- the tenants whose rows it locked as opted in. It takes no lock by waiting (see capture_bounded): a tenant row, scope
-- parent or stream another session holds defers the batch, which stays logged. The caller bounds the statement
-- (capture_bounded). The migration owner may call it directly; any other caller only through
-- evidence.capture_record_now. It records only what the triggers logged. Returns {taken,
-- appended, gaps, discarded, already_recorded, busy, deferred, oldest_seconds}: oldest_seconds is the age of the oldest
-- entry still logged (0 when none).
CREATE OR REPLACE FUNCTION evidence.capture_record(p_limit integer)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent, pg_temp
SET enrichedlab.tenant_id = '' SET TimeZone = 'UTC' SET bytea_output = 'hex' SET extra_float_digits = 1
SET lock_timeout = '200ms' AS $$
DECLARE
  batch evidence.capture_log[];
  taken evidence.capture_log;
  meta record;
  setting evidence.capture_tenants%ROWTYPE;
  items jsonb[] := '{}';
  n_taken integer := 0;
  n_discarded integer := 0;
  n_already integer := 0;
  tenant text;
  opted text[] := '{}';
  result jsonb := '{"appended": 0, "gaps": 0}';
BEGIN
  IF p_limit IS NULL OR p_limit < 1 OR p_limit > 10000 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:capture_limit_invalid';
  END IF;
  IF NOT evidence.capture_bounded() THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:capture_statement_unbounded';
  END IF;
  -- Any caller but the migration owner records only through evidence.capture_record_now, whose transaction no caller
  -- controls (current_query is the top-level statement the client sent).
  IF session_user <> evidence.import_owner()
      AND current_query() !~ '^\s*CALL\s+evidence\.capture_record_now\(\s*(\$1|[0-9]{1,5})\s*,\s*NULL\s*\)\s*;?\s*$' THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:capture_record_by_call_only';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM evidence.capture_recorder) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:capture_recorder_missing';
  END IF;
  PERFORM 1 FROM evidence.capture_recorder FOR NO KEY UPDATE SKIP LOCKED;
  IF NOT FOUND THEN
    RETURN jsonb_build_object('busy', true, 'taken', 0, 'oldest_seconds', evidence.capture_log_age());
  END IF;
  -- A lock another session holds defers the whole batch: its entries stay logged for the next call.
  BEGIN
    -- The batch is held in a local array: nothing the calling session created can stand in for it.
    WITH due AS (
      SELECT l.n FROM evidence.capture_log l ORDER BY l.n LIMIT p_limit FOR UPDATE SKIP LOCKED
    ), removed AS (
      DELETE FROM evidence.capture_log l USING due WHERE l.n = due.n RETURNING l.*
    )
    SELECT coalesce(array_agg(removed ORDER BY removed.n), '{}') INTO batch FROM removed;
    -- Each batch tenant's opt-in row, locked in tenant order before any scope or stream. A tenant that opts in after
    -- this is not opted in for this batch; its catch-up, which starts after the opt-in, records the discarded rows.
    FOR tenant IN SELECT c.tenant_id FROM evidence.capture_tenants c
        WHERE c.tenant_id IN (SELECT DISTINCT b.tenant_id FROM unnest(batch) b) ORDER BY c.tenant_id
        FOR NO KEY UPDATE OF c NOWAIT LOOP
      opted := opted || tenant;
    END LOOP;
    FOREACH taken IN ARRAY batch LOOP
      n_taken := n_taken + 1;
      IF NOT (taken.tenant_id = ANY (opted)) THEN
        n_discarded := n_discarded + 1;
        CONTINUE;
      END IF;
      SELECT * INTO setting FROM evidence.capture_tenants c WHERE c.tenant_id = taken.tenant_id;
      SELECT * INTO meta FROM evidence.capture_tables() ct WHERE ct.table_name = taken.table_name;
      PERFORM set_config('enrichedlab.tenant_id', taken.tenant_id, true);
      IF evidence.capture_found(taken.tenant_id, taken.table_name, taken.change,
          evidence.import_key(taken.row_value, meta.key_columns, meta.column_types)) IS NOT NULL THEN
        n_already := n_already + 1;
        CONTINUE;
      END IF;
      items := items || jsonb_build_object('tenant_id', taken.tenant_id, 'table_name', taken.table_name,
        'change', taken.change, 'row_value', taken.row_value, 'standalone_group', setting.standalone_group,
        'written_at', taken.written_at, 'written_by', taken.written_by);
    END LOOP;
    IF cardinality(items) > 0 THEN result := evidence.capture_batch(items, 'live'); END IF;
  EXCEPTION WHEN lock_not_available THEN
    RETURN jsonb_build_object('appended', 0, 'gaps', 0, 'taken', 0, 'discarded', 0, 'already_recorded', 0,
      'busy', false, 'deferred', true, 'oldest_seconds', evidence.capture_log_age());
  END;
  RETURN result || jsonb_build_object('taken', n_taken, 'discarded', n_discarded, 'already_recorded', n_already,
    'busy', false, 'deferred', false, 'oldest_seconds', evidence.capture_log_age());
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_record(integer) FROM PUBLIC;

-- The recorder as the append service runs it: a procedure that commits before recording, so it refuses inside a
-- transaction its caller opened (invalid transaction termination, before any lock is taken), and commits again before it
-- returns, so no caller keeps a lock the recording took. A procedure that commits cannot be a security definer, so this
-- one runs as its caller and calls the owner's capture_record, which accepts no other caller but the migration owner.
-- The caller sends exactly `CALL evidence.capture_record_now(<limit or $1>, NULL)` with its statement bounded
-- (capture_bounded); the result is the recorder's.
CREATE OR REPLACE PROCEDURE evidence.capture_record_now(p_limit integer, INOUT result jsonb DEFAULT NULL)
LANGUAGE plpgsql AS $$
BEGIN
  COMMIT;
  result := evidence.capture_record(p_limit);
  COMMIT;
END;
$$;
REVOKE ALL ON PROCEDURE evidence.capture_record_now(integer, jsonb) FROM PUBLIC;

-- Completeness (migration owner only): per captured table, the rows, and the changes with no record split into those
-- with a gap entry (`gap_only`), those with a log entry the recorder has not yet taken (`pending`), and those with none
-- of these (`missing`, which must be zero once catch-up is done).
CREATE OR REPLACE FUNCTION evidence.capture_missing(p_tenant text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' SET TimeZone = 'UTC' SET bytea_output = 'hex' SET extra_float_digits = 1 AS $$
DECLARE
  meta record;
  row_value jsonb;
  key jsonb;
  v_change text;
  rows_seen bigint;
  gap_only bigint;
  pending bigint;
  missing bigint;
  total_pending bigint := 0;
  tables jsonb := '{}'::jsonb;
  total_missing bigint := 0;
  total_gaps bigint := 0;
BEGIN
  IF session_user <> evidence.import_owner() THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:capture_not_migration_role';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  FOR meta IN SELECT * FROM evidence.capture_tables() ct ORDER BY ct.table_name LOOP
    rows_seen := 0;
    gap_only := 0;
    pending := 0;
    missing := 0;
    FOR row_value IN EXECUTE format('SELECT to_jsonb(t) FROM enrichedlab_agent.%I t WHERE t.tenant_id = $1',
        meta.table_name) USING p_tenant LOOP
      rows_seen := rows_seen + 1;
      key := evidence.import_key(row_value, meta.key_columns, meta.column_types);
      FOREACH v_change IN ARRAY evidence.capture_changes_due(p_tenant, meta.table_name, row_value, key) LOOP
        IF EXISTS (SELECT 1 FROM evidence.capture_gaps g WHERE g.tenant_id = p_tenant
            AND g.idempotency_key = evidence.capture_idempotency_key(meta.table_name, v_change, key)) THEN
          gap_only := gap_only + 1;
        ELSIF EXISTS (SELECT 1 FROM evidence.capture_log l WHERE l.tenant_id = p_tenant
            AND l.table_name = meta.table_name AND l.change = v_change
            AND evidence.import_key(l.row_value, meta.key_columns, meta.column_types) = key) THEN
          pending := pending + 1;
        ELSE
          missing := missing + 1;
        END IF;
      END LOOP;
    END LOOP;
    tables := tables || jsonb_build_object(meta.table_name,
      jsonb_build_object('rows', rows_seen, 'gap_only', gap_only, 'pending', pending, 'missing', missing));
    total_missing := total_missing + missing;
    total_gaps := total_gaps + gap_only;
    total_pending := total_pending + pending;
  END LOOP;
  RETURN jsonb_build_object('missing', total_missing, 'gap_only', total_gaps, 'pending', total_pending,
    'tables', tables);
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_missing(text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (81) ON CONFLICT DO NOTHING;
COMMIT;
