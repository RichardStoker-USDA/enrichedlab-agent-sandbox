-- Object custody for the research record: new bytes a producer captures (model prompts, answers, tool calls and
-- reasoning; artifacts) live as content-addressed objects in their lab's two object-store copies, and the record holds
-- only their digest, size, class and reference.
--
-- The custody service (the evidence append service) is the only writer. For a producer it has authenticated it asks
-- evidence.custody_authorise for the lab's two stores, hashes the bytes the producer sends, writes and reads back the
-- primary copy, and records an immutable claim (evidence.custody_issue_claim) bound to that producer and scope. When
-- the replica holds the same version, read back in full, and both copies' retention covers the claim's floor, it
-- appends the claim's `ready` event (evidence.custody_mark_ready). A record binds a claim inside evidence.append:
-- an entity names it with `claim`, a model call names one per role; the reference row is written in the record's own
-- transaction, so a reference exists exactly when the row that uses it committed.
--
-- Every custody row is immutable. States are events: a claim with no `ready` event is waiting; a copy is current until
-- a `copy_missing` or `copy_deleted` event names it; retention extensions, holds, releases and deletion steps are
-- events too. Each row records the database epoch (its WAL timeline), so a restore or a failover, which starts a new
-- timeline, is visible to the service and to garbage collection.
--
-- Rules for producers other than the broker's own roles: an entity carrying any byte field (`sha256`, `size`,
-- `media_type`, `uri`, `object_version`) is refused; bytes are named only through a ready claim, and the server fills
-- digest, size, media type, location and version from it. Such producers may not declare imported or external
-- entities. Only an `inference` producer (the gateway every research model call passes through) may record a model
-- call; it names a claim for the prompt and the completion, and for tool calls and reasoning either a claim or
-- `*_not_returned: true`; its decoding and cache members are restricted to listed metadata.
--
-- Closed event schemas: a producer other than the broker's own roles appends only event types its tenant registered a
-- schema for (evidence.register_event_schema, owner only), and every payload member other than `lineage` (which
-- evidence.write_lineage checks) must match it: no member the schema does not name, every string within its bound and
-- pattern, no control characters. Text reaches the record only where a schema allows it. The receipt names the schema
-- version and digest it was checked against.
--
-- Retention: a run's objects keep at least 90 days (the buckets' governance default). A project accepted under an
-- approved records class needs that class's period on both copies; binding derives the floor at bind time while holding
-- the project's custody state row FOR SHARE, and acceptance takes the same row FOR UPDATE, so no bind interleaves with
-- an acceptance. Acceptance of a project holding references needs an approved records class for its lab, and appends a
-- `retention_requested` event for every reference in the project. The acceptance stays pending until the custody
-- service has extended and read back both copies of every one of them (`acceptance_completed`,
-- evidence.custody_acceptance_state).
--
-- Layout 6. The append service accepts layout 5 (append only) and 6 (append and custody), so the service release can
-- precede this migration. Rollback: restore 076's evidence.append and evidence.write_lineage (kept in that file) and
-- the layout comment; the custody tables may stay, and dropping them loses custody facts.
BEGIN;

DO $$
BEGIN
  IF obj_description(to_regnamespace('evidence'), 'pg_namespace') IS DISTINCT FROM 'enrichedlab evidence layout 5'
    AND obj_description(to_regnamespace('evidence'), 'pg_namespace') IS DISTINCT FROM 'enrichedlab evidence layout 6' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:layout_unexpected';
  END IF;
END;
$$;

-- The gateway every research model call passes through records the calls.
ALTER TABLE evidence.producers DROP CONSTRAINT IF EXISTS producers_kind_check;
ALTER TABLE evidence.producers ADD CONSTRAINT producers_kind_check
  CHECK (kind IN ('broker', 'sandbox', 'platform', 'knowledge', 'clusterops', 'inference'));

-- The database epoch: the WAL timeline of the primary, as eight hex digits. A restore or a promotion starts a new one.
CREATE OR REPLACE FUNCTION evidence.database_epoch() RETURNS text LANGUAGE sql VOLATILE
SET search_path = pg_catalog AS $$
  SELECT substr(pg_walfile_name(pg_current_wal_lsn()), 1, 8)
$$;
REVOKE ALL ON FUNCTION evidence.database_epoch() FROM PUBLIC;

-- A lab's bucket name, derived from tenant and lab and never from request text at use.
CREATE OR REPLACE FUNCTION evidence.custody_bucket_name(p_tenant text, p_group text) RETURNS text LANGUAGE sql IMMUTABLE
SET search_path = pg_catalog AS $$
  SELECT 'ev-' || left(encode(sha256(convert_to(p_tenant || E'\n' || p_group, 'UTF8')), 'hex'), 20)
$$;
REVOKE ALL ON FUNCTION evidence.custody_bucket_name(text, text) FROM PUBLIC;

-- Each lab's two copies. Only the migration owner registers them (evidence.custody_register_store); the service maps
-- the endpoint name to its address and credentials in its private configuration.
CREATE TABLE IF NOT EXISTS evidence.custody_stores (
  tenant_id text NOT NULL,
  group_key text NOT NULL,
  copy_role text NOT NULL CHECK (copy_role IN ('primary', 'replica')),
  bucket text NOT NULL CHECK (bucket ~ '^ev-[0-9a-f]{20}$'),
  endpoint_name text NOT NULL CHECK (endpoint_name ~ '^[a-z][a-z0-9-]{1,62}$'),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  epoch text NOT NULL,
  PRIMARY KEY (tenant_id, group_key, copy_role),
  UNIQUE (bucket, copy_role),
  FOREIGN KEY (tenant_id, group_key) REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  CHECK (bucket = evidence.custody_bucket_name(tenant_id, group_key))
);

-- Records classes a person approved for a lab: owner, period, review cadence, disposition.
CREATE TABLE IF NOT EXISTS evidence.custody_classes (
  tenant_id text NOT NULL,
  class_id uuid NOT NULL DEFAULT uuidv7(),
  group_key text NOT NULL,
  period interval NOT NULL CHECK (period > interval '90 days' AND period <= interval '100 years'),
  owner_id uuid NOT NULL,
  review_cadence interval NOT NULL CHECK (review_cadence BETWEEN interval '30 days' AND interval '10 years'),
  disposition text NOT NULL CHECK (length(disposition) BETWEEN 1 AND 512 AND disposition ~ '\S'
    AND disposition !~ '[[:cntrl:]]'),
  approved_by uuid NOT NULL,
  approved_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  epoch text NOT NULL,
  PRIMARY KEY (tenant_id, class_id),
  FOREIGN KEY (tenant_id, group_key) REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key)
);

-- One row per stored content (tenant, lab, SHA-256 of the uncompressed bytes).
CREATE TABLE IF NOT EXISTS evidence.custody_objects (
  tenant_id text NOT NULL,
  object_id uuid NOT NULL DEFAULT uuidv7(),
  group_key text NOT NULL,
  raw_sha256 text NOT NULL CHECK (raw_sha256 ~ '^[0-9a-f]{64}$'),
  raw_size bigint NOT NULL CHECK (raw_size BETWEEN 0 AND 67108864),
  object_key text NOT NULL,
  encoding text NOT NULL CHECK (encoding = 'zstd-v1'),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  epoch text NOT NULL,
  PRIMARY KEY (tenant_id, object_id),
  UNIQUE (tenant_id, group_key, raw_sha256),
  FOREIGN KEY (tenant_id, group_key) REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  CHECK (object_key = 'sha256/' || left(raw_sha256, 2) || '/' || raw_sha256 || '.zst')
);

-- What one copy held when it was read back in full. Never updated; a later verification is a new row.
CREATE TABLE IF NOT EXISTS evidence.custody_copies (
  tenant_id text NOT NULL,
  copy_id uuid NOT NULL DEFAULT uuidv7(),
  object_id uuid NOT NULL,
  copy_role text NOT NULL CHECK (copy_role IN ('primary', 'replica')),
  bucket text NOT NULL,
  version_id text NOT NULL CHECK (version_id ~ '^[A-Za-z0-9._-]+$' AND length(version_id) <= 256),
  stored_size bigint NOT NULL CHECK (stored_size BETWEEN 1 AND 67108864),
  stored_sha256 text NOT NULL CHECK (stored_sha256 ~ '^[0-9a-f]{64}$'),
  retention_mode text CHECK (retention_mode IN ('GOVERNANCE', 'COMPLIANCE')),
  retain_until timestamptz,
  legal_hold boolean NOT NULL,
  verified_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  epoch text NOT NULL,
  PRIMARY KEY (tenant_id, copy_id),
  FOREIGN KEY (tenant_id, object_id) REFERENCES evidence.custody_objects (tenant_id, object_id),
  CHECK ((retention_mode IS NULL) = (retain_until IS NULL))
);
CREATE INDEX IF NOT EXISTS custody_copies_object ON evidence.custody_copies (tenant_id, object_id, copy_role);

-- A claim: the service hashed this producer's bytes in one call and verified the primary copy. Binds only in its own
-- producer and scope, only after its `ready` event, and only before it expires.
CREATE TABLE IF NOT EXISTS evidence.custody_claims (
  tenant_id text NOT NULL,
  claim_id uuid NOT NULL DEFAULT uuidv7(),
  producer_id uuid NOT NULL,
  group_key text NOT NULL,
  scope_kind text NOT NULL CHECK (scope_kind IN ('project', 'lab', 'standalone')),
  project_id uuid,
  run_id uuid,
  object_id uuid NOT NULL,
  media_type text NOT NULL CHECK (media_type ~ '^[a-z0-9.+-]+/[A-Za-z0-9.+-]+$' AND length(media_type) <= 128),
  idempotency_key text NOT NULL CHECK (idempotency_key ~ '^[A-Za-z0-9._:-]{8,128}$'),
  request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  primary_copy uuid NOT NULL,
  class text NOT NULL CHECK (class IN ('run', 'records')),
  floor_until timestamptz NOT NULL,
  issued_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  expires_at timestamptz NOT NULL,
  epoch text NOT NULL,
  PRIMARY KEY (tenant_id, claim_id),
  UNIQUE (tenant_id, producer_id, idempotency_key),
  FOREIGN KEY (tenant_id, producer_id) REFERENCES evidence.producers (tenant_id, producer_id),
  FOREIGN KEY (tenant_id, object_id) REFERENCES evidence.custody_objects (tenant_id, object_id),
  FOREIGN KEY (tenant_id, primary_copy) REFERENCES evidence.custody_copies (tenant_id, copy_id),
  CHECK ((scope_kind = 'project') = (project_id IS NOT NULL)),
  CHECK (run_id IS NULL OR scope_kind = 'project'),
  CHECK (expires_at > issued_at AND expires_at <= issued_at + interval '1 day')
);

CREATE TABLE IF NOT EXISTS evidence.custody_claim_events (
  tenant_id text NOT NULL,
  event_id uuid NOT NULL DEFAULT uuidv7(),
  claim_id uuid NOT NULL,
  kind text NOT NULL CHECK (kind = 'ready'),
  primary_copy uuid NOT NULL,
  replica_copy uuid NOT NULL,
  covered_until timestamptz NOT NULL,
  at timestamptz NOT NULL DEFAULT clock_timestamp(),
  epoch text NOT NULL,
  PRIMARY KEY (tenant_id, event_id),
  UNIQUE (tenant_id, claim_id, kind),
  FOREIGN KEY (tenant_id, claim_id) REFERENCES evidence.custody_claims (tenant_id, claim_id),
  FOREIGN KEY (tenant_id, primary_copy) REFERENCES evidence.custody_copies (tenant_id, copy_id),
  FOREIGN KEY (tenant_id, replica_copy) REFERENCES evidence.custody_copies (tenant_id, copy_id)
);

-- The row that used bytes, the claim it bound and the exact copies, in the record's transaction. Never deleted.
CREATE TABLE IF NOT EXISTS evidence.custody_references (
  tenant_id text NOT NULL,
  reference_id uuid NOT NULL DEFAULT uuidv7(),
  claim_id uuid NOT NULL,
  object_id uuid NOT NULL,
  record_id uuid NOT NULL,
  ingested_at timestamptz NOT NULL,
  role text NOT NULL CHECK (role IN ('entity', 'prompt', 'completion', 'tool_calls', 'reasoning')),
  entity_id uuid,
  group_key text NOT NULL,
  project_id uuid,
  class text NOT NULL CHECK (class IN ('run', 'records')),
  floor_until timestamptz NOT NULL,
  primary_copy uuid NOT NULL,
  replica_copy uuid NOT NULL,
  bound_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  epoch text NOT NULL,
  PRIMARY KEY (tenant_id, reference_id),
  UNIQUE (tenant_id, record_id, role, claim_id),
  FOREIGN KEY (tenant_id, claim_id) REFERENCES evidence.custody_claims (tenant_id, claim_id),
  FOREIGN KEY (tenant_id, object_id) REFERENCES evidence.custody_objects (tenant_id, object_id),
  FOREIGN KEY (tenant_id, record_id) REFERENCES evidence.receipts (tenant_id, record_id),
  FOREIGN KEY (tenant_id, entity_id) REFERENCES evidence.entities (tenant_id, entity_id),
  CHECK ((role = 'entity') = (entity_id IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS custody_references_object ON evidence.custody_references (tenant_id, object_id);
CREATE INDEX IF NOT EXISTS custody_references_project ON evidence.custody_references (tenant_id, project_id)
  WHERE project_id IS NOT NULL;

-- Lifecycle facts about objects, copies and references.
CREATE TABLE IF NOT EXISTS evidence.custody_events (
  tenant_id text NOT NULL,
  event_id uuid NOT NULL DEFAULT uuidv7(),
  object_id uuid,
  copy_id uuid,
  reference_id uuid,
  kind text NOT NULL,
  detail jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(detail) = 'object' AND octet_length(detail::text) <= 2048),
  actor text NOT NULL CHECK (actor ~ '^[a-z][a-z0-9:/._-]{2,127}$'),
  at timestamptz NOT NULL DEFAULT clock_timestamp(),
  epoch text NOT NULL,
  PRIMARY KEY (tenant_id, event_id),
  FOREIGN KEY (tenant_id, object_id) REFERENCES evidence.custody_objects (tenant_id, object_id),
  FOREIGN KEY (tenant_id, copy_id) REFERENCES evidence.custody_copies (tenant_id, copy_id),
  FOREIGN KEY (tenant_id, reference_id) REFERENCES evidence.custody_references (tenant_id, reference_id)
);
-- The event kinds and which need an object are replaced on every run, so a rerun over an earlier layout 6 table ends
-- with exactly these two checks (an earlier one could refuse kinds the functions below write). Every row an earlier
-- check admitted satisfies these.
DO $$
DECLARE c record;
BEGIN
  FOR c IN SELECT conname FROM pg_constraint WHERE conrelid = 'evidence.custody_events'::regclass AND contype = 'c'
      AND pg_get_constraintdef(oid) ~ '\mkind\M' LOOP
    EXECUTE format('ALTER TABLE evidence.custody_events DROP CONSTRAINT %I', c.conname);
  END LOOP;
END;
$$;
ALTER TABLE evidence.custody_events ADD CONSTRAINT custody_events_kind_allowed CHECK (kind IN ('copy_missing',
  'retention_requested', 'retention_applied', 'retention_failed', 'hold_placed', 'hold_released', 'reference_released',
  'delete_intent', 'delete_started', 'copy_deleted', 'epoch_observed', 'acceptance_completed'));
ALTER TABLE evidence.custody_events ADD CONSTRAINT custody_events_object_required
  CHECK (kind IN ('epoch_observed', 'acceptance_completed') OR object_id IS NOT NULL);
CREATE INDEX IF NOT EXISTS custody_events_copy ON evidence.custody_events (tenant_id, copy_id) WHERE copy_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS custody_events_object ON evidence.custody_events (tenant_id, object_id, kind);

-- One row per project with custody: the lock binding (FOR SHARE) and acceptance (FOR UPDATE) share. Never updated.
CREATE TABLE IF NOT EXISTS evidence.custody_project_states (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, project_id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects (tenant_id, project_id)
);

CREATE TABLE IF NOT EXISTS evidence.custody_project_acceptances (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  class_id uuid NOT NULL,
  record_id uuid NOT NULL,
  accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  epoch text NOT NULL,
  PRIMARY KEY (tenant_id, project_id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES evidence.custody_project_states (tenant_id, project_id),
  FOREIGN KEY (tenant_id, class_id) REFERENCES evidence.custody_classes (tenant_id, class_id),
  FOREIGN KEY (tenant_id, record_id) REFERENCES evidence.receipts (tenant_id, record_id)
);

DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['custody_stores', 'custody_classes', 'custody_objects', 'custody_copies', 'custody_claims',
      'custody_claim_events', 'custody_references', 'custody_events', 'custody_project_states',
      'custody_project_acceptances'] LOOP
    EXECUTE format('ALTER TABLE evidence.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE evidence.%I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON evidence.%I FROM PUBLIC', t);
    EXECUTE format('DROP POLICY IF EXISTS tenant_scope ON evidence.%I', t);
    EXECUTE format('CREATE POLICY tenant_scope ON evidence.%I USING (tenant_id = current_setting(%L, true)) '
      'WITH CHECK (tenant_id = current_setting(%L, true))', t, 'enrichedlab.tenant_id', 'enrichedlab.tenant_id');
    EXECUTE format('DROP TRIGGER IF EXISTS immutable_record ON evidence.%I', t);
    EXECUTE format('CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON evidence.%I '
      'FOR EACH ROW EXECUTE FUNCTION evidence.reject_mutation()', t);
    EXECUTE format('DROP TRIGGER IF EXISTS no_truncate ON evidence.%I', t);
    EXECUTE format('CREATE TRIGGER no_truncate BEFORE TRUNCATE ON evidence.%I '
      'FOR EACH STATEMENT EXECUTE FUNCTION evidence.reject_mutation()', t);
  END LOOP;
END;
$$;

-- Whether a JSON text names a member twice in any object. jsonb keeps only the last of duplicate members while the
-- record keeps the text, so a duplicate would let text the schema never saw into the record.
CREATE OR REPLACE FUNCTION evidence.json_duplicate_members(p json) RETURNS boolean LANGUAGE plpgsql IMMUTABLE
SET search_path = pg_catalog, evidence AS $$
BEGIN
  CASE json_typeof(p)
    WHEN 'object' THEN
      IF (SELECT count(*) <> count(DISTINCT e.key) FROM json_each(p) e) THEN RETURN true; END IF;
      RETURN EXISTS (SELECT 1 FROM json_each(p) e WHERE evidence.json_duplicate_members(e.value));
    WHEN 'array' THEN
      RETURN EXISTS (SELECT 1 FROM json_array_elements(p) e WHERE evidence.json_duplicate_members(e));
    ELSE
      RETURN false;
  END CASE;
END;
$$;

-- Whether every string in a JSON value (keys of maps included) is a token: letters, digits and ._:/@+=- only, at
-- most 256 characters. An untrusted producer's lineage may carry no other text.
CREATE OR REPLACE FUNCTION evidence.tokens_only(p jsonb) RETURNS boolean LANGUAGE plpgsql IMMUTABLE
SET search_path = pg_catalog, evidence AS $$
BEGIN
  CASE jsonb_typeof(p)
    WHEN 'string' THEN
      RETURN (p #>> '{}') ~ '^[A-Za-z0-9._:/@+=-]+$' AND length(p #>> '{}') <= 256;
    WHEN 'object' THEN
      RETURN NOT EXISTS (SELECT 1 FROM jsonb_each(p) e
        WHERE e.key !~ '^[A-Za-z0-9._:/@+=-]+$' OR length(e.key) > 64 OR NOT evidence.tokens_only(e.value));
    WHEN 'array' THEN
      RETURN NOT EXISTS (SELECT 1 FROM jsonb_array_elements(p) e WHERE NOT evidence.tokens_only(e));
    ELSE
      RETURN true;
  END CASE;
END;
$$;

-- The shape a payload member must have (a small closed subset of JSON Schema): string (max, pattern), number
-- (integer), boolean, array (items, max), object (properties, required; or a map: keys, key_max, values, max; or open,
-- which only an owner-registered qualification schema uses), enum, and nullable on any node.
CREATE OR REPLACE FUNCTION evidence.shape_ok(p_value jsonb, p_node jsonb, p_depth integer DEFAULT 0)
RETURNS boolean LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, evidence AS $$
DECLARE
  t text := jsonb_typeof(p_value);
BEGIN
  IF p_depth > 16 OR p_node IS NULL OR jsonb_typeof(p_node) <> 'object' THEN RETURN false; END IF;
  IF t = 'null' THEN RETURN coalesce((p_node->>'nullable')::boolean, false); END IF;
  IF p_node ? 'enum' THEN RETURN p_node->'enum' @> jsonb_build_array(p_value); END IF;
  CASE p_node->>'type'
    WHEN 'string' THEN
      IF t <> 'string' OR length(p_value #>> '{}') > coalesce((p_node->>'max')::int, 256)
        OR (p_value #>> '{}') ~ '[[:cntrl:]]' THEN
        RETURN false;
      END IF;
      RETURN NOT (p_node ? 'pattern') OR (p_value #>> '{}') ~ (p_node->>'pattern');
    WHEN 'number' THEN
      RETURN t = 'number' AND (NOT coalesce((p_node->>'integer')::boolean, false)
        OR p_value::numeric = trunc(p_value::numeric));
    WHEN 'boolean' THEN
      RETURN t = 'boolean';
    WHEN 'array' THEN
      IF t <> 'array' OR jsonb_array_length(p_value) > coalesce((p_node->>'max')::int, 256) THEN RETURN false; END IF;
      RETURN NOT EXISTS (SELECT 1 FROM jsonb_array_elements(p_value) e
        WHERE NOT evidence.shape_ok(e, p_node->'items', p_depth + 1));
    WHEN 'object' THEN
      IF t <> 'object' THEN RETURN false; END IF;
      IF coalesce((p_node->>'open')::boolean, false) THEN RETURN true; END IF;
      IF p_node ? 'keys' THEN
        IF (SELECT count(*) FROM jsonb_object_keys(p_value)) > coalesce((p_node->>'max')::int, 64) THEN
          RETURN false;
        END IF;
        RETURN NOT EXISTS (SELECT 1 FROM jsonb_each(p_value) e WHERE e.key !~ (p_node->>'keys')
          OR length(e.key) > coalesce((p_node->>'key_max')::int, 128)
          OR NOT evidence.shape_ok(e.value, p_node->'values', p_depth + 1));
      END IF;
      IF EXISTS (SELECT 1 FROM jsonb_array_elements_text(coalesce(p_node->'required', '[]')) r
          WHERE NOT p_value ? r) THEN
        RETURN false;
      END IF;
      RETURN NOT EXISTS (SELECT 1 FROM jsonb_each(p_value) e
        WHERE NOT (coalesce(p_node->'properties', '{}') ? e.key)
          OR NOT evidence.shape_ok(e.value, p_node->'properties'->e.key, p_depth + 1));
    ELSE
      RETURN false;
  END CASE;
EXCEPTION
  WHEN invalid_regular_expression OR invalid_text_representation OR numeric_value_out_of_range THEN
    RETURN false;
END;
$$;

-- The schemas a tenant's untrusted producers' event types must match, versioned; the newest version applies.
CREATE TABLE IF NOT EXISTS evidence.event_schemas (
  tenant_id text NOT NULL REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  event_type text NOT NULL CHECK (event_type ~ '^[a-z][a-z0-9_]{1,31}(\.[a-z][a-z0-9_]{1,31}){0,3}$'),
  version integer NOT NULL CHECK (version >= 1),
  schema jsonb NOT NULL CHECK (jsonb_typeof(schema) = 'object' AND octet_length(schema::text) <= 65536),
  schema_sha256 text NOT NULL CHECK (schema_sha256 ~ '^[0-9a-f]{64}$'),
  registered_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, event_type, version)
);
ALTER TABLE evidence.event_schemas ENABLE ROW LEVEL SECURITY;
ALTER TABLE evidence.event_schemas FORCE ROW LEVEL SECURITY;
REVOKE ALL ON evidence.event_schemas FROM PUBLIC;
DROP POLICY IF EXISTS tenant_scope ON evidence.event_schemas;
CREATE POLICY tenant_scope ON evidence.event_schemas USING (tenant_id = current_setting('enrichedlab.tenant_id', true))
  WITH CHECK (tenant_id = current_setting('enrichedlab.tenant_id', true));
DROP TRIGGER IF EXISTS immutable_record ON evidence.event_schemas;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON evidence.event_schemas
  FOR EACH ROW EXECUTE FUNCTION evidence.reject_mutation();
DROP TRIGGER IF EXISTS no_truncate ON evidence.event_schemas;
CREATE TRIGGER no_truncate BEFORE TRUNCATE ON evidence.event_schemas
  FOR EACH STATEMENT EXECUTE FUNCTION evidence.reject_mutation();
ALTER TABLE evidence.receipts ADD COLUMN IF NOT EXISTS payload_schema text
  CHECK (payload_schema IS NULL OR payload_schema ~ '^[a-z0-9_.]+@[0-9]+:[0-9a-f]{64}$');

-- Registers a schema for a tenant's event type (owner only; no grant). The same schema again returns its version.
CREATE OR REPLACE FUNCTION evidence.register_event_schema(p_tenant text, p_event_type text, p_schema jsonb)
RETURNS text LANGUAGE plpgsql SET search_path = pg_catalog, evidence AS $$
DECLARE
  digest text := encode(sha256(convert_to(p_schema::text, 'UTF8')), 'hex');
  held evidence.event_schemas%ROWTYPE;
BEGIN
  IF p_schema IS NULL OR jsonb_typeof(p_schema) <> 'object' OR p_schema->>'type' IS DISTINCT FROM 'object' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:event_schema_invalid';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  PERFORM pg_advisory_xact_lock(hashtextextended('evidence.register_event_schema:' || p_tenant || ':' || p_event_type, 0));
  SELECT * INTO held FROM evidence.event_schemas s WHERE s.tenant_id = p_tenant AND s.event_type = p_event_type
    ORDER BY s.version DESC LIMIT 1;
  IF FOUND AND held.schema_sha256 = digest THEN
    RETURN p_event_type || '@' || held.version || ':' || digest;
  END IF;
  INSERT INTO evidence.event_schemas (tenant_id, event_type, version, schema, schema_sha256)
    VALUES (p_tenant, p_event_type, coalesce(held.version, 0) + 1, p_schema, digest);
  RETURN p_event_type || '@' || (coalesce(held.version, 0) + 1) || ':' || digest;
END;
$$;

-- Producers already appending keep working: a tenant with an untrusted producer granted `sandbox.receipt` and no
-- schema for it yet gets the reviewed receipt schema (schemas/evidence/sandbox.receipt.v1.json, embedded between the
-- markers below and kept equal to that file by a test) in this transaction. A tenant that already has one keeps its own.
-- The producer and grant tables force row security even for their owner, so the tenants come from the broker's own
-- list and each tenant's producers are read with that tenant set; run as the migration owner, a single query over all
-- tenants finds no row.
DO $$
DECLARE t text;
BEGIN
  FOR t IN SELECT b.tenant_id FROM enrichedlab_agent.broker_tenants b ORDER BY b.tenant_id LOOP
    PERFORM set_config('enrichedlab.tenant_id', t, true);
    CONTINUE WHEN NOT EXISTS (SELECT 1 FROM evidence.producer_grants g
        JOIN evidence.producers p ON p.tenant_id = g.tenant_id AND p.producer_id = g.producer_id
        WHERE g.tenant_id = t AND p.kind <> 'broker' AND 'sandbox.receipt' = ANY (g.event_types))
      OR EXISTS (SELECT 1 FROM evidence.event_schemas s WHERE s.tenant_id = t AND s.event_type = 'sandbox.receipt');
    PERFORM evidence.register_event_schema(t, 'sandbox.receipt',
      -- sandbox.receipt.v1:start
      $receipt${"properties":{"ending":{"nullable":true,"properties":{"deletion_timestamp":{"max":40,"nullable":true,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"},"observation":{"max":32,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"observed_at":{"max":40,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"}},"type":"object"},"first_seen_at":{"max":40,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"},"gateway":{"max":128,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"images_omitted":{"integer":true,"type":"number"},"images_seen":{"items":{"max":256,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"max":64,"type":"array"},"last_seen_at":{"max":40,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"},"not_recorded":{"properties":{"deleted_by":{"enum":["the caller of the delete is in the gateway's audit log; Kubernetes shows only the gateway"]},"main_process_exit":{"enum":["the gateway keeps the main process's exit status in its own store, which this service does not read"]},"ocsf_events":{"enum":["the supervisor's OCSF events are counted elsewhere, not kept; join those counts by the supervisor Pod's name and UID"]},"output_checksums":{"enum":["workspace contents are not read; outputs are recorded where they are collected"]},"policy":{"enum":["the sandbox was created without a policy of its own; the gateway's default applied, which the committed sandbox does not carry"]},"policy_revisions":{"enum":["a policy change's committed response does not name its sandbox, so changes after the start are not recorded"]},"prover_result":{"enum":["the gateway's prover checks policy proposals and keeps its verdicts in its own store"]},"start":{"enum":["the gateway's commit of this sandbox was not seen"]}},"type":"object"},"pods":{"items":{"properties":{"component":{"max":63,"nullable":true,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"containers":{"items":{"properties":{"exit_code":{"integer":true,"nullable":true,"type":"number"},"image":{"max":256,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"image_id":{"max":256,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"init":{"type":"boolean"},"name":{"max":63,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"reason":{"max":64,"nullable":true,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"restart_count":{"integer":true,"type":"number"},"state":{"max":32,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"}},"type":"object"},"max":32,"type":"array"},"containers_omitted":{"integer":true,"type":"number"},"created_at":{"max":40,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"},"name":{"max":253,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"node":{"max":253,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"phase":{"max":32,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"runtime_class_name":{"max":63,"nullable":true,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"uid":{"max":36,"pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$","type":"string"}},"type":"object"},"max":64,"type":"array"},"pods_omitted":{"integer":true,"type":"number"},"sandbox":{"properties":{"created_at":{"max":40,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"},"deletion_timestamp":{"max":40,"nullable":true,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"},"generation":{"integer":true,"type":"number"},"labels":{"properties":{"entries":{"key_max":317,"keys":"^[A-Za-z0-9./_-]+$","max":64,"type":"object","values":{"max":63,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"}},"omitted":{"integer":true,"type":"number"}},"type":"object"},"name":{"max":253,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"namespace":{"max":63,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"object_name":{"max":253,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"pair":{"max":253,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"sandbox_id":{"max":36,"pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$","type":"string"},"uid":{"max":36,"pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$","type":"string"},"workspace":{"max":63,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"}},"type":"object"},"sandbox_id":{"max":36,"pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$","type":"string"},"schema":{"max":64,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"start":{"nullable":true,"properties":{"annotations":{"properties":{"entries":{"key_max":317,"keys":"^[A-Za-z0-9./_-]+$","max":64,"type":"object","values":{"max":512,"pattern":"^[A-Za-z0-9._:/@+=-]*…?$","type":"string"}},"omitted":{"integer":true,"type":"number"}},"type":"object"},"command_arguments":{"integer":true,"type":"number"},"created_time":{"max":40,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"},"environment_names":{"items":{"max":128,"pattern":"^[A-Za-z_][A-Za-z0-9_]*$","type":"string"},"max":256,"type":"array"},"labels":{"properties":{"entries":{"key_max":317,"keys":"^[A-Za-z0-9./_-]+$","max":64,"type":"object","values":{"max":63,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"}},"omitted":{"integer":true,"type":"number"}},"type":"object"},"log_level":{"max":32,"nullable":true,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"name":{"max":253,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"pods_unheld":{"type":"boolean"},"policy_hash":{"max":64,"nullable":true,"pattern":"^[0-9a-f]{64}$","type":"string"},"principal":{"properties":{"entries":{"key_max":317,"keys":"^[A-Za-z0-9./_-]+$","max":64,"type":"object","values":{"max":128,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"}},"omitted":{"integer":true,"type":"number"}},"type":"object"},"program":{"max":256,"nullable":true,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"providers":{"items":{"max":128,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"max":64,"type":"array"},"recorded_at":{"max":40,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"},"sandbox_id":{"max":36,"pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$","type":"string"},"schema":{"max":64,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"template":{"properties":{"annotations":{"properties":{"entries":{"key_max":317,"keys":"^[A-Za-z0-9./_-]+$","max":64,"type":"object","values":{"max":512,"pattern":"^[A-Za-z0-9._:/@+=-]*…?$","type":"string"}},"omitted":{"integer":true,"type":"number"}},"type":"object"},"environment_names":{"items":{"max":128,"pattern":"^[A-Za-z_][A-Za-z0-9_]*$","type":"string"},"max":256,"type":"array"},"image":{"max":256,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"labels":{"properties":{"entries":{"key_max":317,"keys":"^[A-Za-z0-9./_-]+$","max":64,"type":"object","values":{"max":63,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"}},"omitted":{"integer":true,"type":"number"}},"type":"object"},"runtime_class_name":{"max":63,"nullable":true,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"},"user_namespaces":{"nullable":true,"type":"boolean"}},"type":"object"},"workspace":{"max":63,"pattern":"^[A-Za-z0-9._:/@+=-]*$","type":"string"}},"type":"object"},"written_at":{"max":40,"pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$","type":"string"}},"required":["schema","sandbox_id","sandbox","gateway","pods","pods_omitted","images_seen","images_omitted","first_seen_at","last_seen_at","not_recorded","written_at"],"type":"object"}$receipt$
      -- sandbox.receipt.v1:end
      ::jsonb);
  END LOOP;
  PERFORM set_config('enrichedlab.tenant_id', '', true);
END;
$$;

-- The producer a caller may act as, by the same rule evidence.append applies (its own role, or an adapter bound to it).
CREATE OR REPLACE FUNCTION evidence.custody_caller_producer(p_tenant text, p_producer text)
RETURNS evidence.producers LANGUAGE plpgsql STABLE SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
DECLARE
  producer evidence.producers%ROWTYPE;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_tenants t WHERE t.tenant_id = p_tenant AND t.enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:tenant_disabled'; END IF;
  SELECT * INTO producer FROM evidence.producers WHERE tenant_id = p_tenant AND identity = p_producer AND enabled;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_unknown'; END IF;
  IF producer.database_role IS NOT NULL THEN
    IF producer.database_role <> session_user THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_not_caller';
    END IF;
  ELSIF NOT EXISTS (SELECT 1 FROM evidence.adapter_bindings b WHERE b.tenant_id = p_tenant
      AND b.producer_id = producer.producer_id AND b.role_name = session_user) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:producer_not_caller';
  END IF;
  RETURN producer;
END;
$$;

-- A scope must be real and consistent, and the producer must hold a grant for it that lists `custody.store`.
CREATE OR REPLACE FUNCTION evidence.custody_scope_granted(p_tenant text, p_producer uuid, p_group text,
  p_scope_kind text, p_project uuid, p_run uuid)
RETURNS void LANGUAGE plpgsql STABLE SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
BEGIN
  IF p_scope_kind IS NULL OR p_scope_kind NOT IN ('project', 'lab', 'standalone')
    OR (p_scope_kind = 'project') <> (p_project IS NOT NULL) OR (p_run IS NOT NULL AND p_scope_kind <> 'project') THEN
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
  PERFORM 1 FROM evidence.producer_grants pg
    WHERE pg.tenant_id = p_tenant AND pg.producer_id = p_producer AND pg.group_key = p_group
      AND pg.scope_kind = p_scope_kind AND pg.project_id IS NOT DISTINCT FROM p_project
      AND (pg.run_id = p_run OR (pg.run_id IS NULL AND (p_run IS NULL OR pg.all_runs)))
      AND 'custody.store' = ANY (pg.event_types);
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:grant_missing'; END IF;
END;
$$;

-- The retention floor for a scope now: a records class's period for an accepted project, else 90 days.
CREATE OR REPLACE FUNCTION evidence.custody_floor(p_tenant text, p_project uuid, p_from timestamptz)
RETURNS TABLE (class text, floor_until timestamptz) LANGUAGE sql STABLE
SET search_path = pg_catalog, evidence AS $$
  SELECT coalesce(max('records'::text), 'run'), coalesce(max(p_from + c.period), p_from + interval '90 days')
    FROM evidence.custody_project_acceptances a
    JOIN evidence.custody_classes c ON c.tenant_id = a.tenant_id AND c.class_id = a.class_id
    WHERE a.tenant_id = p_tenant AND a.project_id = p_project
$$;

-- For the custody service, before it touches a store: the lab's two stores and the scope's retention floor.
CREATE OR REPLACE FUNCTION evidence.custody_authorise(p_tenant text, p_producer text, p_group text, p_scope_kind text,
  p_project uuid, p_run uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  producer evidence.producers%ROWTYPE;
  stores jsonb;
  fl record;
BEGIN
  IF p_tenant IS NULL OR p_producer IS NULL OR p_group IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:argument_missing';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  producer := evidence.custody_caller_producer(p_tenant, p_producer);
  PERFORM evidence.custody_scope_granted(p_tenant, producer.producer_id, p_group, p_scope_kind, p_project, p_run);
  SELECT jsonb_object_agg(s.copy_role, jsonb_build_object('bucket', s.bucket, 'endpoint', s.endpoint_name))
    INTO stores FROM evidence.custody_stores s WHERE s.tenant_id = p_tenant AND s.group_key = p_group;
  IF stores IS NULL OR NOT (stores ? 'primary' AND stores ? 'replica') THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_store_missing';
  END IF;
  SELECT * INTO fl FROM evidence.custody_floor(p_tenant, p_project, clock_timestamp());
  RETURN jsonb_build_object('primary', stores->'primary', 'replica', stores->'replica', 'class', fl.class,
    'floor_until', evidence.utc_text(fl.floor_until), 'epoch', evidence.database_epoch());
END;
$$;

-- A copy read back in full. A second row for a version must agree with the first on its stored bytes.
CREATE OR REPLACE FUNCTION evidence.custody_record_copy(p_tenant text, p_object uuid, p_copy_role text,
  p_version text, p_stored_size bigint, p_stored_sha256 text, p_retention_mode text, p_retain_until timestamptz,
  p_legal_hold boolean)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  obj evidence.custody_objects%ROWTYPE;
  store evidence.custody_stores%ROWTYPE;
  new_copy uuid;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO obj FROM evidence.custody_objects WHERE tenant_id = p_tenant AND object_id = p_object;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_object_unknown'; END IF;
  SELECT * INTO store FROM evidence.custody_stores
    WHERE tenant_id = p_tenant AND group_key = obj.group_key AND copy_role = p_copy_role;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_store_missing'; END IF;
  IF EXISTS (SELECT 1 FROM evidence.custody_copies c WHERE c.tenant_id = p_tenant AND c.object_id = p_object
      AND c.version_id = p_version AND (c.stored_sha256 <> p_stored_sha256 OR c.stored_size <> p_stored_size)) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_copy_conflict';
  END IF;
  INSERT INTO evidence.custody_copies (tenant_id, object_id, copy_role, bucket, version_id, stored_size, stored_sha256,
      retention_mode, retain_until, legal_hold, epoch)
    VALUES (p_tenant, p_object, p_copy_role, store.bucket, p_version, p_stored_size, p_stored_sha256,
      p_retention_mode, p_retain_until, coalesce(p_legal_hold, false), evidence.database_epoch())
    RETURNING copy_id INTO new_copy;
  RETURN new_copy;
EXCEPTION
  WHEN check_violation OR not_null_violation THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_copy_invalid';
END;
$$;

-- A claim for bytes the service hashed from this producer in one call, with the primary copy it wrote and read back.
-- An identical retry under the same idempotency key returns the original claim; a different one is refused.
CREATE OR REPLACE FUNCTION evidence.custody_issue_claim(p_tenant text, p_producer text, p_group text,
  p_scope_kind text, p_project uuid, p_run uuid, p_idempotency_key text, p_raw_sha256 text, p_raw_size bigint,
  p_media_type text, p_primary_version text, p_stored_size bigint, p_stored_sha256 text, p_retention_mode text,
  p_retain_until timestamptz, p_legal_hold boolean)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  producer evidence.producers%ROWTYPE;
  request text;
  held evidence.custody_claims%ROWTYPE;
  obj evidence.custody_objects%ROWTYPE;
  copy uuid;
  fl record;
  new_claim uuid;
  now_at timestamptz := clock_timestamp();
BEGIN
  IF p_tenant IS NULL OR p_producer IS NULL OR p_group IS NULL OR p_idempotency_key IS NULL OR p_raw_sha256 IS NULL
    OR p_raw_size IS NULL OR p_media_type IS NULL OR p_primary_version IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:argument_missing';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  producer := evidence.custody_caller_producer(p_tenant, p_producer);
  PERFORM evidence.custody_scope_granted(p_tenant, producer.producer_id, p_group, p_scope_kind, p_project, p_run);
  request := encode(sha256(convert_to(concat_ws(E'\n', 'enrichedlab.evidence.custody.claim.v1', p_tenant, p_producer,
    p_group, p_scope_kind, coalesce(p_project::text, '-'), coalesce(p_run::text, '-'), p_raw_sha256,
    p_raw_size::text, p_media_type), 'UTF8')), 'hex');
  SELECT * INTO held FROM evidence.custody_claims c
    WHERE c.tenant_id = p_tenant AND c.producer_id = producer.producer_id AND c.idempotency_key = p_idempotency_key;
  IF FOUND THEN
    IF held.request_sha256 <> request THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'evidence:idempotency_conflict';
    END IF;
    RETURN evidence.custody_claim_answer(p_tenant, held.claim_id, true);
  END IF;
  INSERT INTO evidence.custody_objects (tenant_id, group_key, raw_sha256, raw_size, object_key, encoding, epoch)
    VALUES (p_tenant, p_group, p_raw_sha256, p_raw_size,
      'sha256/' || left(p_raw_sha256, 2) || '/' || p_raw_sha256 || '.zst', 'zstd-v1', evidence.database_epoch())
    ON CONFLICT DO NOTHING;
  SELECT * INTO obj FROM evidence.custody_objects o
    WHERE o.tenant_id = p_tenant AND o.group_key = p_group AND o.raw_sha256 = p_raw_sha256;
  IF obj.raw_size <> p_raw_size THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_size_conflict';
  END IF;
  IF EXISTS (SELECT 1 FROM evidence.custody_events e WHERE e.tenant_id = p_tenant AND e.object_id = obj.object_id
      AND e.kind = 'delete_intent') THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_object_closing';
  END IF;
  copy := evidence.custody_record_copy(p_tenant, obj.object_id, 'primary', p_primary_version, p_stored_size,
    p_stored_sha256, p_retention_mode, p_retain_until, p_legal_hold);
  SELECT * INTO fl FROM evidence.custody_floor(p_tenant, p_project, now_at);
  BEGIN
    INSERT INTO evidence.custody_claims (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, object_id,
        media_type, idempotency_key, request_sha256, primary_copy, class, floor_until, issued_at, expires_at, epoch)
      VALUES (p_tenant, producer.producer_id, p_group, p_scope_kind, p_project, p_run, obj.object_id, p_media_type,
        p_idempotency_key, request, copy, fl.class, fl.floor_until, now_at, now_at + interval '1 day',
        evidence.database_epoch())
      RETURNING claim_id INTO new_claim;
  EXCEPTION WHEN unique_violation THEN
    -- A concurrent identical call issued it first: answer that claim, or refuse a different request.
    SELECT * INTO held FROM evidence.custody_claims c
      WHERE c.tenant_id = p_tenant AND c.producer_id = producer.producer_id AND c.idempotency_key = p_idempotency_key;
    IF held.request_sha256 IS DISTINCT FROM request THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'evidence:idempotency_conflict';
    END IF;
    RETURN evidence.custody_claim_answer(p_tenant, held.claim_id, true);
  END;
  RETURN evidence.custody_claim_answer(p_tenant, new_claim, false);
EXCEPTION
  WHEN check_violation THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_claim_invalid';
END;
$$;

-- A claim as the service answers it: ids, the object's key and the stores, the floor, whether it is ready, and the
-- primary copy's version.
CREATE OR REPLACE FUNCTION evidence.custody_claim_answer(p_tenant text, p_claim uuid, p_replayed boolean)
RETURNS jsonb LANGUAGE sql STABLE SET search_path = pg_catalog, evidence AS $$
  SELECT jsonb_build_object('claim_id', c.claim_id, 'object_id', c.object_id, 'object_key', o.object_key,
      'raw_sha256', o.raw_sha256, 'raw_size', o.raw_size, 'class', c.class,
      'floor_until', evidence.utc_text(c.floor_until), 'expires_at', evidence.utc_text(c.expires_at),
      'primary_version', p.version_id, 'stored_sha256', p.stored_sha256, 'stored_size', p.stored_size,
      'ready', EXISTS (SELECT 1 FROM evidence.custody_claim_events e WHERE e.tenant_id = c.tenant_id
        AND e.claim_id = c.claim_id AND e.kind = 'ready'),
      'replayed', p_replayed)
    FROM evidence.custody_claims c
    JOIN evidence.custody_objects o ON o.tenant_id = c.tenant_id AND o.object_id = c.object_id
    JOIN evidence.custody_copies p ON p.tenant_id = c.tenant_id AND p.copy_id = c.primary_copy
    WHERE c.tenant_id = p_tenant AND c.claim_id = p_claim
$$;

-- Whether a copy is current: no copy_missing or copy_deleted event names any verification of the same physical
-- version (object, copy role, version id). A failed re-read recorded against a later verification row invalidates
-- the rows claims and references pinned too.
CREATE OR REPLACE FUNCTION evidence.custody_copy_current(p_tenant text, p_copy uuid) RETURNS boolean LANGUAGE sql STABLE
SET search_path = pg_catalog, evidence AS $$
  SELECT NOT EXISTS (SELECT 1 FROM evidence.custody_copies me
    JOIN evidence.custody_copies same ON same.tenant_id = me.tenant_id AND same.object_id = me.object_id
      AND same.copy_role = me.copy_role AND same.version_id = me.version_id
    JOIN evidence.custody_events e ON e.tenant_id = same.tenant_id AND e.copy_id = same.copy_id
      AND e.kind IN ('copy_missing', 'copy_deleted')
    WHERE me.tenant_id = p_tenant AND me.copy_id = p_copy)
$$;

-- The claim's `ready` event, once the replica copy (same version and stored bytes) is recorded and both copies'
-- retention covers the claim's floor. Idempotent.
CREATE OR REPLACE FUNCTION evidence.custody_mark_ready(p_tenant text, p_producer text, p_claim uuid,
  p_replica_copy uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  producer evidence.producers%ROWTYPE;
  claim evidence.custody_claims%ROWTYPE;
  p evidence.custody_copies%ROWTYPE;
  r evidence.custody_copies%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  producer := evidence.custody_caller_producer(p_tenant, p_producer);
  SELECT * INTO claim FROM evidence.custody_claims c
    WHERE c.tenant_id = p_tenant AND c.claim_id = p_claim AND c.producer_id = producer.producer_id;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_claim_unknown'; END IF;
  IF EXISTS (SELECT 1 FROM evidence.custody_claim_events e WHERE e.tenant_id = p_tenant AND e.claim_id = p_claim
      AND e.kind = 'ready') THEN
    RETURN evidence.custody_claim_answer(p_tenant, p_claim, true);
  END IF;
  IF claim.expires_at <= clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_claim_expired';
  END IF;
  SELECT * INTO p FROM evidence.custody_copies WHERE tenant_id = p_tenant AND copy_id = claim.primary_copy;
  SELECT * INTO r FROM evidence.custody_copies WHERE tenant_id = p_tenant AND copy_id = p_replica_copy;
  IF r.copy_id IS NULL OR r.object_id <> claim.object_id OR r.copy_role <> 'replica' OR r.version_id <> p.version_id
    OR r.stored_sha256 <> p.stored_sha256 OR r.stored_size <> p.stored_size THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_replica_mismatch';
  END IF;
  IF NOT (evidence.custody_copy_current(p_tenant, p.copy_id) AND evidence.custody_copy_current(p_tenant, r.copy_id)) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_copy_missing';
  END IF;
  IF p.retain_until IS NULL OR r.retain_until IS NULL OR p.retain_until < claim.floor_until
    OR r.retain_until < claim.floor_until THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_retention_short';
  END IF;
  INSERT INTO evidence.custody_claim_events (tenant_id, claim_id, kind, primary_copy, replica_copy, covered_until, epoch)
    VALUES (p_tenant, p_claim, 'ready', p.copy_id, r.copy_id, least(p.retain_until, r.retain_until),
      evidence.database_epoch());
  RETURN evidence.custody_claim_answer(p_tenant, p_claim, false);
END;
$$;

-- A claim the producer named, for AwaitClaim and retries: only the producer's own claims are answered.
CREATE OR REPLACE FUNCTION evidence.custody_claim(p_tenant text, p_producer text, p_claim uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  producer evidence.producers%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  producer := evidence.custody_caller_producer(p_tenant, p_producer);
  PERFORM 1 FROM evidence.custody_claims c
    WHERE c.tenant_id = p_tenant AND c.claim_id = p_claim AND c.producer_id = producer.producer_id;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_claim_unknown'; END IF;
  RETURN evidence.custody_claim_answer(p_tenant, p_claim, false);
END;
$$;

-- Claims still waiting for their replica, for the reconciler and AwaitClaim: unexpired, of producers bound to the
-- calling role, oldest first, at most p_limit; p_claim narrows it to one claim. Each row names the replica to read.
DROP FUNCTION IF EXISTS evidence.custody_waiting(text, integer);
CREATE OR REPLACE FUNCTION evidence.custody_waiting(p_tenant text, p_limit integer, p_claim uuid DEFAULT NULL)
RETURNS TABLE (claim_id uuid, producer text, object_id uuid, object_key text, raw_sha256 text, raw_size bigint,
  primary_version text, stored_sha256 text, stored_size bigint, floor_until timestamptz, replica_bucket text,
  replica_endpoint text)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  RETURN QUERY SELECT c.claim_id, pr.identity, c.object_id, o.object_key, o.raw_sha256, o.raw_size, p.version_id,
      p.stored_sha256, p.stored_size, c.floor_until, st.bucket, st.endpoint_name
    FROM evidence.custody_claims c
    JOIN evidence.producers pr ON pr.tenant_id = c.tenant_id AND pr.producer_id = c.producer_id
    JOIN evidence.custody_objects o ON o.tenant_id = c.tenant_id AND o.object_id = c.object_id
    JOIN evidence.custody_copies p ON p.tenant_id = c.tenant_id AND p.copy_id = c.primary_copy
    JOIN evidence.custody_stores st ON st.tenant_id = c.tenant_id AND st.group_key = c.group_key
      AND st.copy_role = 'replica'
    WHERE c.tenant_id = p_tenant AND c.expires_at > clock_timestamp()
      AND (p_claim IS NULL OR c.claim_id = p_claim)
      AND pr.identity = ANY (evidence.adapter_producers(p_tenant))
      AND NOT EXISTS (SELECT 1 FROM evidence.custody_claim_events e WHERE e.tenant_id = c.tenant_id
        AND e.claim_id = c.claim_id AND e.kind = 'ready')
    ORDER BY c.issued_at LIMIT greatest(1, least(coalesce(p_limit, 100), 1000));
END;
$$;

-- Copies due for a full re-read: the newest verification of each referenced object's copy, when it is older than
-- p_age and no copy_missing names it, at most p_limit, oldest first.
CREATE OR REPLACE FUNCTION evidence.custody_due_copies(p_tenant text, p_age interval, p_limit integer)
RETURNS TABLE (object_id uuid, copy_id uuid, copy_role text, bucket text, endpoint text, object_key text,
  raw_sha256 text, raw_size bigint, version_id text, stored_sha256 text, stored_size bigint, verified_at timestamptz)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  RETURN QUERY SELECT latest.object_id, latest.copy_id, latest.copy_role, latest.bucket, st.endpoint_name,
      o.object_key, o.raw_sha256,
      o.raw_size, latest.version_id, latest.stored_sha256, latest.stored_size, latest.verified_at
    FROM (SELECT DISTINCT ON (c.object_id, c.copy_role, c.version_id) c.*
            FROM evidence.custody_copies c
            WHERE c.tenant_id = p_tenant AND EXISTS (SELECT 1 FROM evidence.custody_references r
              WHERE r.tenant_id = c.tenant_id AND r.object_id = c.object_id)
            ORDER BY c.object_id, c.copy_role, c.version_id, c.verified_at DESC) latest
    JOIN evidence.custody_objects o ON o.tenant_id = p_tenant AND o.object_id = latest.object_id
    JOIN evidence.custody_stores st ON st.tenant_id = p_tenant AND st.group_key = o.group_key
      AND st.copy_role = latest.copy_role
    WHERE latest.verified_at < clock_timestamp() - p_age
      AND evidence.custody_copy_current(p_tenant, latest.copy_id)
    ORDER BY latest.verified_at LIMIT greatest(1, least(coalesce(p_limit, 50), 500));
END;
$$;

-- Retention the acceptance of a project asked for and no copy has confirmed: each reference's two pinned copies with
-- the date asked, at most p_limit, oldest request first.
CREATE OR REPLACE FUNCTION evidence.custody_retention_pending(p_tenant text, p_limit integer)
RETURNS TABLE (event_id uuid, object_id uuid, copy_id uuid, copy_role text, bucket text, endpoint text,
  object_key text, version_id text, until timestamptz)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  RETURN QUERY SELECT q.event_id, q.object_id, c.copy_id, c.copy_role, c.bucket, st.endpoint_name, o.object_key,
      c.version_id, (q.detail->>'until')::timestamptz
    FROM evidence.custody_events q
    JOIN evidence.custody_references r ON r.tenant_id = q.tenant_id AND r.reference_id = q.reference_id
    JOIN evidence.custody_copies c ON c.tenant_id = q.tenant_id AND c.copy_id IN (r.primary_copy, r.replica_copy)
    JOIN evidence.custody_objects o ON o.tenant_id = q.tenant_id AND o.object_id = q.object_id
    JOIN evidence.custody_stores st ON st.tenant_id = q.tenant_id AND st.group_key = o.group_key
      AND st.copy_role = c.copy_role
    WHERE q.tenant_id = p_tenant AND q.kind = 'retention_requested'
      AND NOT EXISTS (SELECT 1 FROM evidence.custody_events a WHERE a.tenant_id = q.tenant_id
        AND a.kind = 'retention_applied' AND a.copy_id = c.copy_id
        AND (a.detail->>'retain_until')::timestamptz >= (q.detail->>'until')::timestamptz)
    ORDER BY (SELECT max(f.at) FROM evidence.custody_events f WHERE f.tenant_id = q.tenant_id
        AND f.kind = 'retention_failed' AND f.copy_id = c.copy_id) NULLS FIRST, q.at
    LIMIT greatest(1, least(coalesce(p_limit, 50), 500));
END;
$$;

-- Completes the acceptances whose retention is in place: every reference of the project has, for both pinned copies,
-- a `retention_applied` at or after the date its `retention_requested` asked. Appends `acceptance_completed` once per
-- project and returns how many it completed. Called by the custody service's retention worker.
CREATE OR REPLACE FUNCTION evidence.custody_complete_acceptances(p_tenant text)
RETURNS integer LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  a record;
  done integer := 0;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  FOR a IN SELECT acc.project_id FROM evidence.custody_project_acceptances acc
      WHERE acc.tenant_id = p_tenant AND NOT EXISTS (SELECT 1 FROM evidence.custody_events e
        WHERE e.tenant_id = p_tenant AND e.kind = 'acceptance_completed' AND e.detail->>'project_id' = acc.project_id::text)
  LOOP
    IF NOT EXISTS (
        SELECT 1 FROM evidence.custody_events q
        JOIN evidence.custody_references r ON r.tenant_id = q.tenant_id AND r.reference_id = q.reference_id
        JOIN evidence.custody_copies c ON c.tenant_id = q.tenant_id AND c.copy_id IN (r.primary_copy, r.replica_copy)
        WHERE q.tenant_id = p_tenant AND q.kind = 'retention_requested' AND r.project_id = a.project_id
          AND NOT EXISTS (SELECT 1 FROM evidence.custody_events d WHERE d.tenant_id = q.tenant_id
            AND d.kind = 'retention_applied' AND d.copy_id = c.copy_id
            AND (d.detail->>'retain_until')::timestamptz >= (q.detail->>'until')::timestamptz)) THEN
      PERFORM pg_advisory_xact_lock(hashtextextended('evidence.custody_complete:' || a.project_id::text, 0));
      IF NOT EXISTS (SELECT 1 FROM evidence.custody_events e WHERE e.tenant_id = p_tenant
          AND e.kind = 'acceptance_completed' AND e.detail->>'project_id' = a.project_id::text) THEN
        INSERT INTO evidence.custody_events (tenant_id, kind, detail, actor, epoch)
          VALUES (p_tenant, 'acceptance_completed', jsonb_build_object('project_id', a.project_id),
            'service:custody', evidence.database_epoch());
        done := done + 1;
      END IF;
    END IF;
  END LOOP;
  RETURN done;
END;
$$;

-- A project's acceptance as people see it: none, pending (retention not yet in place on every copy) or complete.
CREATE OR REPLACE FUNCTION evidence.custody_acceptance_state(p_tenant text, p_project uuid)
RETURNS text LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF NOT EXISTS (SELECT 1 FROM evidence.custody_project_acceptances a WHERE a.tenant_id = p_tenant
      AND a.project_id = p_project) THEN
    RETURN 'none';
  END IF;
  RETURN CASE WHEN EXISTS (SELECT 1 FROM evidence.custody_events e WHERE e.tenant_id = p_tenant
      AND e.kind = 'acceptance_completed' AND e.detail->>'project_id' = p_project::text) THEN 'complete' ELSE 'pending' END;
END;
$$;

-- What the service observed about a copy it holds: missing on read back, retention extended, or a new epoch.
CREATE OR REPLACE FUNCTION evidence.custody_observe(p_tenant text, p_object uuid, p_copy uuid, p_kind text,
  p_detail jsonb)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  new_event uuid;
BEGIN
  IF p_kind NOT IN ('copy_missing', 'retention_applied', 'retention_failed') THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:custody_event_reserved';
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF p_copy IS NOT NULL AND NOT EXISTS (SELECT 1 FROM evidence.custody_copies c WHERE c.tenant_id = p_tenant
      AND c.copy_id = p_copy AND c.object_id = p_object) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_copy_unknown';
  END IF;
  INSERT INTO evidence.custody_events (tenant_id, object_id, copy_id, kind, detail, actor, epoch)
    VALUES (p_tenant, p_object, p_copy, p_kind, coalesce(p_detail, '{}'), 'service:custody', evidence.database_epoch())
    RETURNING event_id INTO new_event;
  RETURN new_event;
EXCEPTION
  WHEN check_violation THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_event_invalid';
END;
$$;

-- Whether this epoch is newer than every one custody has recorded; the first observer appends `epoch_observed`.
CREATE OR REPLACE FUNCTION evidence.custody_epoch_check(p_tenant text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  current text := evidence.database_epoch();
  seen text;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT max(e) INTO seen FROM (
    SELECT max(epoch) AS e FROM evidence.custody_copies WHERE tenant_id = p_tenant
    UNION ALL SELECT max(epoch) FROM evidence.custody_events WHERE tenant_id = p_tenant) x;
  IF seen IS NOT NULL AND current > seen THEN
    PERFORM pg_advisory_xact_lock(hashtextextended('evidence.custody_epoch_check:' || p_tenant, 0));
    IF NOT EXISTS (SELECT 1 FROM evidence.custody_events WHERE tenant_id = p_tenant AND kind = 'epoch_observed'
        AND epoch = current) THEN
      INSERT INTO evidence.custody_events (tenant_id, kind, detail, actor, epoch)
        VALUES (p_tenant, 'epoch_observed', jsonb_build_object('previous', seen), 'service:custody', current);
    END IF;
  END IF;
  RETURN jsonb_build_object('epoch', current, 'previous', seen);
END;
$$;

-- Checks one claim for the record being appended, under the acceptance and object locks it takes (held to the end of
-- the transaction), and returns what the entity or model call takes from it and what its reference pins. Called only
-- by evidence.write_lineage inside evidence.append's transaction; evidence.custody_reference writes the reference.
CREATE OR REPLACE FUNCTION evidence.custody_bind(p_tenant text, p_producer uuid, p_group text, p_scope_kind text,
  p_project uuid, p_run uuid, p_claim text)
RETURNS jsonb LANGUAGE plpgsql SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
DECLARE
  claim evidence.custody_claims%ROWTYPE;
  ready_event evidence.custody_claim_events%ROWTYPE;
  obj evidence.custody_objects%ROWTYPE;
  p evidence.custody_copies%ROWTYPE;
  fl record;
BEGIN
  IF p_claim IS NULL OR p_claim !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_claim_unknown';
  END IF;
  SELECT * INTO claim FROM evidence.custody_claims c WHERE c.tenant_id = p_tenant AND c.claim_id = p_claim::uuid;
  IF NOT FOUND OR claim.producer_id <> p_producer THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_claim_unknown';
  END IF;
  IF claim.group_key <> p_group OR claim.scope_kind <> p_scope_kind OR claim.project_id IS DISTINCT FROM p_project
    OR claim.run_id IS DISTINCT FROM p_run THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:custody_scope';
  END IF;
  IF claim.expires_at <= clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_claim_expired';
  END IF;
  -- The acceptance lock (shared), then the object lock (shared): acceptance and garbage collection take them
  -- exclusively, so neither interleaves with this bind.
  IF p_project IS NOT NULL THEN
    INSERT INTO evidence.custody_project_states (tenant_id, project_id) VALUES (p_tenant, p_project)
      ON CONFLICT DO NOTHING;
    PERFORM 1 FROM evidence.custody_project_states s WHERE s.tenant_id = p_tenant AND s.project_id = p_project
      FOR SHARE;
  END IF;
  SELECT * INTO obj FROM evidence.custody_objects o WHERE o.tenant_id = p_tenant AND o.object_id = claim.object_id
    FOR SHARE;
  SELECT * INTO ready_event FROM evidence.custody_claim_events e
    WHERE e.tenant_id = p_tenant AND e.claim_id = claim.claim_id AND e.kind = 'ready';
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_unverified'; END IF;
  IF NOT (evidence.custody_copy_current(p_tenant, ready_event.primary_copy)
      AND evidence.custody_copy_current(p_tenant, ready_event.replica_copy)) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_unverified';
  END IF;
  IF EXISTS (SELECT 1 FROM evidence.custody_events e WHERE e.tenant_id = p_tenant AND e.object_id = obj.object_id
      AND e.kind = 'delete_intent') THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_object_closing';
  END IF;
  -- The floor now, not the claim's: a project accepted since the claim was issued needs its records period.
  SELECT * INTO fl FROM evidence.custody_floor(p_tenant, p_project, clock_timestamp());
  IF fl.class = 'run' THEN
    fl.floor_until := claim.issued_at + interval '90 days';
  END IF;
  IF ready_event.covered_until < fl.floor_until THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_retention_short';
  END IF;
  SELECT * INTO p FROM evidence.custody_copies WHERE tenant_id = p_tenant AND copy_id = ready_event.primary_copy;
  RETURN jsonb_build_object('claim_id', claim.claim_id, 'object_id', obj.object_id, 'class', fl.class,
    'floor_until', fl.floor_until, 'primary_copy', ready_event.primary_copy, 'replica_copy', ready_event.replica_copy,
    'sha256', obj.raw_sha256, 'size', obj.raw_size, 'media_type', claim.media_type,
    'uri', 's3://' || p.bucket || '/' || obj.object_key, 'object_version', p.version_id);
END;
$$;

-- The reference for a checked claim, once the row that uses it exists (an entity) or for a model call's role.
CREATE OR REPLACE FUNCTION evidence.custody_reference(p_tenant text, p_bound jsonb, p_record uuid,
  p_ingested_at timestamptz, p_role text, p_group text, p_project uuid, p_entity uuid)
RETURNS uuid LANGUAGE sql SET search_path = pg_catalog, evidence AS $$
  INSERT INTO evidence.custody_references (tenant_id, claim_id, object_id, record_id, ingested_at, role, entity_id,
      group_key, project_id, class, floor_until, primary_copy, replica_copy, epoch)
    VALUES (p_tenant, (p_bound->>'claim_id')::uuid, (p_bound->>'object_id')::uuid, p_record, p_ingested_at, p_role,
      p_entity, p_group, p_project, p_bound->>'class', (p_bound->>'floor_until')::timestamptz,
      (p_bound->>'primary_copy')::uuid, (p_bound->>'replica_copy')::uuid, evidence.database_epoch())
    RETURNING reference_id
$$;

-- Decoding and cache metadata a model call may carry: listed keys with numbers, booleans or short tokens.
CREATE OR REPLACE FUNCTION evidence.model_metadata_ok(p jsonb, p_keys text[]) RETURNS boolean LANGUAGE sql IMMUTABLE
SET search_path = pg_catalog AS $$
  SELECT p IS NULL OR (jsonb_typeof(p) = 'object' AND NOT EXISTS (
    SELECT 1 FROM jsonb_each(p) m WHERE NOT (m.key = ANY (p_keys))
      OR jsonb_typeof(m.value) NOT IN ('number', 'boolean', 'string')
      OR (jsonb_typeof(m.value) = 'string' AND (m.value #>> '{}') !~ '^[A-Za-z0-9._:-]{1,32}$')))
$$;

-- Write one record's lineage (migration 079: custody). Called only by evidence.append, inside its transaction, after
-- the record row exists. Migration 076's rules stand; 079 adds the custody rules in the header above.
CREATE OR REPLACE FUNCTION evidence.write_lineage(
  p_tenant text, p_producer uuid, p_producer_kind text, p_stream uuid, p_group text, p_project uuid, p_record uuid,
  p_ingested_at timestamptz, p_source_time timestamptz, p_lineage jsonb)
RETURNS void LANGUAGE plpgsql SET search_path = pg_catalog, evidence AS $$
DECLARE
  activity_row evidence.activities%ROWTYPE;
  stream_row evidence.streams%ROWTYPE;
  owner_kind text;
  act jsonb := p_lineage->'activity';
  item jsonb;
  agent_ids uuid[] := '{}';
  agent_id uuid;
  ent_id uuid;
  other_id uuid;
  held evidence.entities%ROWTYPE;
  declared_generated text[] := '{}';
  key text;
  mc jsonb;
  oversized boolean;
  bound jsonb;
  roles text[];
  digests jsonb := '{}';
  untrusted boolean := p_producer_kind <> 'broker';
BEGIN
  IF NOT evidence.lineage_shape_ok(p_lineage, '{"version": "number", "activity": "object", "agents": "array",
      "entities": "array", "used": "array", "generated": "array", "derived": "array", "informed_by": "array",
      "model_call": "object"}', ARRAY['version']) OR p_lineage->'version' IS DISTINCT FROM '1'::jsonb THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_invalid';
  END IF;
  -- An untrusted producer's lineage carries identifiers, kinds, digests and times only: every string a token.
  IF untrusted AND NOT evidence.tokens_only(p_lineage) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_not_metadata';
  END IF;
  -- A supplied activity time is a time, whether or not an event names it.
  IF p_lineage->'activity' ? 'at' THEN
    PERFORM (p_lineage->'activity'->>'at')::timestamptz;
  END IF;
  SELECT * INTO stream_row FROM evidence.streams WHERE tenant_id = p_tenant AND stream_id = p_stream;
  FOREACH key IN ARRAY ARRAY['agents', 'entities', 'used', 'generated', 'derived', 'informed_by'] LOOP
    oversized := CASE WHEN jsonb_typeof(p_lineage->key) <> 'array' THEN true
      ELSE jsonb_array_length(p_lineage->key) > 256 END;
    IF p_lineage ? key AND oversized THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_invalid';
    END IF;
  END LOOP;
  IF EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(p_lineage->'used', '[]')) e
        WHERE NOT evidence.lineage_shape_ok(e, '{"entity": "string", "role": "string"}', ARRAY['entity', 'role']))
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(p_lineage->'generated', '[]')) e
        WHERE NOT evidence.lineage_shape_ok(e, '{"entity": "string", "role": "string"}', ARRAY['entity', 'role']))
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(p_lineage->'derived', '[]')) e
        WHERE NOT evidence.lineage_shape_ok(e, '{"from": "string", "to": "string", "transform": "string"}',
          ARRAY['from', 'to', 'transform']))
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(p_lineage->'informed_by', '[]')) e
        WHERE jsonb_typeof(e) <> 'string' OR (e #>> '{}') !~ '\S' OR (e #>> '{}') ~ '[[:cntrl:]]') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_invalid';
  END IF;

  IF act IS NOT NULL THEN
    IF NOT evidence.lineage_shape_ok(act, '{"key": "string", "kind": "string", "event": "string", "at": "string"}',
        ARRAY['key', 'kind']) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_invalid';
    END IF;
    IF act->>'kind' = 'review' AND p_producer_kind <> 'broker' THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:lineage_kind_reserved';
    END IF;
    INSERT INTO evidence.activities (tenant_id, stream_id, activity_key, kind, producer_id, first_record)
      VALUES (p_tenant, p_stream, act->>'key', act->>'kind', p_producer, p_record) ON CONFLICT DO NOTHING;
    SELECT * INTO activity_row FROM evidence.activities
      WHERE tenant_id = p_tenant AND stream_id = p_stream AND activity_key = act->>'key';
    IF activity_row.kind <> act->>'kind' THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_activity_kind_changed';
    END IF;
    IF activity_row.producer_id <> p_producer THEN
      SELECT p.kind INTO owner_kind FROM evidence.producers p
        WHERE p.tenant_id = p_tenant AND p.producer_id = activity_row.producer_id;
      IF NOT (owner_kind = 'broker' AND p_producer_kind = 'broker') THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:lineage_activity_not_owned';
      END IF;
    END IF;
    IF act ? 'event' THEN
      INSERT INTO evidence.activity_events (tenant_id, activity_id, record_id, ingested_at, event, occurred_at)
        VALUES (p_tenant, activity_row.activity_id, p_record, p_ingested_at, act->>'event',
          coalesce((act->>'at')::timestamptz, p_source_time));
    END IF;
  ELSIF p_lineage ?| ARRAY['agents', 'used', 'generated', 'derived', 'informed_by', 'model_call'] THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_activity_missing';
  END IF;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_lineage->'agents', '[]')) LOOP
    IF NOT evidence.lineage_shape_ok(item, '{"kind": "string", "identity": "string", "version": "string",
        "image_digest": "string", "role": "string"}', ARRAY['kind', 'identity', 'role']) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_invalid';
    END IF;
    IF item->>'kind' = 'person' AND p_producer_kind <> 'broker' THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:lineage_person_reserved';
    END IF;
    IF item->>'kind' = 'model' AND item->>'version' IS NULL AND item->>'image_digest' IS NULL THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_model_identity_missing';
    END IF;
    IF item->>'kind' = 'tool' AND (item->>'version' IS NULL OR item->>'image_digest' IS NULL) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_tool_identity_missing';
    END IF;
    INSERT INTO evidence.agents (tenant_id, kind, identity, version, image_digest, first_record)
      VALUES (p_tenant, item->>'kind', item->>'identity', item->>'version', item->>'image_digest', p_record)
      ON CONFLICT DO NOTHING;
    SELECT a.agent_id INTO agent_id FROM evidence.agents a WHERE a.tenant_id = p_tenant AND a.kind = item->>'kind'
      AND a.identity = item->>'identity' AND a.version IS NOT DISTINCT FROM item->>'version'
      AND a.image_digest IS NOT DISTINCT FROM item->>'image_digest';
    agent_ids := agent_ids || agent_id;
    INSERT INTO evidence.associated (tenant_id, activity_id, agent_id, record_id, ingested_at, role)
      VALUES (p_tenant, activity_row.activity_id, agent_id, p_record, p_ingested_at, item->>'role')
      ON CONFLICT DO NOTHING;
  END LOOP;

  SELECT coalesce(array_agg(g->>'entity'), '{}') INTO declared_generated
    FROM jsonb_array_elements(coalesce(p_lineage->'generated', '[]')) g;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_lineage->'entities', '[]')) LOOP
    IF NOT evidence.lineage_shape_ok(item, '{"key": "string", "kind": "string", "sha256": "string", "size": "number",
        "media_type": "string", "uri": "string", "object_version": "string", "disclosure": "string",
        "origin": "string", "claim": "string"}', ARRAY['key', 'kind', 'disclosure']) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_invalid';
    END IF;
    IF untrusted AND item ?| ARRAY['sha256', 'size', 'media_type', 'uri', 'object_version'] THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:custody_required';
    END IF;
    IF (item->>'key') = ANY (declared_generated) THEN
      IF coalesce(item->>'origin', 'produced') <> 'produced' THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_invalid';
      END IF;
      bound := NULL;
      IF item ? 'claim' THEN
        IF item ?| ARRAY['sha256', 'size', 'media_type', 'uri', 'object_version'] THEN
          RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_fields_with_claim';
        END IF;
        bound := evidence.custody_bind(p_tenant, p_producer, p_group, stream_row.scope_kind, p_project,
          stream_row.run_id, item->>'claim');
      END IF;
      INSERT INTO evidence.entities (tenant_id, group_key, project_id, entity_key, kind, content_sha256, size_bytes,
          media_type, object_uri, object_version, disclosure, origin, generated_by, first_record)
        VALUES (p_tenant, p_group, p_project, item->>'key', item->>'kind',
          coalesce(bound->>'sha256', item->>'sha256'), coalesce((bound->>'size')::bigint, (item->>'size')::bigint),
          coalesce(bound->>'media_type', item->>'media_type'), coalesce(bound->>'uri', item->>'uri'),
          coalesce(bound->>'object_version', item->>'object_version'), item->>'disclosure', 'produced',
          activity_row.activity_id, p_record)
        RETURNING entity_id INTO ent_id;
      IF bound IS NOT NULL THEN
        PERFORM evidence.custody_reference(p_tenant, bound, p_record, p_ingested_at, 'entity', p_group, p_project,
          ent_id);
      END IF;
    ELSE
      IF untrusted THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:lineage_entity_origin';
      END IF;
      IF item ? 'claim' THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:custody_claim_not_generated';
      END IF;
      IF coalesce(item->>'origin', '') NOT IN ('imported', 'external') THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_entity_origin';
      END IF;
      INSERT INTO evidence.entities (tenant_id, group_key, project_id, entity_key, kind, content_sha256, size_bytes,
          media_type, object_uri, object_version, disclosure, origin, first_record)
        VALUES (p_tenant, p_group, p_project, item->>'key', item->>'kind', item->>'sha256', (item->>'size')::bigint,
          item->>'media_type', item->>'uri', item->>'object_version', item->>'disclosure', item->>'origin', p_record)
        ON CONFLICT DO NOTHING;
      SELECT * INTO held FROM evidence.entities e WHERE e.tenant_id = p_tenant AND e.group_key = p_group
        AND e.project_id IS NOT DISTINCT FROM p_project AND e.entity_key = item->>'key';
      IF held.kind IS DISTINCT FROM item->>'kind' OR held.content_sha256 IS DISTINCT FROM item->>'sha256'
        OR held.size_bytes IS DISTINCT FROM (item->>'size')::bigint OR held.media_type IS DISTINCT FROM item->>'media_type'
        OR held.object_uri IS DISTINCT FROM item->>'uri' OR held.object_version IS DISTINCT FROM item->>'object_version'
        OR held.disclosure IS DISTINCT FROM item->>'disclosure' OR held.origin IS DISTINCT FROM item->>'origin' THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_entity_conflict';
      END IF;
    END IF;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_lineage->'used', '[]')) LOOP
    SELECT e.entity_id INTO ent_id FROM evidence.entities e WHERE e.tenant_id = p_tenant AND e.group_key = p_group
      AND e.project_id IS NOT DISTINCT FROM p_project AND e.entity_key = item->>'entity';
    IF ent_id IS NULL OR item->>'role' IS NULL THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_entity_unknown';
    END IF;
    INSERT INTO evidence.used (tenant_id, activity_id, entity_id, record_id, ingested_at, role)
      VALUES (p_tenant, activity_row.activity_id, ent_id, p_record, p_ingested_at, item->>'role') ON CONFLICT DO NOTHING;
  END LOOP;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_lineage->'generated', '[]')) LOOP
    SELECT e.entity_id INTO ent_id FROM evidence.entities e WHERE e.tenant_id = p_tenant AND e.group_key = p_group
      AND e.project_id IS NOT DISTINCT FROM p_project AND e.entity_key = item->>'entity'
      AND e.first_record = p_record AND e.generated_by = activity_row.activity_id;
    IF ent_id IS NULL OR item->>'role' IS NULL THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_generated_not_declared';
    END IF;
    INSERT INTO evidence.generated (tenant_id, entity_id, activity_id, record_id, ingested_at, role)
      VALUES (p_tenant, ent_id, activity_row.activity_id, p_record, p_ingested_at, item->>'role');
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_lineage->'derived', '[]')) LOOP
    IF (item->>'to') IS NULL OR NOT ((item->>'to') = ANY (declared_generated)) THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:lineage_derived_target';
    END IF;
    SELECT e.entity_id INTO ent_id FROM evidence.entities e WHERE e.tenant_id = p_tenant AND e.group_key = p_group
      AND e.project_id IS NOT DISTINCT FROM p_project AND e.entity_key = item->>'from';
    SELECT e.entity_id INTO other_id FROM evidence.entities e WHERE e.tenant_id = p_tenant AND e.group_key = p_group
      AND e.project_id IS NOT DISTINCT FROM p_project AND e.entity_key = item->>'to' AND e.first_record = p_record;
    IF ent_id IS NULL OR other_id IS NULL OR item->>'transform' IS NULL THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_entity_unknown';
    END IF;
    INSERT INTO evidence.derived (tenant_id, from_entity, to_entity, activity_id, record_id, ingested_at, transform)
      VALUES (p_tenant, ent_id, other_id, activity_row.activity_id, p_record, p_ingested_at, item->>'transform');
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_lineage->'informed_by', '[]')) LOOP
    SELECT a.activity_id INTO other_id FROM evidence.activities a
      WHERE a.tenant_id = p_tenant AND a.stream_id = p_stream AND a.activity_key = item #>> '{}';
    IF other_id IS NULL THEN RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_activity_unknown'; END IF;
    INSERT INTO evidence.informed (tenant_id, activity_id, informant_id, record_id, ingested_at)
      VALUES (p_tenant, activity_row.activity_id, other_id, p_record, p_ingested_at) ON CONFLICT DO NOTHING;
  END LOOP;

  mc := p_lineage->'model_call';
  IF mc IS NOT NULL THEN
    IF untrusted THEN
      -- Only the inference gateway records model calls, and only through claims.
      IF p_producer_kind <> 'inference' THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:model_call_reserved';
      END IF;
      IF NOT evidence.lineage_shape_ok(mc, '{"agent": "number", "decoding": "object", "prompt_claim": "string",
          "completion_claim": "string", "tool_calls_claim": "string", "tool_calls_not_returned": "boolean",
          "reasoning_claim": "string", "reasoning_not_returned": "boolean", "input_tokens": "number",
          "output_tokens": "number", "latency_ms": "number", "cache": "object"}',
          ARRAY['agent', 'prompt_claim', 'completion_claim', 'input_tokens', 'output_tokens', 'latency_ms'])
        OR (mc ? 'tool_calls_claim') = coalesce((mc->>'tool_calls_not_returned')::boolean, false)
        OR (mc ? 'reasoning_claim') = coalesce((mc->>'reasoning_not_returned')::boolean, false)
        OR NOT evidence.model_metadata_ok(mc->'decoding', ARRAY['temperature', 'top_p', 'top_k', 'min_p', 'max_tokens',
          'seed', 'stop_count', 'repetition_penalty', 'presence_penalty', 'frequency_penalty', 'reasoning_effort'])
        OR NOT evidence.model_metadata_ok(mc->'cache', ARRAY['hit', 'cached_tokens', 'prompt_tokens']) THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_model_call_invalid';
      END IF;
      roles := ARRAY['prompt', 'completion'];
      IF mc ? 'tool_calls_claim' THEN roles := array_append(roles, 'tool_calls'); END IF;
      IF mc ? 'reasoning_claim' THEN roles := array_append(roles, 'reasoning'); END IF;
      FOREACH key IN ARRAY roles LOOP
        bound := evidence.custody_bind(p_tenant, p_producer, p_group, stream_row.scope_kind, p_project,
          stream_row.run_id, mc->>(key || '_claim'));
        PERFORM evidence.custody_reference(p_tenant, bound, p_record, p_ingested_at, key, p_group, p_project, NULL);
        digests := digests || jsonb_build_object(key, bound->>'sha256');
      END LOOP;
    ELSIF NOT evidence.lineage_shape_ok(mc, '{"agent": "number", "decoding": "object", "prompt_sha256": "string",
        "completion_sha256": "string", "tool_calls_sha256": "string", "reasoning_sha256": "string",
        "input_tokens": "number", "output_tokens": "number", "latency_ms": "number", "cache": "object"}',
        ARRAY['agent', 'prompt_sha256', 'completion_sha256', 'input_tokens', 'output_tokens', 'latency_ms']) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_model_call_invalid';
    ELSE
      digests := jsonb_build_object('prompt', mc->>'prompt_sha256', 'completion', mc->>'completion_sha256',
        'tool_calls', mc->>'tool_calls_sha256', 'reasoning', mc->>'reasoning_sha256');
    END IF;
    IF jsonb_typeof(mc->'agent') <> 'number' OR (mc->>'agent')::int NOT BETWEEN 0 AND cardinality(agent_ids) - 1
      OR (p_lineage->'agents'->((mc->>'agent')::int)->>'kind') <> 'model' THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_model_call_invalid';
    END IF;
    INSERT INTO evidence.model_calls (tenant_id, activity_id, agent_id, record_id, ingested_at, decoding, prompt_sha256,
        completion_sha256, tool_calls_sha256, reasoning_sha256, input_tokens, output_tokens, latency_ms, cache)
      VALUES (p_tenant, activity_row.activity_id, agent_ids[(mc->>'agent')::int + 1], p_record, p_ingested_at,
        coalesce(mc->'decoding', '{}'), digests->>'prompt', digests->>'completion', digests->>'tool_calls',
        digests->>'reasoning', (mc->>'input_tokens')::int, (mc->>'output_tokens')::int, (mc->>'latency_ms')::int,
        coalesce(mc->'cache', '{}'));
  END IF;
EXCEPTION
  WHEN invalid_text_representation OR numeric_value_out_of_range OR invalid_datetime_format
    OR datetime_field_overflow OR not_null_violation OR check_violation OR unique_violation THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:lineage_invalid', DETAIL = SQLERRM;
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
    p_project, p_run, p_producer, p_event_type, granted_role, p_source_time, now_at, p_payload_sha256, stream.head_hash);
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

-- A project's acceptance (called by evidence.append for an `acceptance.project` record): the project's custody state
-- row FOR UPDATE, so binds in flight finish first and later binds see the acceptance; an approved records class of the
-- project's lab; and a `retention_requested` event for every reference in the project, which the custody service's
-- retention worker extends on both copies. Acceptance is complete when each has a `retention_applied` per copy.
CREATE OR REPLACE FUNCTION evidence.custody_accept_project(p_tenant text, p_group text, p_project uuid, p_record uuid,
  p_class text)
RETURNS void LANGUAGE plpgsql SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
DECLARE
  cls evidence.custody_classes%ROWTYPE;
BEGIN
  IF p_class IS NULL OR p_class !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:records_class_missing';
  END IF;
  SELECT * INTO cls FROM evidence.custody_classes c
    WHERE c.tenant_id = p_tenant AND c.class_id = p_class::uuid AND c.group_key = p_group;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:records_class_missing'; END IF;
  INSERT INTO evidence.custody_project_states (tenant_id, project_id) VALUES (p_tenant, p_project)
    ON CONFLICT DO NOTHING;
  PERFORM 1 FROM evidence.custody_project_states s WHERE s.tenant_id = p_tenant AND s.project_id = p_project
    FOR UPDATE;
  IF EXISTS (SELECT 1 FROM evidence.custody_project_acceptances a WHERE a.tenant_id = p_tenant
      AND a.project_id = p_project) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:project_already_accepted';
  END IF;
  INSERT INTO evidence.custody_project_acceptances (tenant_id, project_id, class_id, record_id, epoch)
    VALUES (p_tenant, p_project, cls.class_id, p_record, evidence.database_epoch());
  INSERT INTO evidence.custody_events (tenant_id, object_id, reference_id, kind, detail, actor, epoch)
    SELECT p_tenant, r.object_id, r.reference_id, 'retention_requested',
        jsonb_build_object('class_id', cls.class_id, 'until', evidence.utc_text(clock_timestamp() + cls.period)),
        'broker:acceptance', evidence.database_epoch()
      FROM evidence.custody_references r WHERE r.tenant_id = p_tenant AND r.project_id = p_project;
END;
$$;

-- Registers a lab's two stores. Only the migration owner runs it (no grant); the bucket name is derived, not given.
CREATE OR REPLACE FUNCTION evidence.custody_register_store(p_tenant text, p_group text, p_copy_role text,
  p_endpoint text)
RETURNS text LANGUAGE plpgsql SET search_path = pg_catalog, evidence AS $$
DECLARE
  name text := evidence.custody_bucket_name(p_tenant, p_group);
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  INSERT INTO evidence.custody_stores (tenant_id, group_key, copy_role, bucket, endpoint_name, epoch)
    VALUES (p_tenant, p_group, p_copy_role, name, p_endpoint, evidence.database_epoch())
    ON CONFLICT DO NOTHING;
  IF NOT EXISTS (SELECT 1 FROM evidence.custody_stores s WHERE s.tenant_id = p_tenant AND s.group_key = p_group
      AND s.copy_role = p_copy_role AND s.bucket = name AND s.endpoint_name = p_endpoint) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'evidence:custody_store_conflict';
  END IF;
  RETURN name;
END;
$$;

-- A records class for a lab, approved by the lab's lead: the API names the signed-in person, as every broker function
-- does. The owner is a person of the lab.
CREATE OR REPLACE FUNCTION evidence.custody_approve_records_class(p_tenant text, p_actor uuid, p_group text,
  p_period interval, p_owner uuid, p_review_cadence interval, p_disposition text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  new_class uuid;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  PERFORM 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = m.tenant_id AND a.principal_id = m.principal_id
    WHERE m.tenant_id = p_tenant AND m.group_key = p_group AND m.principal_id = p_actor AND m.role = 'lead'
      AND a.enabled AND a.principal_class = 'person';
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:records_class_not_lead'; END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_group_memberships m
    WHERE m.tenant_id = p_tenant AND m.group_key = p_group AND m.principal_id = p_owner;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:records_class_owner'; END IF;
  INSERT INTO evidence.custody_classes (tenant_id, group_key, period, owner_id, review_cadence, disposition,
      approved_by, epoch)
    VALUES (p_tenant, p_group, p_period, p_owner, p_review_cadence, p_disposition, p_actor, evidence.database_epoch())
    RETURNING class_id INTO new_class;
  RETURN new_class;
EXCEPTION
  WHEN check_violation OR not_null_violation THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'evidence:records_class_invalid';
END;
$$;

DO $$
DECLARE f text;
BEGIN
  FOREACH f IN ARRAY ARRAY[
      'evidence.custody_caller_producer(text,text)',
      'evidence.custody_scope_granted(text,uuid,text,text,uuid,uuid)',
      'evidence.custody_floor(text,uuid,timestamptz)',
      'evidence.custody_authorise(text,text,text,text,uuid,uuid)',
      'evidence.custody_record_copy(text,uuid,text,text,bigint,text,text,timestamptz,boolean)',
      'evidence.custody_issue_claim(text,text,text,text,uuid,uuid,text,text,bigint,text,text,bigint,text,text,timestamptz,boolean)',
      'evidence.custody_claim_answer(text,uuid,boolean)',
      'evidence.custody_copy_current(text,uuid)',
      'evidence.custody_mark_ready(text,text,uuid,uuid)',
      'evidence.custody_claim(text,text,uuid)',
      'evidence.custody_waiting(text,integer,uuid)',
      'evidence.custody_due_copies(text,interval,integer)',
      'evidence.custody_retention_pending(text,integer)',
      'evidence.custody_observe(text,uuid,uuid,text,jsonb)',
      'evidence.custody_epoch_check(text)',
      'evidence.custody_bind(text,uuid,text,text,uuid,uuid,text)',
      'evidence.custody_reference(text,jsonb,uuid,timestamptz,text,text,uuid,uuid)',
      'evidence.model_metadata_ok(jsonb,text[])',
      'evidence.shape_ok(jsonb,jsonb,integer)',
      'evidence.json_duplicate_members(json)',
      'evidence.tokens_only(jsonb)',
      'evidence.custody_complete_acceptances(text)',
      'evidence.custody_acceptance_state(text,uuid)',
      'evidence.register_event_schema(text,text,jsonb)',
      'evidence.custody_accept_project(text,text,uuid,uuid,text)',
      'evidence.custody_register_store(text,text,text,text)',
      'evidence.custody_approve_records_class(text,uuid,text,interval,uuid,interval,text)',
      'evidence.write_lineage(text,uuid,text,uuid,text,uuid,uuid,timestamptz,timestamptz,jsonb)',
      'evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text)'] LOOP
    EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', f);
  END LOOP;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

COMMENT ON SCHEMA evidence IS 'enrichedlab evidence layout 6';
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (79) ON CONFLICT DO NOTHING;
COMMIT;
