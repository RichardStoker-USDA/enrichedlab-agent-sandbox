-- Digests for result packages registered before the worker built them. The operator builds each
-- one with the worker's own builder (`agent-sandbox-broker digest-packages`, which reads the retained bytes and checks
-- their size and SHA-256) and records it here. The row is bound to the same bytes as a worker's row and says it was
-- backfilled; the attempt and lease generation are the task's last. No runtime role may call it. Additive: the
-- readers already join digests by artifact and bytes, and running services are unchanged.
BEGIN;

ALTER TABLE enrichedlab_agent.broker_artifact_digests ADD COLUMN IF NOT EXISTS source text NOT NULL DEFAULT 'worker';
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
                 WHERE conrelid = 'enrichedlab_agent.broker_artifact_digests'::regclass
                   AND conname = 'broker_artifact_digests_source') THEN
    ALTER TABLE enrichedlab_agent.broker_artifact_digests
      ADD CONSTRAINT broker_artifact_digests_source CHECK (source IN ('worker', 'operator_backfill'));
  END IF;
END $$;

-- The operator's digest for a researcher-visible result package of a task that has ended. The same digest again is
-- accepted; a different digest for the same bytes is refused, as for the worker.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_backfill_artifact_digest(
  p_tenant text,
  p_task uuid,
  p_relative_path text,
  p_artifact_sha256 text,
  p_status text,
  p_digest jsonb
)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_attempt integer;
  v_generation bigint;
  v_artifact uuid;
  v_sha text;
  v_existing text;
BEGIN
  SELECT attempt, lease_generation INTO v_attempt, v_generation FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND task_id = p_task AND status IN ('completed', 'failed', 'cancelled', 'review_required')
  FOR SHARE;
  IF NOT FOUND OR v_generation IS NULL OR v_generation < 1 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:artifact_digest_target_invalid';
  END IF;
  SELECT artifact_id INTO v_artifact FROM enrichedlab_agent.broker_artifacts
  WHERE tenant_id = p_tenant AND task_id = p_task AND relative_path = p_relative_path
    AND visibility = 'researcher' AND role = 'result' AND sha256 = p_artifact_sha256;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:artifact_digest_target_invalid';
  END IF;
  IF jsonb_typeof(p_digest) <> 'object' OR p_digest->>'schema' IS DISTINCT FROM 'enrichedlab.result-digest.v1'
     OR p_digest->>'status' IS DISTINCT FROM p_status THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:artifact_digest_invalid';
  END IF;
  v_sha := encode(sha256(convert_to(p_digest::text, 'UTF8')), 'hex');
  INSERT INTO enrichedlab_agent.broker_artifact_digests (
    tenant_id, artifact_id, artifact_sha256, task_id, attempt, lease_generation, schema_version, status, digest,
    digest_sha256, source
  ) VALUES (
    p_tenant, v_artifact, p_artifact_sha256, p_task, v_attempt, v_generation, 'enrichedlab.result-digest.v1', p_status,
    p_digest, v_sha, 'operator_backfill'
  )
  ON CONFLICT DO NOTHING;
  SELECT digest_sha256 INTO v_existing FROM enrichedlab_agent.broker_artifact_digests
  WHERE tenant_id = p_tenant AND artifact_id = v_artifact AND artifact_sha256 = p_artifact_sha256;
  IF v_existing IS DISTINCT FROM v_sha THEN
    RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:artifact_digest_conflict';
  END IF;
  RETURN v_sha;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_backfill_artifact_digest(text,uuid,text,text,text,jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (35) ON CONFLICT DO NOTHING;
COMMIT;
