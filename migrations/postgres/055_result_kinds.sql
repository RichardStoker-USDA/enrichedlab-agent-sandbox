-- Result kinds. The worker checks each researcher-visible artifact against the result kind registry
-- built into the broker and records, once per artifact bytes and registry, the kind the bytes hold and the check's
-- receipt. A file that fails its kind's check, or that no accepted kind claims, is recorded as file.opaque, with the
-- kind it failed and the reason; it stays downloadable. The API serves the record made under the registry it runs
-- with, so after a registry change an artifact shows no kind until that registry has checked it. A record says what a
-- deterministic check found in the stored bytes; it validates no scientific claim. A record is bound to the artifact's
-- bytes and to its recorded media type, which chooses how the bytes are read (a CSV and a TSV of the same bytes differ):
-- a registration that changes either leaves the artifact unchecked again.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_artifact_kinds (
  tenant_id text NOT NULL,
  artifact_id uuid NOT NULL,
  -- A retry may re-register a path with different bytes; each record stays bound to the bytes it checked.
  artifact_sha256 text NOT NULL,
  media_type text NOT NULL,
  registry_digest text NOT NULL,
  kind_id text NOT NULL,
  kind_version integer NOT NULL,
  checked_kind_id text NOT NULL,
  checked_kind_version integer NOT NULL,
  state text NOT NULL,
  reason text,
  -- The receipt as the worker wrote it, in RFC 8785 canonical JSON; its SHA-256 is over these exact bytes.
  receipt text NOT NULL,
  receipt_sha256 text NOT NULL,
  recorded_by text NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, artifact_id, artifact_sha256, media_type, registry_digest),
  FOREIGN KEY (tenant_id, artifact_id) REFERENCES enrichedlab_agent.broker_artifacts (tenant_id, artifact_id),
  CHECK (artifact_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (length(media_type) BETWEEN 1 AND 256),
  CHECK (registry_digest ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (kind_id ~ '^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)+$' AND length(kind_id) <= 64),
  CHECK (checked_kind_id ~ '^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)+$' AND length(checked_kind_id) <= 64),
  CHECK (kind_version > 0 AND checked_kind_version > 0),
  CHECK (
    (state = 'passed' AND kind_id = checked_kind_id AND kind_version = checked_kind_version AND reason IS NULL)
    OR (state = 'failed' AND kind_id = 'file.opaque' AND reason IS NOT NULL
        AND length(reason) BETWEEN 1 AND 300)
  ),
  CHECK (octet_length(receipt) <= 4096),
  CHECK (receipt_sha256 = encode(sha256(convert_to(receipt, 'UTF8')), 'hex')),
  CHECK (length(recorded_by) BETWEEN 1 AND 200)
);
ALTER TABLE enrichedlab_agent.broker_artifact_kinds ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_artifact_kinds FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_artifact_kinds;
-- An earlier draft of this migration bound a record to the bytes alone. Its records cannot name the media type their
-- receipts must now carry, so they are removed (the worker checks those artifacts again) and the table takes the
-- current shape. The draft's record function and its reader, which returned no bytes or media type, are dropped so the
-- current ones replace them; the runtime grants are applied again after migrating, as always.
DO $$
DECLARE
  v_reader regprocedure := to_regprocedure('enrichedlab_agent.broker_task_artifact_kinds(text,uuid,uuid,text)');
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid = 'enrichedlab_agent.broker_artifact_kinds'::regclass
                   AND attname = 'media_type' AND NOT attisdropped) THEN
    DELETE FROM enrichedlab_agent.broker_artifact_kinds;
    ALTER TABLE enrichedlab_agent.broker_artifact_kinds
      ADD COLUMN media_type text NOT NULL,
      ADD CHECK (length(media_type) BETWEEN 1 AND 256),
      DROP CONSTRAINT broker_artifact_kinds_pkey,
      ADD PRIMARY KEY (tenant_id, artifact_id, artifact_sha256, media_type, registry_digest);
  END IF;
  IF v_reader IS NOT NULL AND pg_get_function_result(v_reader) NOT LIKE '%media_type%' THEN
    DROP FUNCTION enrichedlab_agent.broker_task_artifact_kinds(text,uuid,uuid,text);
  END IF;
END $$;
DROP FUNCTION IF EXISTS
  enrichedlab_agent.broker_record_artifact_kind(text,text,uuid,text,text,text,integer,text,integer,text,text,text);
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_artifact_kinds
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- The worker's next researcher-visible artifacts that the registry has not checked, newest first. `p_skip` names
-- artifacts whose stored bytes the worker could not read lately, so one unreadable file does not hold the queue.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_artifacts_awaiting_kind(
  p_tenant text,
  p_registry text,
  p_skip uuid[],
  p_limit integer
)
RETURNS TABLE(task_id uuid, artifact_id uuid, relative_path text, media_type text, size_bytes bigint, sha256 text)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT a.task_id, a.artifact_id, a.relative_path, a.media_type, a.size_bytes, a.sha256
  FROM enrichedlab_agent.broker_artifacts AS a
  WHERE a.tenant_id = p_tenant
    AND a.visibility = 'researcher'
    AND NOT (a.artifact_id = ANY (coalesce(p_skip, '{}'::uuid[])))
    AND NOT EXISTS (
      SELECT 1 FROM enrichedlab_agent.broker_artifact_kinds AS k
      WHERE k.tenant_id = a.tenant_id AND k.artifact_id = a.artifact_id AND k.artifact_sha256 = a.sha256
        AND k.media_type = a.media_type AND k.registry_digest = p_registry)
  ORDER BY a.created_at DESC, a.artifact_id
  LIMIT least(greatest(coalesce(p_limit, 1), 1), 100)
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_artifacts_awaiting_kind(text,text,uuid[],integer) FROM PUBLIC;

-- The worker records one check. The artifact must still hold the bytes and media type the worker checked, and the
-- receipt must name the same bytes, media type, size, registry, kind checked, result and reason as the columns. The same
-- record again is accepted; a different one for the same bytes, media type and registry is refused. Returns the
-- receipt's SHA-256.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_artifact_kind(
  p_tenant text,
  p_worker text,
  p_artifact uuid,
  p_artifact_sha256 text,
  p_media_type text,
  p_registry text,
  p_kind text,
  p_kind_version integer,
  p_checked_kind text,
  p_checked_version integer,
  p_state text,
  p_reason text,
  p_receipt text
)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_size bigint;
  v_receipt jsonb;
  v_sha text;
  v_existing text;
BEGIN
  SELECT size_bytes INTO v_size FROM enrichedlab_agent.broker_artifacts
  WHERE tenant_id = p_tenant AND artifact_id = p_artifact AND sha256 = p_artifact_sha256
    AND media_type = p_media_type AND visibility = 'researcher';
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:artifact_kind_target_invalid';
  END IF;
  BEGIN
    v_receipt := p_receipt::jsonb;
  EXCEPTION WHEN invalid_text_representation THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:artifact_kind_receipt_invalid';
  END;
  IF jsonb_typeof(v_receipt) IS DISTINCT FROM 'object'
     OR v_receipt->>'routine' IS DISTINCT FROM 'enrichedlab.kind-check'
     OR v_receipt->'version' IS DISTINCT FROM to_jsonb(1)
     OR v_receipt->>'registry' IS DISTINCT FROM p_registry
     OR v_receipt->>'input_sha256' IS DISTINCT FROM p_artifact_sha256
     OR v_receipt->>'media_type' IS DISTINCT FROM p_media_type
     OR v_receipt->'size' IS DISTINCT FROM to_jsonb(v_size)
     OR v_receipt->'checked_as'->>'id' IS DISTINCT FROM p_checked_kind
     OR v_receipt->'checked_as'->'version' IS DISTINCT FROM to_jsonb(p_checked_version)
     OR v_receipt->>'result' IS DISTINCT FROM p_state
     OR v_receipt->>'reason' IS DISTINCT FROM p_reason THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:artifact_kind_receipt_invalid';
  END IF;
  v_sha := encode(sha256(convert_to(p_receipt, 'UTF8')), 'hex');
  INSERT INTO enrichedlab_agent.broker_artifact_kinds (
    tenant_id, artifact_id, artifact_sha256, media_type, registry_digest, kind_id, kind_version, checked_kind_id,
    checked_kind_version, state, reason, receipt, receipt_sha256, recorded_by)
  VALUES (p_tenant, p_artifact, p_artifact_sha256, p_media_type, p_registry, p_kind, p_kind_version, p_checked_kind,
    p_checked_version, p_state, p_reason, p_receipt, v_sha, p_worker)
  ON CONFLICT (tenant_id, artifact_id, artifact_sha256, media_type, registry_digest) DO NOTHING;
  IF NOT FOUND THEN
    SELECT receipt_sha256 INTO v_existing FROM enrichedlab_agent.broker_artifact_kinds
    WHERE tenant_id = p_tenant AND artifact_id = p_artifact AND artifact_sha256 = p_artifact_sha256
      AND media_type = p_media_type AND registry_digest = p_registry;
    IF v_existing IS DISTINCT FROM v_sha THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:artifact_kind_conflict';
    END IF;
  END IF;
  RETURN v_sha;
EXCEPTION WHEN check_violation THEN
  RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:artifact_kind_receipt_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_artifact_kind(text,text,uuid,text,text,text,text,integer,text,integer,text,text,text) FROM PUBLIC;

-- A task reader's kind records for the task's artifacts under one registry, for their current bytes and media type,
-- each with the bytes and media type it was made for, so a caller joining it to an artifact list made in another read
-- attaches a record only to the artifact it describes.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_artifact_kinds(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_registry text
)
RETURNS TABLE(artifact_id uuid, artifact_sha256 text, media_type text, kind_id text, kind_version integer,
  checked_kind_id text, checked_kind_version integer, state text, reason text, receipt_sha256 text)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT k.artifact_id, k.artifact_sha256, k.media_type, k.kind_id, k.kind_version, k.checked_kind_id,
    k.checked_kind_version, k.state, k.reason, k.receipt_sha256
  FROM enrichedlab_agent.broker_artifacts AS a
  JOIN enrichedlab_agent.broker_artifact_kinds AS k
    ON k.tenant_id = a.tenant_id AND k.artifact_id = a.artifact_id AND k.artifact_sha256 = a.sha256
      AND k.media_type = a.media_type AND k.registry_digest = p_registry
  WHERE a.tenant_id = p_tenant AND a.task_id = p_task AND a.visibility = 'researcher'
    AND enrichedlab_agent.broker_task_can(p_tenant, p_principal, a.task_id, 'read')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_artifact_kinds(text,uuid,uuid,text) FROM PUBLIC;

-- One artifact's record with its receipt, for a reader of the artifact. Null when the artifact cannot be read; a
-- readable artifact that this registry has not checked answers with state not_checked.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_artifact_kind(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_artifact uuid,
  p_registry text
)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object(
    'artifact_id', a.artifact_id,
    'artifact_sha256', a.sha256,
    'media_type', a.media_type,
    'registry', p_registry,
    'state', coalesce(k.state, 'not_checked'),
    'kind_id', k.kind_id,
    'kind_version', k.kind_version,
    'checked_kind_id', k.checked_kind_id,
    'checked_kind_version', k.checked_kind_version,
    'reason', k.reason,
    'receipt', k.receipt,
    'receipt_sha256', k.receipt_sha256,
    'recorded_at', k.recorded_at)
  FROM enrichedlab_agent.broker_artifacts AS a
  LEFT JOIN enrichedlab_agent.broker_artifact_kinds AS k
    ON k.tenant_id = a.tenant_id AND k.artifact_id = a.artifact_id AND k.artifact_sha256 = a.sha256
      AND k.media_type = a.media_type AND k.registry_digest = p_registry
  WHERE a.tenant_id = p_tenant AND a.task_id = p_task AND a.artifact_id = p_artifact
    AND a.visibility = 'researcher'
    AND enrichedlab_agent.broker_task_can(p_tenant, p_principal, a.task_id, 'read')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_artifact_kind(text,uuid,uuid,uuid,text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (55) ON CONFLICT DO NOTHING;
COMMIT;
