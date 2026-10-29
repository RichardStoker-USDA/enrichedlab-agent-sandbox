-- Each lead reply is kept by its SHA-256 beside the texts it answered. A committed turn's receipts
-- already name each reply's hash (content_sha256); its bytes are now kept with the system prompts and contexts. A reply
-- the coordinator's checks refused is kept too, and the deferred turn's event names the context it answered and the
-- reply, so a replay of that call can be compared with what the lead said. An unreadable reply keeps only its fixed
-- code. The texts are read as contexts are: by a reader of the project who is a member of its lab.
BEGIN;

ALTER TABLE enrichedlab_agent.broker_research_run_events
  ADD COLUMN IF NOT EXISTS lead_context_sha256 text,
  ADD COLUMN IF NOT EXISTS lead_reply_sha256 text;
ALTER TABLE enrichedlab_agent.broker_research_run_events
  DROP CONSTRAINT IF EXISTS broker_research_run_events_lead_reply_check;
ALTER TABLE enrichedlab_agent.broker_research_run_events
  ADD CONSTRAINT broker_research_run_events_lead_reply_check CHECK (
    (lead_context_sha256 IS NULL AND lead_reply_sha256 IS NULL)
    OR (lead_context_sha256 IS NOT NULL AND lead_reply_sha256 IS NOT NULL
        AND lead_context_sha256 ~ '^sha256:[0-9a-f]{64}$' AND lead_reply_sha256 ~ '^sha256:[0-9a-f]{64}$'));

-- A deferral that refused a readable lead reply names the context it answered and the reply. Both must already be
-- kept for the tenant, under the same lease, by broker_research_keep_contexts.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_yield_research_run(p_tenant text,p_run uuid,p_worker text,
  p_generation bigint,p_code text,p_reason text,p_lead_context_sha256 text,p_lead_reply_sha256 text)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; next_sequence bigint;
  previous record;
BEGIN
  IF p_code IS NULL OR p_code NOT IN ('waiting_for_tools','model_unavailable','proposal_invalid','checkpoint_changed','turn_limit','review_required')
     OR (p_reason IS NOT NULL AND p_reason !~ '^[a-z][a-z0-9_]{1,63}$')
     OR ((p_lead_context_sha256 IS NULL) <> (p_lead_reply_sha256 IS NULL))
     OR (p_lead_reply_sha256 IS NOT NULL AND (p_code <> 'proposal_invalid'
         OR p_lead_context_sha256 !~ '^sha256:[0-9a-f]{64}$' OR p_lead_reply_sha256 !~ '^sha256:[0-9a-f]{64}$')) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  IF p_lead_reply_sha256 IS NOT NULL AND (SELECT count(*) FROM enrichedlab_agent.broker_research_contexts c
      WHERE c.tenant_id=p_tenant AND c.sha256 IN (p_lead_context_sha256,p_lead_reply_sha256))
      <> (CASE WHEN p_lead_context_sha256=p_lead_reply_sha256 THEN 1 ELSE 2 END) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_not_kept'; END IF;
  UPDATE enrichedlab_agent.broker_research_runs SET
    failures=failures+CASE WHEN p_code IN ('model_unavailable','proposal_invalid') THEN 1 ELSE 0 END,
    status=CASE WHEN p_code IN ('turn_limit','review_required') THEN 'review_required'
      WHEN p_code IN ('model_unavailable','proposal_invalid') AND failures>=2 THEN 'paused' ELSE 'waiting' END,
    code=p_code,available_at=clock_timestamp()+CASE WHEN p_code='waiting_for_tools' THEN interval '5 seconds' ELSE interval '15 seconds' END,
    lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=p_run AND status='running' AND lease_owner=p_worker AND generation=p_generation AND lease_until>clock_timestamp()
    RETURNING * INTO r;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  SELECT e.sequence,e.code,e.turn INTO previous FROM enrichedlab_agent.broker_research_run_events e
    WHERE e.tenant_id=p_tenant AND e.run_id=p_run ORDER BY e.sequence DESC LIMIT 1;
  -- A run polls every few seconds while a tool works. Keep the first wait of a turn,
  -- not one row per poll; every other deferral is recorded.
  IF p_code='waiting_for_tools' AND previous.code='waiting_for_tools' AND previous.turn=r.turn THEN RETURN; END IF;
  next_sequence := coalesce(previous.sequence,0)+1;
  INSERT INTO enrichedlab_agent.broker_research_run_events(tenant_id,run_id,sequence,code,reason,generation,turn,failures,
    status_after,lead_context_sha256,lead_reply_sha256)
    VALUES(p_tenant,p_run,next_sequence,p_code,p_reason,p_generation,r.turn,r.failures,r.status,p_lead_context_sha256,
      p_lead_reply_sha256);
END $$;

-- The six-argument signature stays valid for a coordinator that has not been upgraded.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_yield_research_run(p_tenant text,p_run uuid,p_worker text,
  p_generation bigint,p_code text,p_reason text)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  PERFORM enrichedlab_agent.broker_yield_research_run(p_tenant,p_run,p_worker,p_generation,p_code,p_reason,NULL::text,
    NULL::text);
END $$;

-- A reader of the project who is a member of its lab reads a text a decision or a deferral of this run names: the
-- system prompts, contexts and replies of a committed turn, and the context and reply of a refused lead reply.
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
                       t.record->'lead'->>'content_sha256',
                       t.record->'methods_specialist'->>'context_sha256', t.record->'methods_specialist'->>'system_sha256',
                       t.record->'methods_specialist'->>'content_sha256'))
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

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'enrichedlab_agent' AND p.proname IN ('broker_yield_research_run', 'broker_research_context')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (44) ON CONFLICT DO NOTHING;
COMMIT;
