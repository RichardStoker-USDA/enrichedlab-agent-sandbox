-- The research record's evidence schema, first part: who may append, the scope
-- every record carries, one hash chain per stream, an unpartitioned receipt per record that holds its sequence and its
-- idempotency key, and the records themselves partitioned by the month they were ingested.
--
-- A stream is the chain one record belongs to: a research run's, or a project's, a lab's or a standalone scope's when
-- there is no run. A producer is a registered identity: the broker's own database roles append as themselves, other
-- producers (OpenShell sandboxes, the platform, the knowledge pipeline, ClusterOps agents) only through an append
-- adapter role bound to them by name, never by naming another identity. A producer writes only
-- the labs, scopes, projects, runs and event types its grants list, and only broker producers write review,
-- validation or acceptance records. Source times before 1970 or more than a day ahead are refused.
--
-- evidence.append is the one write path. It locks the stream's head row, so appends to one stream serialise and take
-- consecutive sequences; an identical retry under the same idempotency key returns the original receipt and a
-- conflicting one is refused. Each record's hash covers the chain's previous hash, its scope, producer, event type,
-- source and ingestion times and the SHA-256 of its exact payload bytes (material version 1, below).
--
-- Reads go through evidence.verify_stream and (from 076) evidence.trace_activity. Both take the person the broker API
-- authenticated, as every broker function does: the API database role is trusted to name the signed-in person, and
-- the functions decide what that person may see (a person, read access to the project or membership of the lab, the
-- disclosure class, and a purpose that is recorded with the read).
--
-- Until a release carries 075 and 076, their layout may still change. The schema's comment names the layout; a
-- database that holds an earlier draft's layout and no receipts has its evidence schema rebuilt, and one with receipts
-- is refused. Counting every tenant's receipts needs a migration role that row security does not filter (the owner
-- with BYPASSRLS, or a superuser); any other role is refused before anything is dropped. Change the layout name with
-- any change to either file before it is published, never after.
--
-- Rollback: the coordinator reads none of this; the schema may stay. Dropping it loses the records.
BEGIN;

DO $$
DECLARE
  layout text;
  held bigint := 0;
BEGIN
  IF to_regnamespace('evidence') IS NOT NULL THEN
    layout := obj_description(to_regnamespace('evidence'), 'pg_namespace');
    IF layout IS DISTINCT FROM 'enrichedlab evidence layout 5'
      AND layout IS DISTINCT FROM 'enrichedlab evidence layout 6' THEN
      IF to_regclass('evidence.receipts') IS NOT NULL THEN
        -- Appends wait until this transaction ends, and the count covers every tenant: with row security off, a role
        -- that row security would filter gets an error instead of a short count, so the rebuild fails closed.
        BEGIN
          LOCK TABLE evidence.receipts IN ACCESS EXCLUSIVE MODE;
          PERFORM set_config('row_security', 'off', true);
          EXECUTE 'SELECT count(*) FROM evidence.receipts' INTO held;
          PERFORM set_config('row_security', 'on', true);
        EXCEPTION WHEN insufficient_privilege THEN
          RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:draft_layout_unverifiable',
            DETAIL = 'the migration role cannot count every receipt; run it as the schema owner with BYPASSRLS';
        END;
      END IF;
      IF held > 0 THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:draft_layout_holds_records',
          DETAIL = format('layout %s holds %s receipts', coalesce(layout, 'unnamed'), held);
      END IF;
      DROP SCHEMA evidence CASCADE;
    END IF;
  END IF;
END;
$$;

CREATE SCHEMA IF NOT EXISTS evidence;
REVOKE ALL ON SCHEMA evidence FROM PUBLIC;

-- Producers and the database roles that may act for them.
CREATE TABLE IF NOT EXISTS evidence.producers (
  tenant_id text NOT NULL REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  producer_id uuid NOT NULL DEFAULT uuidv7(),
  identity text NOT NULL CHECK (identity ~ '^[a-z][a-z0-9:/._-]{2,255}$'),
  kind text NOT NULL CHECK (kind IN ('broker', 'sandbox', 'platform', 'knowledge', 'clusterops')),
  -- A broker producer appends from this login role directly; every other producer has none and appends only through a
  -- role bound to it in evidence.adapter_bindings.
  database_role name,
  enabled boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, producer_id),
  UNIQUE (tenant_id, identity),
  UNIQUE (tenant_id, database_role),
  CHECK ((kind = 'broker') = (database_role IS NOT NULL))
);

-- The login roles that may name a non-broker producer. An append adapter authenticates the producer's workload
-- identity and then appends as that producer; each binding names one producer, so an adapter acts only for the
-- producers bound to it. Empty until the adapter exists.
CREATE TABLE IF NOT EXISTS evidence.adapter_bindings (
  tenant_id text NOT NULL,
  producer_id uuid NOT NULL,
  role_name name NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, producer_id, role_name),
  FOREIGN KEY (tenant_id, producer_id) REFERENCES evidence.producers (tenant_id, producer_id)
);

-- What a producer may write: a lab, a scope kind, optionally one project, and event types. A project-scope grant names
-- its project and covers one run (run_id), every run and the project's own records (all_runs), or, with neither, only
-- the project's records outside runs. Lab and standalone grants name no project or run.
CREATE TABLE IF NOT EXISTS evidence.producer_grants (
  tenant_id text NOT NULL,
  grant_id uuid NOT NULL DEFAULT uuidv7(),
  producer_id uuid NOT NULL,
  group_key text NOT NULL,
  scope_kind text NOT NULL CHECK (scope_kind IN ('project', 'lab', 'standalone')),
  project_id uuid,
  run_id uuid,
  all_runs boolean NOT NULL DEFAULT false,
  event_types text[] NOT NULL CHECK (cardinality(event_types) BETWEEN 1 AND 64),
  actor_role text NOT NULL CHECK (actor_role IN ('broker', 'tool', 'model', 'service', 'sandbox')),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, grant_id),
  UNIQUE NULLS NOT DISTINCT (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, all_runs),
  FOREIGN KEY (tenant_id, producer_id) REFERENCES evidence.producers (tenant_id, producer_id),
  FOREIGN KEY (tenant_id, group_key) REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects (tenant_id, project_id),
  FOREIGN KEY (tenant_id, run_id) REFERENCES enrichedlab_agent.broker_research_runs (tenant_id, run_id),
  CHECK ((scope_kind = 'project') = (project_id IS NOT NULL)),
  CHECK (NOT (all_runs AND run_id IS NOT NULL)),
  CHECK (scope_kind = 'project' OR (run_id IS NULL AND NOT all_runs))
);

-- One hash chain per stream. The head row is the lock appends take.
CREATE TABLE IF NOT EXISTS evidence.streams (
  tenant_id text NOT NULL,
  stream_id uuid NOT NULL DEFAULT uuidv7(),
  group_key text NOT NULL,
  scope_kind text NOT NULL CHECK (scope_kind IN ('project', 'lab', 'standalone')),
  project_id uuid,
  run_id uuid,
  hash_version smallint NOT NULL DEFAULT 1 CHECK (hash_version = 1),
  head_sequence bigint NOT NULL DEFAULT 0 CHECK (head_sequence >= 0),
  head_hash text NOT NULL DEFAULT repeat('0', 64) CHECK (head_hash ~ '^[0-9a-f]{64}$'),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, stream_id),
  UNIQUE NULLS NOT DISTINCT (tenant_id, group_key, scope_kind, project_id, run_id),
  FOREIGN KEY (tenant_id, group_key) REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects (tenant_id, project_id),
  FOREIGN KEY (tenant_id, run_id) REFERENCES enrichedlab_agent.broker_research_runs (tenant_id, run_id),
  CHECK ((scope_kind = 'project') = (project_id IS NOT NULL)),
  CHECK (run_id IS NULL OR scope_kind = 'project')
);

-- Every record's sequence, identity and idempotency key, unpartitioned so they stay unique across months.
CREATE TABLE IF NOT EXISTS evidence.receipts (
  tenant_id text NOT NULL,
  stream_id uuid NOT NULL,
  sequence bigint NOT NULL CHECK (sequence >= 1),
  record_id uuid NOT NULL,
  producer_id uuid NOT NULL,
  idempotency_key text NOT NULL CHECK (idempotency_key ~ '^[A-Za-z0-9._:-]{8,128}$'),
  request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  record_hash text NOT NULL CHECK (record_hash ~ '^[0-9a-f]{64}$'),
  ingested_at timestamptz NOT NULL,
  PRIMARY KEY (tenant_id, stream_id, sequence),
  UNIQUE (tenant_id, record_id),
  UNIQUE (tenant_id, producer_id, stream_id, idempotency_key),
  FOREIGN KEY (tenant_id, stream_id) REFERENCES evidence.streams (tenant_id, stream_id),
  FOREIGN KEY (tenant_id, producer_id) REFERENCES evidence.producers (tenant_id, producer_id)
);

-- The records, range-partitioned by ingestion month with the partition key in the primary key. Scope is repeated from
-- the stream so row security and queries need no join. The payload is the exact JSON bytes the producer hashed.
CREATE TABLE IF NOT EXISTS evidence.records (
  tenant_id text NOT NULL,
  stream_id uuid NOT NULL,
  sequence bigint NOT NULL,
  ingested_at timestamptz NOT NULL,
  record_id uuid NOT NULL,
  group_key text NOT NULL,
  scope_kind text NOT NULL,
  project_id uuid,
  run_id uuid,
  producer_id uuid NOT NULL,
  event_type text NOT NULL CHECK (event_type ~ '^[a-z][a-z0-9_]{1,31}(\.[a-z][a-z0-9_]{1,31}){0,3}$'),
  actor_role text NOT NULL,
  source_time timestamptz NOT NULL,
  payload text NOT NULL CHECK (octet_length(payload) BETWEEN 2 AND 65536),
  payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'),
  previous_hash text NOT NULL CHECK (previous_hash ~ '^[0-9a-f]{64}$'),
  record_hash text NOT NULL CHECK (record_hash ~ '^[0-9a-f]{64}$'),
  hash_version smallint NOT NULL,
  PRIMARY KEY (tenant_id, stream_id, sequence, ingested_at),
  FOREIGN KEY (tenant_id, stream_id, sequence) REFERENCES evidence.receipts (tenant_id, stream_id, sequence)
) PARTITION BY RANGE (ingested_at);

CREATE INDEX IF NOT EXISTS records_scope_time ON evidence.records (tenant_id, group_key, project_id, ingested_at);
CREATE INDEX IF NOT EXISTS records_run ON evidence.records (tenant_id, run_id, sequence) WHERE run_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS records_ingested_brin ON evidence.records USING brin (ingested_at);

-- Who read which stream, as whom, for what purpose, decided at which scope. Written by the read functions.
CREATE TABLE IF NOT EXISTS evidence.reads (
  tenant_id text NOT NULL,
  read_id uuid NOT NULL DEFAULT uuidv7(),
  read_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  actor_id uuid NOT NULL,
  stream_id uuid NOT NULL,
  reader text NOT NULL CHECK (reader IN ('verify_stream', 'trace_activity')),
  activity_key text,
  purpose text NOT NULL CHECK (purpose IN ('review', 'reproduce', 'audit')),
  scope text NOT NULL CHECK (scope IN ('audit', 'member', 'restricted')),
  PRIMARY KEY (tenant_id, read_id, read_at)
) PARTITION BY RANGE (read_at);

-- Monthly partitions: this creates the current month and the next p_months for each partitioned table, and returns the
-- ones it created. The migration creates six months ahead; the append service's scheduled maintenance keeps them ahead
-- after that. Calls are serialised by a transaction lock, and the migration owner owns every partition.
CREATE OR REPLACE FUNCTION evidence.ensure_record_partitions(p_months integer DEFAULT 2)
RETURNS text[] LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence AS $$
DECLARE
  month_start date;
  made text[] := '{}';
  name text;
  parent text;
BEGIN
  IF p_months IS NULL OR p_months NOT BETWEEN 0 AND 12 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:partition_months_invalid';
  END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('evidence.ensure_record_partitions', 0));
  FOREACH parent IN ARRAY ARRAY['records', 'reads'] LOOP
    FOR i IN 0..p_months LOOP
      month_start := (date_trunc('month', clock_timestamp() AT TIME ZONE 'UTC') + make_interval(months => i))::date;
      name := format('%s_y%sm%s', parent, to_char(month_start, 'YYYY'), to_char(month_start, 'MM'));
      IF to_regclass(format('evidence.%I', name)) IS NULL THEN
        EXECUTE format('CREATE TABLE evidence.%I PARTITION OF evidence.%I FOR VALUES FROM (%L) TO (%L)', name, parent,
          (month_start::timestamp AT TIME ZONE 'UTC'), ((month_start + interval '1 month')::timestamp AT TIME ZONE 'UTC'));
        EXECUTE format('ALTER TABLE evidence.%I ENABLE ROW LEVEL SECURITY', name);
        EXECUTE format('ALTER TABLE evidence.%I FORCE ROW LEVEL SECURITY', name);
        EXECUTE format('REVOKE ALL ON evidence.%I FROM PUBLIC', name);
        made := made || name;
      END IF;
    END LOOP;
  END LOOP;
  RETURN made;
END;
$$;
SELECT evidence.ensure_record_partitions(6);

-- Row security is the second line: every table is tenant-scoped by the setting the broker sets, and the functions below
-- set it to the tenant they act for. Runtime roles get no table privileges at all.
DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['producers', 'adapter_bindings', 'producer_grants', 'streams', 'receipts', 'records', 'reads']
  LOOP
    EXECUTE format('ALTER TABLE evidence.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE evidence.%I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON evidence.%I FROM PUBLIC', t);
    EXECUTE format('DROP POLICY IF EXISTS tenant_scope ON evidence.%I', t);
    EXECUTE format('CREATE POLICY tenant_scope ON evidence.%I USING (tenant_id = current_setting(%L, true)) '
      'WITH CHECK (tenant_id = current_setting(%L, true))', t, 'enrichedlab.tenant_id', 'enrichedlab.tenant_id');
  END LOOP;
END;
$$;

-- Receipts and records never change once written, for any role. Disposal will be a separate,
-- audited path, not an update or delete through these tables.
CREATE OR REPLACE FUNCTION evidence.reject_mutation() RETURNS trigger LANGUAGE plpgsql
SET search_path = pg_catalog AS $$
BEGIN
  RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:append_only';
END;
$$;
REVOKE ALL ON FUNCTION evidence.reject_mutation() FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON evidence.receipts;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON evidence.receipts
  FOR EACH ROW EXECUTE FUNCTION evidence.reject_mutation();
DROP TRIGGER IF EXISTS immutable_record ON evidence.records;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON evidence.records
  FOR EACH ROW EXECUTE FUNCTION evidence.reject_mutation();
DROP TRIGGER IF EXISTS no_truncate ON evidence.receipts;
CREATE TRIGGER no_truncate BEFORE TRUNCATE ON evidence.receipts
  FOR EACH STATEMENT EXECUTE FUNCTION evidence.reject_mutation();
DROP TRIGGER IF EXISTS no_truncate ON evidence.records;
CREATE TRIGGER no_truncate BEFORE TRUNCATE ON evidence.records
  FOR EACH STATEMENT EXECUTE FUNCTION evidence.reject_mutation();
DROP TRIGGER IF EXISTS immutable_record ON evidence.reads;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON evidence.reads
  FOR EACH ROW EXECUTE FUNCTION evidence.reject_mutation();
DROP TRIGGER IF EXISTS no_truncate ON evidence.reads;
CREATE TRIGGER no_truncate BEFORE TRUNCATE ON evidence.reads
  FOR EACH STATEMENT EXECUTE FUNCTION evidence.reject_mutation();

-- Hash material version 1: these fields, in this order, each as text, joined by a line feed (no field can contain one:
-- identifiers and event types are constrained, times are canonical UTC with microseconds, digests are hex), encoded as
-- UTF-8 and hashed with SHA-256.
--   enrichedlab.evidence.record.v1, tenant_id, stream_id, sequence, record_id, group_key, scope_kind, project_id or '-',
--   run_id or '-', producer identity, event_type, actor_role, source_time, ingested_at, payload_sha256, previous_hash
CREATE OR REPLACE FUNCTION evidence.utc_text(p timestamptz) RETURNS text LANGUAGE sql IMMUTABLE
SET search_path = pg_catalog AS $$
  SELECT to_char(p AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')
$$;

CREATE OR REPLACE FUNCTION evidence.record_material_v1(
  p_tenant text, p_stream uuid, p_sequence bigint, p_record uuid, p_group text, p_scope_kind text, p_project uuid,
  p_run uuid, p_producer_identity text, p_event_type text, p_actor_role text, p_source_time timestamptz,
  p_ingested_at timestamptz, p_payload_sha256 text, p_previous_hash text)
RETURNS text LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, evidence AS $$
  SELECT concat_ws(E'\n', 'enrichedlab.evidence.record.v1', p_tenant, p_stream::text, p_sequence::text,
    p_record::text, p_group, p_scope_kind, coalesce(p_project::text, '-'), coalesce(p_run::text, '-'),
    p_producer_identity, p_event_type, p_actor_role, evidence.utc_text(p_source_time), evidence.utc_text(p_ingested_at),
    p_payload_sha256, p_previous_hash)
$$;

-- The one write path. Returns the receipt: record_id, stream_id, sequence, record_hash, ingested_at and replayed (true
-- when an identical earlier request is answered again). The SET clause restores the caller's tenant setting on return.
-- Only on a schema no later migration has laid out: migrations run again on every migrate, and a later one
-- (076, 079) replaces this function.
DO $guard$
BEGIN
  IF obj_description(to_regnamespace('evidence'), 'pg_namespace') IS NULL THEN
    EXECUTE $def$
CREATE OR REPLACE FUNCTION evidence.append(
  p_tenant text, p_producer text, p_group text, p_scope_kind text, p_project uuid, p_run uuid, p_event_type text,
  p_idempotency_key text, p_source_time timestamptz, p_payload text, p_payload_sha256 text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  producer evidence.producers%ROWTYPE;
  role_name text;
  stream evidence.streams%ROWTYPE;
  held evidence.receipts%ROWTYPE;
  request text;
  next_sequence bigint;
  new_record uuid := uuidv7();
  now_at timestamptz := clock_timestamp();
  material text;
  new_hash text;
  granted_role text;
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
  role_name := session_user;
  IF producer.database_role IS NOT NULL THEN
    IF producer.database_role <> role_name THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_not_caller';
    END IF;
  ELSIF NOT EXISTS (SELECT 1 FROM evidence.adapter_bindings b WHERE b.tenant_id = p_tenant
      AND b.producer_id = producer.producer_id AND b.role_name = session_user) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_not_caller';
  END IF;
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

  -- The stream's head row serialises appends to it.
  INSERT INTO evidence.streams (tenant_id, group_key, scope_kind, project_id, run_id)
    VALUES (p_tenant, p_group, p_scope_kind, p_project, p_run) ON CONFLICT DO NOTHING;
  SELECT * INTO stream FROM evidence.streams s
    WHERE s.tenant_id = p_tenant AND s.group_key = p_group AND s.scope_kind = p_scope_kind
      AND s.project_id IS NOT DISTINCT FROM p_project AND s.run_id IS NOT DISTINCT FROM p_run
    FOR UPDATE;

  request := encode(sha256(convert_to(concat_ws(E'\n', 'enrichedlab.evidence.request.v1', p_tenant, p_producer, p_group,
    p_scope_kind, coalesce(p_project::text, '-'), coalesce(p_run::text, '-'), p_event_type,
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

  next_sequence := stream.head_sequence + 1;
  material := evidence.record_material_v1(p_tenant, stream.stream_id, next_sequence, new_record, p_group, p_scope_kind,
    p_project, p_run, p_producer, p_event_type, granted_role, p_source_time, now_at, p_payload_sha256, stream.head_hash);
  new_hash := encode(sha256(convert_to(material, 'UTF8')), 'hex');

  INSERT INTO evidence.receipts (tenant_id, stream_id, sequence, record_id, producer_id, idempotency_key, request_sha256,
      record_hash, ingested_at)
    VALUES (p_tenant, stream.stream_id, next_sequence, new_record, producer.producer_id, p_idempotency_key, request,
      new_hash, now_at);
  INSERT INTO evidence.records (tenant_id, stream_id, sequence, ingested_at, record_id, group_key, scope_kind,
      project_id, run_id, producer_id, event_type, actor_role, source_time, payload, payload_sha256, previous_hash,
      record_hash, hash_version)
    VALUES (p_tenant, stream.stream_id, next_sequence, now_at, new_record, p_group, p_scope_kind, p_project, p_run,
      producer.producer_id, p_event_type, granted_role, p_source_time, p_payload, p_payload_sha256, stream.head_hash,
      new_hash, 1);
  UPDATE evidence.streams SET head_sequence = next_sequence, head_hash = new_hash, updated_at = now_at
    WHERE tenant_id = p_tenant AND stream_id = stream.stream_id;

  RETURN jsonb_build_object('record_id', new_record, 'stream_id', stream.stream_id, 'sequence', next_sequence,
    'record_hash', new_hash, 'ingested_at', evidence.utc_text(now_at), 'replayed', false);
END;
$$
$def$;
  END IF;
END;
$guard$;

-- What a person may read in a stream's scope, for a purpose: NULL (nothing), 'audit' (an administrator's time-limited
-- project inspection, for the audit purpose only), 'member' (the project or lab without restricted entities) or
-- 'restricted' (also restricted entities: the project's owner or a person it was granted to, or a lab lead for a
-- lab-level stream). Only enabled people of an enabled tenant qualify; service accounts and campaigns read nothing here.
-- Uses the broker's own project decisions, so evidence reads follow the same rules as the rest of the broker.
CREATE OR REPLACE FUNCTION evidence.read_scope(p_tenant text, p_actor uuid, p_group text, p_project uuid, p_purpose text)
RETURNS text LANGUAGE plpgsql STABLE SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
DECLARE
  project_scope text;
  member_role text;
BEGIN
  IF p_actor IS NULL OR p_purpose IS NULL OR p_purpose NOT IN ('review', 'reproduce', 'audit') THEN
    RETURN NULL;
  END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_tenants t WHERE t.tenant_id = p_tenant AND t.enabled;
  IF NOT FOUND THEN RETURN NULL; END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_principals a
    WHERE a.tenant_id = p_tenant AND a.principal_id = p_actor AND a.enabled AND a.principal_class = 'person';
  IF NOT FOUND THEN RETURN NULL; END IF;
  IF p_project IS NOT NULL THEN
    project_scope := enrichedlab_agent.broker_project_package_scope(p_tenant, p_actor, p_project);
    IF project_scope IS NULL THEN RETURN NULL; END IF;
    IF project_scope = 'inspection' THEN
      RETURN CASE WHEN p_purpose = 'audit' THEN 'audit' END;
    END IF;
    IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_projects p
        WHERE p.tenant_id = p_tenant AND p.project_id = p_project AND p.owner_id = p_actor)
      OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_grants g
        WHERE g.tenant_id = p_tenant AND g.project_id = p_project AND g.principal_id = p_actor
          AND cardinality(g.capabilities) > 0) THEN
      RETURN 'restricted';
    END IF;
    RETURN 'member';
  END IF;
  SELECT m.role INTO member_role FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_research_groups g
      ON g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND g.enabled
    WHERE m.tenant_id = p_tenant AND m.group_key = p_group AND m.principal_id = p_actor;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN CASE WHEN member_role = 'lead' THEN 'restricted' ELSE 'member' END;
END;
$$;

-- Recompute a stream's chain from its records: true only when every sequence from 1 to the head is present once, each
-- record's previous hash is its predecessor's hash and each hash matches its material. The answer holds counts and
-- hashes only. A stream the person may not read is reported as unknown; every answered read is recorded.
CREATE OR REPLACE FUNCTION evidence.verify_stream(p_tenant text, p_actor uuid, p_stream uuid, p_purpose text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  head evidence.streams%ROWTYPE;
  r record;
  expected bigint := 1;
  previous text := repeat('0', 64);
  recomputed text;
  scope text;
BEGIN
  IF p_purpose IS NULL OR p_purpose NOT IN ('review', 'reproduce', 'audit') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:purpose_invalid';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO head FROM evidence.streams WHERE tenant_id = p_tenant AND stream_id = p_stream;
  IF FOUND THEN
    scope := evidence.read_scope(p_tenant, p_actor, head.group_key, head.project_id, p_purpose);
  END IF;
  IF scope IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:stream_unknown';
  END IF;
  INSERT INTO evidence.reads (tenant_id, actor_id, stream_id, reader, purpose, scope)
    VALUES (p_tenant, p_actor, p_stream, 'verify_stream', p_purpose, scope);
  -- Records past the head read above belong to appends that committed during this call; the chain is checked up to
  -- that head, so a concurrent append cannot make a sound stream look broken.
  FOR r IN SELECT rec.*, p.identity FROM evidence.records rec
      JOIN evidence.producers p ON p.tenant_id = rec.tenant_id AND p.producer_id = rec.producer_id
      WHERE rec.tenant_id = p_tenant AND rec.stream_id = p_stream AND rec.sequence <= head.head_sequence
      ORDER BY rec.sequence LOOP
    IF r.sequence <> expected OR r.previous_hash <> previous THEN
      RETURN jsonb_build_object('valid', false, 'at_sequence', r.sequence, 'reason', 'chain_break');
    END IF;
    recomputed := encode(sha256(convert_to(evidence.record_material_v1(r.tenant_id, r.stream_id, r.sequence,
      r.record_id, r.group_key, r.scope_kind, r.project_id, r.run_id, r.identity, r.event_type, r.actor_role,
      r.source_time, r.ingested_at, r.payload_sha256, r.previous_hash), 'UTF8')), 'hex');
    IF recomputed <> r.record_hash OR encode(sha256(convert_to(r.payload, 'UTF8')), 'hex') <> r.payload_sha256 THEN
      RETURN jsonb_build_object('valid', false, 'at_sequence', r.sequence, 'reason', 'hash_mismatch');
    END IF;
    previous := r.record_hash;
    expected := expected + 1;
  END LOOP;
  IF expected - 1 <> head.head_sequence OR previous <> head.head_hash THEN
    RETURN jsonb_build_object('valid', false, 'at_sequence', expected, 'reason', 'head_mismatch');
  END IF;
  RETURN jsonb_build_object('valid', true, 'records', head.head_sequence, 'head_hash', head.head_hash);
END;
$$;

REVOKE ALL ON FUNCTION evidence.ensure_record_partitions(integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.utc_text(timestamptz) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.record_material_v1(text, uuid, bigint, uuid, text, text, uuid, uuid, text, text, text,
  timestamptz, timestamptz, text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.append(text, text, text, text, uuid, uuid, text, text, timestamptz, text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.read_scope(text, uuid, text, uuid, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION evidence.verify_stream(text, uuid, uuid, text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (75) ON CONFLICT DO NOTHING;
COMMIT;
