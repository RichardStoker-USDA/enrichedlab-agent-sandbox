-- The harness surface manifest. At start each coordinator computes the index of every surface that shapes its model
-- calls (the lead system text's digest, model limits and section budgets, settings and profile digests, which labs
-- each feature reaches, supervisor bounds, retrieval generations) with each surface marked editable or fixed, and
-- stores its canonical JSON here once by digest. Every turn record then names it as `harness_manifest_sha256`, so a
-- decision traces to the exact harness it ran under. The digest is checked by the database against the stored bytes;
-- a row is never changed or deleted. The manifest holds digests, numbers, names and lab keys, no secret, path,
-- endpoint or research value. Only the coordinator role writes it; no person or API role reads it here (the
-- retrospective tooling reads it under operator custody).
--
-- Rollback: move coordinators to an image without 089, drop broker_record_harness_manifest and
-- broker_harness_manifests, run 088's broker_schema_ready again, and delete schema_migrations version 89. Turn records
-- keep their harness_manifest_sha256 fields; nothing else reads them.
BEGIN;
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_harness_manifests (
  tenant_id text NOT NULL,
  sha256 text NOT NULL,
  content bytea NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, sha256),
  FOREIGN KEY (tenant_id) REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  CHECK (octet_length(content) BETWEEN 1 AND 65536),
  CHECK (sha256 = 'sha256:' || encode(pg_catalog.sha256(content), 'hex'))
);
ALTER TABLE enrichedlab_agent.broker_harness_manifests ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_harness_manifests FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_harness_manifests;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_harness_manifests
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- Keeps one manifest, once: the bytes must be a JSON object whose schema_version is the number 1 and whose SHA-256 is
-- the digest given. A second call with the same digest keeps the first row (the same bytes by construction) and
-- returns true.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_harness_manifest(
  p_tenant text, p_sha256 text, p_content bytea)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE doc jsonb;
BEGIN
  IF p_tenant IS NULL OR p_sha256 IS NULL OR p_content IS NULL OR p_sha256 !~ '^sha256:[0-9a-f]{64}$'
    OR octet_length(p_content) NOT BETWEEN 1 AND 65536
    OR p_sha256 IS DISTINCT FROM 'sha256:' || encode(pg_catalog.sha256(p_content), 'hex') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:harness_manifest_invalid';
  END IF;
  BEGIN
    doc := convert_from(p_content, 'UTF8')::jsonb;
  EXCEPTION WHEN OTHERS THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:harness_manifest_invalid';
  END;
  IF jsonb_typeof(doc) IS DISTINCT FROM 'object' OR doc->'schema_version' IS DISTINCT FROM '1'::jsonb
    OR jsonb_typeof(doc->'surfaces') IS DISTINCT FROM 'object' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:harness_manifest_invalid';
  END IF;
  INSERT INTO enrichedlab_agent.broker_harness_manifests(tenant_id, sha256, content)
  VALUES (p_tenant, p_sha256, p_content) ON CONFLICT (tenant_id, sha256) DO NOTHING;
  RETURN true;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_harness_manifest(text, text, bytea) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (89) ON CONFLICT DO NOTHING;
COMMIT;
