-- Paper acceptance. A lab curator, a person who leads or reviews in the lab, accepts a paper in
-- the library before the research lead may read it, and may withdraw that acceptance with a reason. Both are appended
-- paper events, like removal: a paper is accepted while its newest acceptance, withdrawal, removal or restoration is an
-- acceptance, so removing a paper ends its acceptance and a restored paper needs a new one. The coordinator's library
-- read now says of each paper whether it is accepted; which labs the lead may read without acceptance is the
-- coordinator's configuration. Additive: the running API and coordinator keep working.
BEGIN;

DO $$
DECLARE c record;
BEGIN
  FOR c IN SELECT conname FROM pg_constraint
    WHERE conrelid = 'enrichedlab_agent.broker_library_paper_events'::regclass AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%restored%'
  LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.broker_library_paper_events DROP CONSTRAINT %I', c.conname);
  END LOOP;
END $$;
ALTER TABLE enrichedlab_agent.broker_library_paper_events ADD CONSTRAINT broker_library_paper_events_kind CHECK (
  (kind IN ('removed', 'acceptance_withdrawn') AND reason IS NOT NULL AND char_length(reason) BETWEEN 1 AND 600)
  OR (kind IN ('restored', 'accepted') AND reason IS NULL));

-- The newest acceptance while it stands: who accepted and when, or null.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_acceptance(p_tenant text, p_paper uuid)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT CASE WHEN x.kind = 'accepted' THEN jsonb_build_object('accepted_at', x.created_at, 'by', a.display_name) END
  FROM (SELECT * FROM enrichedlab_agent.broker_library_paper_events e
        WHERE e.tenant_id = p_tenant AND e.paper_id = p_paper
        ORDER BY e.created_at DESC, e.event_id DESC LIMIT 1) x
  JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = x.tenant_id AND a.principal_id = x.actor_id
$$;

-- One paper as a lab member sees it, now with its acceptance and whether the caller may accept it. A lab lead or the
-- person who added it may remove it; a lab curator may accept it or withdraw the acceptance.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_paper_json(p_tenant text, p_actor uuid, p_role text,
  x enrichedlab_agent.broker_library_papers)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT jsonb_build_object('paper_id', x.paper_id, 'lab', x.group_key, 'title', x.title, 'authors', x.authors,
    'year', x.year, 'doi', x.doi, 'page_count', x.page_count, 'byte_size', x.byte_size,
    'source_sha256', x.source_sha256, 'parse', x.parse,
    'added_by', jsonb_build_object('name', p.display_name,
      'kind', CASE WHEN p.principal_class <> 'person' THEN 'service_account' ELSE 'person' END,
      'you', x.added_by = p_actor),
    'added_at', x.created_at,
    'labels', enrichedlab_agent.broker_library_labels(p_tenant, x.paper_id),
    'removed', enrichedlab_agent.broker_library_removal(p_tenant, x.paper_id),
    'acceptance', enrichedlab_agent.broker_library_acceptance(p_tenant, x.paper_id),
    'can_remove', p_role = 'lead' OR x.added_by = p_actor,
    'can_accept', enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, x.group_key))
  FROM enrichedlab_agent.broker_principals p WHERE p.tenant_id = p_tenant AND p.principal_id = x.added_by
$$;

-- Accepts a paper in the library (p_accept) or withdraws its acceptance with a reason. Only a lab curator, a person
-- who leads or reviews in the lab, may do either, and only while the paper is in the library. Nothing is appended when
-- the paper already reads that way. Idempotent per caller and request.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_accept(p_tenant text, p_actor uuid, p_group text,
  p_paper uuid, p_accept boolean, p_reason text, p_request uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r text; x enrichedlab_agent.broker_library_papers%ROWTYPE; clean text; kind text;
  prior enrichedlab_agent.broker_library_paper_events%ROWTYPE;
BEGIN
  r := enrichedlab_agent.broker_library_member(p_tenant, p_actor, p_group);
  SELECT * INTO x FROM enrichedlab_agent.broker_library_papers
    WHERE tenant_id = p_tenant AND group_key = p_group AND paper_id = p_paper;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:paper_not_found'; END IF;
  clean := nullif(btrim(coalesce(p_reason, ''), E' \t\r\n'), '');
  kind := CASE WHEN p_accept THEN 'accepted' ELSE 'acceptance_withdrawn' END;
  SELECT * INTO prior FROM enrichedlab_agent.broker_library_paper_events
    WHERE tenant_id = p_tenant AND actor_id = p_actor AND request_id = p_request;
  IF FOUND THEN
    IF prior.paper_id <> p_paper OR prior.kind <> kind OR prior.reason IS DISTINCT FROM clean THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict'; END IF;
    RETURN enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x);
  END IF;
  IF NOT enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, p_group) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:library_forbidden'; END IF;
  IF p_accept IS NULL OR p_request IS NULL OR (p_accept AND clean IS NOT NULL)
     OR (NOT p_accept AND (clean IS NULL OR char_length(clean) > 600)) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:library_invalid'; END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-library-paper:' || p_tenant || ':' || p_paper::text, 0));
  IF enrichedlab_agent.broker_library_removal(p_tenant, p_paper) IS NOT NULL THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:paper_removed'; END IF;
  IF (enrichedlab_agent.broker_library_acceptance(p_tenant, p_paper) IS NOT NULL) IS DISTINCT FROM p_accept THEN
    INSERT INTO enrichedlab_agent.broker_library_paper_events (tenant_id, paper_id, kind, actor_id, request_id, reason)
      VALUES (p_tenant, p_paper, kind, p_actor, p_request, clean);
  END IF;
  RETURN enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x);
END $$;

-- For the coordinator holding a run's lease, as in migration 029, with whether each paper is accepted.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_library(
  p_tenant text,
  p_run uuid,
  p_worker text,
  p_generation bigint,
  p_documents uuid[]
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  r enrichedlab_agent.broker_research_runs%ROWTYPE;
  grp text;
  member boolean;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_documents IS NULL OR cardinality(p_documents) > 100 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:library_invalid';
  END IF;
  SELECT p.group_key INTO grp FROM enrichedlab_agent.broker_projects p
  WHERE p.tenant_id = p_tenant AND p.project_id = r.project_id;
  member := EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = m.tenant_id AND a.principal_id = m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_tenants t ON t.tenant_id = m.tenant_id AND t.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND g.enabled
    WHERE m.tenant_id = p_tenant AND m.group_key = grp AND m.principal_id = r.actor_id);
  IF NOT member THEN
    RETURN jsonb_build_object('member', false, 'papers', '[]'::jsonb);
  END IF;
  RETURN jsonb_build_object('member', true, 'papers', (
    SELECT coalesce(jsonb_agg(jsonb_build_object('paper_id', x.paper_id, 'document_id', x.document_id,
        'title', x.title, 'year', x.year,
        'accepted', enrichedlab_agent.broker_library_acceptance(p_tenant, x.paper_id) IS NOT NULL) ORDER BY x.paper_id), '[]'::jsonb)
    FROM enrichedlab_agent.broker_library_papers x
    WHERE x.tenant_id = p_tenant AND x.group_key = grp AND x.document_id = ANY(p_documents)
      AND enrichedlab_agent.broker_library_removal(p_tenant, x.paper_id) IS NULL));
END;
$$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'enrichedlab_agent' AND p.proname IN ('broker_library_acceptance', 'broker_library_accept')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (34) ON CONFLICT DO NOTHING;
COMMIT;
