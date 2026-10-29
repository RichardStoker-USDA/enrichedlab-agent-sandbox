-- Research runs are claimed fairly: among the runs ready for a lead call, the coordinator takes one
-- from the research group with the fewest runs holding a live lease, then from the person with the fewest, then the
-- oldest, so one lab or one person starting many runs cannot hold every coordinator slot while others wait. The order
-- is work-conserving: a lone lab still gets every slot. Each two minutes a run has been ready counts as one fewer
-- lease for its group, so a run the fair order keeps passing over (a person with leases in other groups, say) is
-- still claimed within bounded time. Claims within a tenant run one at a time under a transaction advisory lock; a
-- claim that finds it taken claims nothing and the coordinator asks again on its next pass, so no claim waits on
-- another. Everything else in the function is migration 015's; re-applying 015's definition restores first-in,
-- first-out claims.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_claim_research_run(p_tenant text,p_worker text,p_seconds integer)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project jsonb; profiles jsonb; facts jsonb;
BEGIN
  IF p_seconds NOT BETWEEN 30 AND 600 OR length(p_worker) NOT BETWEEN 1 AND 100 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:lease_invalid'; END IF;
  IF NOT pg_try_advisory_xact_lock(hashtextextended('broker-research-claim:' || p_tenant, 0)) THEN RETURN NULL; END IF;
  -- A revoked delegation never becomes stuck as apparently active work.
  UPDATE enrichedlab_agent.broker_research_runs SET status='paused',code='delegation_revoked',
    generation=generation+1,lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND status IN ('queued','running','waiting')
      AND NOT enrichedlab_agent.broker_research_authorized(tenant_id,actor_id,project_id,allowed_profiles);
  WITH held AS (
    SELECT p.group_key, h.actor_id FROM enrichedlab_agent.broker_research_runs h
      JOIN enrichedlab_agent.broker_projects p ON p.tenant_id=h.tenant_id AND p.project_id=h.project_id
      WHERE h.tenant_id=p_tenant AND h.status='running' AND h.lease_until>clock_timestamp()),
  by_group AS (SELECT group_key, count(*) AS n FROM held GROUP BY group_key),
  by_actor AS (SELECT actor_id, count(*) AS n FROM held GROUP BY actor_id)
  SELECT q.* INTO r FROM enrichedlab_agent.broker_research_runs q
    JOIN enrichedlab_agent.broker_projects g ON g.tenant_id=q.tenant_id AND g.project_id=q.project_id
    LEFT JOIN by_group ON by_group.group_key=g.group_key
    LEFT JOIN by_actor ON by_actor.actor_id=q.actor_id
    WHERE q.tenant_id=p_tenant AND q.status IN ('queued','waiting','running') AND q.available_at<=clock_timestamp()
      AND (q.lease_until IS NULL OR q.lease_until<=clock_timestamp())
    ORDER BY coalesce(by_group.n,0)-floor(extract(epoch FROM clock_timestamp()-q.available_at)/120),
      coalesce(by_actor.n,0), q.available_at, q.created_at
    FOR UPDATE OF q SKIP LOCKED LIMIT 1;
  IF NOT FOUND THEN RETURN NULL; END IF;
  UPDATE enrichedlab_agent.broker_research_runs SET status='running',generation=generation+1,lease_owner=p_worker,
    lease_until=clock_timestamp()+make_interval(secs=>p_seconds),updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=r.run_id RETURNING * INTO r;
  project:=enrichedlab_agent.broker_get_project(p_tenant,r.actor_id,r.project_id);
  SELECT coalesce(jsonb_agg(jsonb_build_object('key',f.profile_key,'name',f.name,'description',f.description,'tool_bundle',f.tool_bundle)
      || CASE WHEN t.profile_key IS NULL THEN '{}'::jsonb ELSE jsonb_build_object('template',
        jsonb_build_object('template_ref',t.template_ref,'block_path',t.block_path,'contract',t.contract)) END),'[]')
    INTO profiles FROM enrichedlab_agent.broker_agent_profiles f
    LEFT JOIN enrichedlab_agent.broker_profile_templates t ON t.tenant_id=f.tenant_id AND t.profile_key=f.profile_key
    WHERE f.tenant_id=p_tenant AND f.profile_key=ANY(r.allowed_profiles) AND f.enabled;
  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id',t.task_id,'action_key',pt.action_key,'status',t.status,
    'result_class',t.result_class,'error_code',t.error_code,
    'evidence_refs',(SELECT coalesce(jsonb_agg('artifact://tasks/'||t.task_id::text||'/'||a.artifact_id::text),'[]') FROM (
      SELECT artifact_id FROM enrichedlab_agent.broker_artifacts WHERE tenant_id=p_tenant AND task_id=t.task_id AND visibility='researcher' ORDER BY artifact_id LIMIT 64
    ) a))),'[]') INTO facts
    FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND EXISTS (
      SELECT 1 FROM jsonb_array_elements(project->'checkpoint'->'state'->'plan') s WHERE s->>'task_id'=t.task_id::text
    );
  RETURN jsonb_build_object('run',to_jsonb(r)-'tenant_id','project',project,'profiles',profiles,'task_facts',facts);
END $$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_claim_research_run(text,text,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (46) ON CONFLICT DO NOTHING;
COMMIT;
