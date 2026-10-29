BEGIN;

-- Runtime roles receive a boolean, never access to migration or research tables.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT p_version IN (2, 3) AND EXISTS (
    SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = p_version
  );
$$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_schema_ready(integer) FROM PUBLIC;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (4) ON CONFLICT DO NOTHING;
COMMIT;
