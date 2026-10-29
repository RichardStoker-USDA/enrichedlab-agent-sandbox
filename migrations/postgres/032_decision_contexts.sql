-- Each research decision keeps the exact text its models were given. A turn's
-- receipts name the SHA-256 of the fixed instructions and of the compiled context for the lead and the methods
-- check; this table keeps those bytes by that address, so a decision can be read and replayed byte for byte
-- after the method catalog, the notes or the library change. The context can hold up to four passages from the
-- lab's papers and the lab's shared notes, so it is readable only by people who may read the project and are
-- members of its lab, and it is never part of a download.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_research_contexts (
  tenant_id text NOT NULL,
  sha256 text NOT NULL,
  content bytea NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, sha256),
  FOREIGN KEY (tenant_id) REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  CHECK (octet_length(content) BETWEEN 1 AND 262144),
  CHECK (sha256 = 'sha256:' || encode(pg_catalog.sha256(content), 'hex'))
);
ALTER TABLE enrichedlab_agent.broker_research_contexts ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_research_contexts FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_research_contexts;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_research_contexts
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- The coordinator keeps the texts of a decision under its current lease, before it commits the turn.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_keep_contexts(
  p_tenant text, p_run uuid, p_worker text, p_generation bigint, p_contents bytea[])
RETURNS text[] LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  c bytea;
  kept text[] := '{}';
  digest text;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_contents IS NULL OR cardinality(p_contents) NOT BETWEEN 1 AND 8 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:context_invalid';
  END IF;
  FOREACH c IN ARRAY p_contents LOOP
    IF c IS NULL OR octet_length(c) NOT BETWEEN 1 AND 262144 THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:context_invalid';
    END IF;
    digest := 'sha256:' || encode(sha256(c), 'hex');
    INSERT INTO enrichedlab_agent.broker_research_contexts (tenant_id, sha256, content)
    VALUES (p_tenant, digest, c) ON CONFLICT (tenant_id, sha256) DO NOTHING;
    kept := kept || digest;
  END LOOP;
  RETURN kept;
END;
$$;

-- A reader of the project who is a member of its lab reads a text a decision of this run names.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_context(
  p_tenant text, p_actor uuid, p_run uuid, p_sha256 text)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  r enrichedlab_agent.broker_research_runs%ROWTYPE;
  grp text;
  body bytea;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id = p_tenant AND run_id = p_run;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, r.project_id, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF p_sha256 IS NULL OR p_sha256 !~ '^sha256:[0-9a-f]{64}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:context_invalid';
  END IF;
  SELECT group_key INTO grp FROM enrichedlab_agent.broker_projects
  WHERE tenant_id = p_tenant AND project_id = r.project_id;
  IF NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = m.tenant_id AND a.principal_id = m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND g.enabled
    WHERE m.tenant_id = p_tenant AND m.group_key = grp AND m.principal_id = p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:context_lab_members_only';
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_research_turns t
    WHERE t.tenant_id = p_tenant AND t.run_id = p_run
      AND p_sha256 IN (t.record->'lead'->>'context_sha256', t.record->'lead'->>'system_sha256',
                       t.record->'methods_specialist'->>'context_sha256', t.record->'methods_specialist'->>'system_sha256')) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:context_not_found';
  END IF;
  SELECT content INTO body FROM enrichedlab_agent.broker_research_contexts
  WHERE tenant_id = p_tenant AND sha256 = p_sha256;
  IF NOT FOUND THEN
    -- A decision from before this migration names its hashes but kept no bytes.
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:context_not_kept';
  END IF;
  RETURN jsonb_build_object('sha256', p_sha256, 'bytes', octet_length(body), 'text', convert_from(body, 'UTF8'));
END;
$$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'enrichedlab_agent' AND p.proname IN ('broker_research_keep_contexts', 'broker_research_context')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (32) ON CONFLICT DO NOTHING;
COMMIT;
