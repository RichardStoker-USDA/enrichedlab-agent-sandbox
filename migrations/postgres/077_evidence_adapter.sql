-- What the evidence append service needs to know about itself: the producers its database role may append as in a
-- tenant, and whether that tenant is enabled. The service refuses to start (and reports itself not serving) unless
-- every producer its configuration maps is bound to its role here. It reads only the calling role's own bindings.
--
-- Rollback: drop the function; nothing else depends on it.
BEGIN;

CREATE OR REPLACE FUNCTION evidence.adapter_producers(p_tenant text)
RETURNS text[] LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent
SET enrichedlab.tenant_id = '' AS $$
DECLARE
  bound text[];
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_tenants t WHERE t.tenant_id = p_tenant AND t.enabled;
  IF NOT FOUND THEN
    RETURN NULL;
  END IF;
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  -- The same rule evidence.append applies: a producer with its own database role accepts only that role; any other
  -- producer accepts the roles bound to it.
  SELECT coalesce(array_agg(p.identity ORDER BY p.identity), '{}') INTO bound
    FROM evidence.producers p
    WHERE p.tenant_id = p_tenant AND p.enabled
      AND (p.database_role = session_user
           OR (p.database_role IS NULL AND EXISTS (
             SELECT 1 FROM evidence.adapter_bindings b
             WHERE b.tenant_id = p_tenant AND b.producer_id = p.producer_id AND b.role_name = session_user)));
  RETURN bound;
END;
$$;
REVOKE ALL ON FUNCTION evidence.adapter_producers(text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (77) ON CONFLICT DO NOTHING;
COMMIT;
