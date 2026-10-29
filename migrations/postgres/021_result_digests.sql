-- Result digests. The research lead saw each analysis's status and artifact references,
-- never what the analysis found. The worker now records, once per retained result package, a bounded
-- digest built from the bytes it verified: the result tables (cells as written when small), figure
-- names and run parameters. A digest is a deterministic excerpt of retained output; it validates
-- nothing. The coordinator reads digests and the project's newest findings through a lease-fenced
-- function added here, so the running coordinator's claim is unchanged. The API serves a digest to
-- anyone who may read its artifact.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_artifact_digests (
  tenant_id text NOT NULL,
  artifact_id uuid NOT NULL,
  -- A retry may re-register a path with different bytes; each digest stays bound to the bytes it read.
  artifact_sha256 text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  schema_version text NOT NULL,
  status text NOT NULL,
  digest jsonb NOT NULL,
  digest_sha256 text NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, artifact_id, artifact_sha256),
  FOREIGN KEY (tenant_id, artifact_id) REFERENCES enrichedlab_agent.broker_artifacts (tenant_id, artifact_id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  CHECK (artifact_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (digest_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (attempt >= 0),
  CHECK (lease_generation > 0),
  CHECK (schema_version = 'enrichedlab.result-digest.v1'),
  CHECK (status IN ('summarized', 'no_result_tables', 'not_a_package', 'integrity_failed', 'too_large', 'unreadable')),
  CHECK (jsonb_typeof(digest) = 'object' AND octet_length(digest::text) <= 16384)
);
ALTER TABLE enrichedlab_agent.broker_artifact_digests ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_artifact_digests FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_artifact_digests;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_artifact_digests
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- The worker records a digest for a researcher-visible result it registered under its current lease.
-- The same digest again is accepted; a different digest for the same bytes is refused. The digest's
-- SHA-256 is computed here over the stored form, so any reader can check it.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_artifact_digest(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint,
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
  v_artifact uuid;
  v_sha text;
  v_existing text;
BEGIN
  SELECT attempt INTO v_attempt FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND task_id = p_task AND lease_owner = p_worker AND lease_generation = p_generation
  FOR SHARE;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '40001', MESSAGE = 'broker:lease_fence_lost';
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
    tenant_id, artifact_id, artifact_sha256, task_id, attempt, lease_generation, schema_version, status, digest, digest_sha256
  ) VALUES (
    p_tenant, v_artifact, p_artifact_sha256, p_task, v_attempt, p_generation, 'enrichedlab.result-digest.v1', p_status, p_digest, v_sha
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
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_artifact_digest(text,uuid,text,bigint,text,text,text,jsonb) FROM PUBLIC;

-- For the coordinator holding a run's lease: the digests of each planned analysis's current result
-- packages (at most four per analysis, newest first) and the project's newest recorded findings.
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
  RETURN jsonb_build_object('results', results, 'earlier_findings', findings);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_results(text,uuid,text,bigint) FROM PUBLIC;

-- A reader of an artifact may read its digest for the artifact's current bytes, or learn that none exists.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_artifact_digest(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_artifact uuid
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
    'status', coalesce(d.status, 'not_recorded'),
    'digest_sha256', d.digest_sha256,
    'recorded_at', d.recorded_at,
    'digest', d.digest)
  FROM enrichedlab_agent.broker_artifacts a
  LEFT JOIN enrichedlab_agent.broker_artifact_digests d
    ON d.tenant_id = a.tenant_id AND d.artifact_id = a.artifact_id AND d.artifact_sha256 = a.sha256
  WHERE a.tenant_id = p_tenant AND a.task_id = p_task AND a.artifact_id = p_artifact
    AND a.visibility = 'researcher'
    AND enrichedlab_agent.broker_task_can(p_tenant, p_principal, a.task_id, 'read')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_artifact_digest(text,uuid,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (21) ON CONFLICT DO NOTHING;
COMMIT;
