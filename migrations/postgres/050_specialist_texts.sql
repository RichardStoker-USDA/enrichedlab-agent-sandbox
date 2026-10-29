-- The literature specialist's texts. A turn that read the specialist's claims keeps
-- the specialist's instructions, each call's context and its reply beside the lead's and the methods check's, and names
-- their digests in the receipts under `literature_claims.specialist`; a reader of the project who is a member of its
-- lab may now read those texts too. Everything else in the reader is migration 044's.
-- Rollback: the reader of migration 044 serves every text it did before; the specialist's kept texts stay unreadable
-- through it.
BEGIN;

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
      AND (p_sha256 IN (t.record->'lead'->>'context_sha256', t.record->'lead'->>'system_sha256',
                        t.record->'lead'->>'content_sha256',
                        t.record->'methods_specialist'->>'context_sha256', t.record->'methods_specialist'->>'system_sha256',
                        t.record->'methods_specialist'->>'content_sha256')
        OR EXISTS (
          SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(t.record->'literature_claims'->'specialist') = 'array'
                                                  THEN t.record->'literature_claims'->'specialist' ELSE '[]'::jsonb END) s
          WHERE p_sha256 IN (s->>'context_sha256', s->>'system_sha256', s->>'content_sha256'))))
     AND NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_research_run_events e
    WHERE e.tenant_id = p_tenant AND e.run_id = p_run
      AND p_sha256 IN (e.lead_context_sha256, e.lead_reply_sha256)) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:context_not_found';
  END IF;
  SELECT content INTO body FROM enrichedlab_agent.broker_research_contexts
  WHERE tenant_id = p_tenant AND sha256 = p_sha256;
  IF NOT FOUND THEN
    -- A decision from before migration 032, or a reply from before this one, names its hash but kept no bytes.
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:context_not_kept';
  END IF;
  RETURN jsonb_build_object('sha256', p_sha256, 'bytes', octet_length(body), 'text', convert_from(body, 'UTF8'));
END;
$$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_context(text,uuid,uuid,text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (50) ON CONFLICT DO NOTHING;
COMMIT;
