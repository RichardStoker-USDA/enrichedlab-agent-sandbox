-- Each lab's methods for the workspace. A person who belongs to several labs saw, in every lab, the
-- methods of all of them, and a method the chosen lab may not run was refused only after it was submitted. This read
-- gives the API, for each of the reader's labs, the enabled methods granted to that lab, so the workspace offers a lab
-- only its own. It is a read; submissions keep their own checks.
-- Rollback: an older API does not call it.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_group_methods(p_tenant text, p_principal uuid)
RETURNS TABLE(group_key text, profile_key text)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT grants.group_key, grants.profile_key
  FROM enrichedlab_agent.broker_group_memberships AS memberships
  JOIN enrichedlab_agent.broker_group_profile_grants AS grants
    ON grants.tenant_id = memberships.tenant_id
   AND grants.group_key = memberships.group_key
  JOIN enrichedlab_agent.broker_agent_profiles AS profiles
    ON profiles.tenant_id = grants.tenant_id
   AND profiles.profile_key = grants.profile_key
  WHERE memberships.tenant_id = p_tenant
    AND memberships.principal_id = p_principal
    AND profiles.enabled
  ORDER BY grants.group_key, grants.profile_key
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_group_methods(text, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (52) ON CONFLICT DO NOTHING;
COMMIT;
