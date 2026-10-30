-- Research run lanes. A run is `accepted` or `candidate`, fixed when it is inserted: `candidate` when its actor holds an
-- operator-written candidate grant that has not expired, otherwise `accepted`. A coordinator claims one lane only, so a
-- candidate coordinator (same image, its own lead configuration) can be measured on real lab rows while the accepted
-- coordinator keeps every other run. The workspace API starts runs as before; it never names a lane.
-- Rollback: an older coordinator calls the three-argument claim, which from here claims accepted runs only, so it
-- never runs a candidate run; the column, the grants table and the trigger can stay.
BEGIN;

ALTER TABLE enrichedlab_agent.broker_research_runs ADD COLUMN IF NOT EXISTS lane text NOT NULL DEFAULT 'accepted';
DO $migration$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname='broker_research_runs_lane_check'
      AND conrelid='enrichedlab_agent.broker_research_runs'::regclass) THEN
    ALTER TABLE enrichedlab_agent.broker_research_runs
      ADD CONSTRAINT broker_research_runs_lane_check CHECK (lane IN ('accepted','candidate'));
  END IF;
END $migration$;

-- Who starts candidate runs, until when. Written by the operator only: no role may read or change it.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_candidate_grants (
  tenant_id text NOT NULL,
  actor_id uuid NOT NULL,
  until timestamptz NOT NULL,
  note text NOT NULL CHECK (length(note) BETWEEN 1 AND 200),
  granted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id,actor_id),
  FOREIGN KEY (tenant_id,actor_id) REFERENCES enrichedlab_agent.broker_principals
);
ALTER TABLE enrichedlab_agent.broker_candidate_grants ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_candidate_grants FROM PUBLIC;

-- A run's lane is set at insert from its actor's grant and never changes.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_lane()
RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF TG_OP='INSERT' THEN
    NEW.lane := CASE WHEN EXISTS (SELECT 1 FROM enrichedlab_agent.broker_candidate_grants g
        WHERE g.tenant_id=NEW.tenant_id AND g.actor_id=NEW.actor_id AND g.until>clock_timestamp())
      THEN 'candidate' ELSE 'accepted' END;
  ELSIF NEW.lane IS DISTINCT FROM OLD.lane THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:run_lane_fixed';
  END IF;
  RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_lane() FROM PUBLIC;
DROP TRIGGER IF EXISTS research_run_lane ON enrichedlab_agent.broker_research_runs;
CREATE TRIGGER research_run_lane BEFORE INSERT OR UPDATE OF lane ON enrichedlab_agent.broker_research_runs
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_research_run_lane();

-- The workspace API's run listings stay as they were: the lane is the operator's measurement setting, not part of a
-- person's view of a run. Migrations 011 and 014's bodies, with the lane left out.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_research_runs(p_tenant text,p_actor uuid,p_project uuid)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  RETURN QUERY SELECT to_jsonb(r)-'tenant_id'-'lease_owner'-'lease_until'-'lane' FROM enrichedlab_agent.broker_research_runs r
    WHERE tenant_id=p_tenant AND project_id=p_project ORDER BY created_at DESC LIMIT 100;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_package_snapshot(p_tenant text,p_actor uuid,p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE record_bytes bigint; task_total bigint; event_total bigint; artifact_total bigint;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  SELECT coalesce((SELECT sum(octet_length(c.state::text)) FROM enrichedlab_agent.broker_project_checkpoints c
      WHERE c.tenant_id=p_tenant AND c.project_id=p_project),0)
    + coalesce((SELECT sum(octet_length(t.record::text)) FROM enrichedlab_agent.broker_research_turns t
      JOIN enrichedlab_agent.broker_research_runs r ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project),0)
    INTO record_bytes;
  SELECT count(*) INTO task_total FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=p_project;
  SELECT count(*) INTO event_total FROM enrichedlab_agent.broker_task_events e
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id=e.tenant_id AND pt.task_id=e.task_id
    WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project;
  SELECT count(*) INTO artifact_total FROM enrichedlab_agent.broker_artifacts a
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id=a.tenant_id AND pt.task_id=a.task_id
    WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project AND a.visibility='researcher';
  -- A package is complete or refused. It never silently truncates a record class.
  IF record_bytes > 48*1024*1024 OR task_total > 2000 OR event_total > 200000 OR artifact_total > 20000 THEN
    RAISE EXCEPTION USING ERRCODE='54000',MESSAGE='broker:project_package_too_large'; END IF;
  RETURN jsonb_build_object(
    'schema','enrichedlab.project-package-snapshot.v1',
    'snapshot_at',now(),
    'requester',jsonb_build_object(
      'principal_id',p_actor,
      'capabilities',(SELECT coalesce(jsonb_agg(c),'[]'::jsonb) FROM unnest(ARRAY['read','review','contribute','steer','cancel','manage']) c
        WHERE enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,c)),
      'inspection_expires_at',(SELECT i.expires_at FROM enrichedlab_agent.broker_project_inspections i
        WHERE i.tenant_id=p_tenant AND i.project_id=p_project AND i.principal_id=p_actor AND i.expires_at>clock_timestamp())),
    'project',(SELECT jsonb_build_object('project_id',p.project_id,'title',p.title,'group_key',p.group_key,'group_name',g.name,
        'visibility',p.visibility,'owner_id',p.owner_id,'revision',p.revision,'created_at',p.created_at,'updated_at',p.updated_at)
      FROM enrichedlab_agent.broker_projects p
      JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id=p.tenant_id AND g.group_key=p.group_key
      WHERE p.tenant_id=p_tenant AND p.project_id=p_project),
    'checkpoints',(SELECT coalesce(jsonb_agg(jsonb_build_object('revision',c.revision,'author_id',c.author_id,'request_id',c.request_id,
        'created_at',c.created_at,'state',c.state) ORDER BY c.revision),'[]'::jsonb)
      FROM enrichedlab_agent.broker_project_checkpoints c WHERE c.tenant_id=p_tenant AND c.project_id=p_project),
    'research_runs',(SELECT coalesce(jsonb_agg(jsonb_build_object(
        'run',to_jsonb(r)-'tenant_id'-'lease_owner'-'lease_until'-'lane',
        'turns',(SELECT coalesce(jsonb_agg(to_jsonb(t)-'tenant_id' ORDER BY t.turn),'[]'::jsonb)
          FROM enrichedlab_agent.broker_research_turns t WHERE t.tenant_id=r.tenant_id AND t.run_id=r.run_id),
        'events',(SELECT coalesce(jsonb_agg(to_jsonb(e)-'tenant_id' ORDER BY e.sequence),'[]'::jsonb)
          FROM enrichedlab_agent.broker_research_run_events e WHERE e.tenant_id=r.tenant_id AND e.run_id=r.run_id))
        ORDER BY r.created_at,r.run_id),'[]'::jsonb)
      FROM enrichedlab_agent.broker_research_runs r WHERE r.tenant_id=p_tenant AND r.project_id=p_project),
    'access_history',(SELECT coalesce(jsonb_agg(to_jsonb(a)-'tenant_id' ORDER BY a.occurred_at,a.event_id),'[]'::jsonb)
      FROM enrichedlab_agent.broker_project_audit a WHERE a.tenant_id=p_tenant AND a.project_id=p_project),
    'tasks',(SELECT coalesce(jsonb_agg(jsonb_build_object(
        'task_id',t.task_id,'action_key',pt.action_key,'parent_task_id',pt.parent_task_id,'principal_id',t.principal_id,
        'group_key',t.group_key,'profile',t.profile_key,'objective',t.objective,'status',t.status,'attempt',t.attempt,
        'max_task_attempts',t.max_task_attempts,'lease_generation',t.lease_generation,'result_class',t.result_class,
        'error_code',t.error_code,'submitted_at',t.submitted_at,'started_at',t.started_at,'finished_at',t.finished_at,
        'updated_at',t.updated_at,'request_sha256',t.request_sha256,
        'execution',jsonb_build_object('image_ref',t.profile_snapshot->'image_ref','cpu_limit',t.profile_snapshot->'cpu_limit',
          'memory_limit',t.profile_snapshot->'memory_limit','max_run_seconds',t.profile_snapshot->'max_run_seconds',
          'role',t.profile_snapshot->'role','tool_bundle',t.profile_snapshot->'tool_bundle','harness',t.profile_snapshot->'harness'),
        'events',(SELECT coalesce(jsonb_agg(jsonb_build_object('sequence',e.sequence,'kind',e.kind,'state',e.state,'code',e.code,
            'metadata',e.metadata,'occurred_at',e.occurred_at) ORDER BY e.sequence),'[]'::jsonb)
          FROM enrichedlab_agent.broker_task_events e WHERE e.tenant_id=t.tenant_id AND e.task_id=t.task_id),
        'artifacts',(SELECT coalesce(jsonb_agg(jsonb_build_object('artifact_id',a.artifact_id,'role',a.role,'label',a.label,
            'relative_path',a.relative_path,'media_type',a.media_type,'size_bytes',a.size_bytes,'sha256',a.sha256,
            'created_at',a.created_at) ORDER BY a.created_at,a.artifact_id),'[]'::jsonb)
          FROM enrichedlab_agent.broker_artifacts a WHERE a.tenant_id=t.tenant_id AND a.task_id=t.task_id AND a.visibility='researcher'),
        'recoveries',(SELECT coalesce(jsonb_agg(jsonb_build_object('recovery_id',v.recovery_id,'recovered_at',v.recovered_at,
            'receipt',v.receipt) ORDER BY v.recovered_at,v.recovery_id),'[]'::jsonb)
          FROM enrichedlab_agent.broker_artifact_recoveries v WHERE v.tenant_id=t.tenant_id AND v.task_id=t.task_id),
        'scientific_history',(SELECT coalesce(jsonb_agg(jsonb_build_object('attempt',h.attempt,'lease_generation',h.lease_generation,
            'first_segment',h.first_segment,'last_segment',h.last_segment,'bound_at',h.bound_at,
            'retained',EXISTS(SELECT 1 FROM enrichedlab_agent.provenance_runs pr WHERE pr.tenant_id=h.tenant_id
              AND pr.run_id=h.state_run_id AND pr.subject=h.subject)) ORDER BY h.attempt,h.lease_generation),'[]'::jsonb)
          FROM enrichedlab_agent.broker_task_scientific_history h WHERE h.tenant_id=t.tenant_id AND h.task_id=t.task_id))
        ORDER BY t.submitted_at,t.task_id),'[]'::jsonb)
      FROM enrichedlab_agent.broker_project_tasks pt
      JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=pt.tenant_id AND t.task_id=pt.task_id
      WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project),
    'counts',jsonb_build_object('tasks',task_total,'task_events',event_total,'artifacts',artifact_total));
END $$;

-- Migration 048's claim, for one lane.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_claim_research_run(p_tenant text,p_worker text,p_seconds integer,p_lane text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project jsonb; profiles jsonb; facts jsonb; pages jsonb;
  newest jsonb;
BEGIN
  IF p_seconds NOT BETWEEN 30 AND 600 OR length(p_worker) NOT BETWEEN 1 AND 100 OR p_lane IS NULL
     OR p_lane NOT IN ('accepted','candidate') THEN
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
    WHERE q.tenant_id=p_tenant AND q.lane=p_lane AND q.status IN ('queued','waiting','running') AND q.available_at<=clock_timestamp()
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
  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id',t.task_id,'action_key',pt.action_key,'profile',t.profile_key,'status',t.status,
    'result_class',t.result_class,'error_code',t.error_code,
    'evidence_refs',(SELECT coalesce(jsonb_agg('artifact://tasks/'||t.task_id::text||'/'||a.artifact_id::text),'[]') FROM (
      SELECT artifact_id FROM enrichedlab_agent.broker_artifacts WHERE tenant_id=p_tenant AND task_id=t.task_id AND visibility='researcher' ORDER BY artifact_id LIMIT 64
    ) a))),'[]') INTO facts
    FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND EXISTS (
      SELECT 1 FROM jsonb_array_elements(project->'checkpoint'->'state'->'plan') s WHERE s->>'task_id'=t.task_id::text
    );
  -- The project's archived steps: every archived action key, so none is planned again, their analyses
  -- (still this project's own work), the counts by status and by method, and the newest sixteen with their titles.
  WITH archived AS (
    SELECT pp.page, s.n, s.v FROM enrichedlab_agent.broker_plan_pages pp
      CROSS JOIN LATERAL jsonb_array_elements(pp.steps) WITH ORDINALITY AS s(v, n)
      WHERE pp.tenant_id=p_tenant AND pp.project_id=r.project_id),
  methods AS (
    SELECT t.profile_key AS method, a.v->>'status' AS status, count(*) AS n FROM archived a
      JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=p_tenant AND t.task_id=(a.v->>'task_id')::uuid
      GROUP BY 1, 2)
  SELECT jsonb_build_object(
      'pages', (SELECT count(DISTINCT page) FROM archived),
      'steps', (SELECT count(*) FROM archived),
      'keys', coalesce((SELECT jsonb_agg(k.action_key ORDER BY k.page, k.ordinal)
        FROM enrichedlab_agent.broker_plan_archived_keys k WHERE k.tenant_id=p_tenant AND k.project_id=r.project_id), '[]'::jsonb),
      'tasks', coalesce((SELECT jsonb_agg(k.task_id ORDER BY k.page, k.ordinal)
        FROM enrichedlab_agent.broker_plan_archived_keys k WHERE k.tenant_id=p_tenant AND k.project_id=r.project_id
          AND k.task_id IS NOT NULL), '[]'::jsonb),
      'by_status', coalesce((SELECT jsonb_object_agg(status, c ORDER BY status) FROM (
        SELECT v->>'status' AS status, count(*) AS c FROM archived GROUP BY 1) x), '{}'::jsonb),
      'by_method', coalesce((SELECT jsonb_object_agg(method, statuses ORDER BY method) FROM (
        SELECT method, jsonb_object_agg(status, n ORDER BY status) AS statuses FROM methods GROUP BY method) y), '{}'::jsonb),
      'newest', coalesce((SELECT jsonb_agg(jsonb_build_object('action_key', v->>'action_key', 'title', v->>'title',
        'status', v->>'status') ORDER BY page DESC, n DESC) FROM (
        SELECT * FROM archived ORDER BY page DESC, n DESC LIMIT 16) z), '[]'::jsonb))
    INTO pages;
  -- The project's 64 newest analyses by submission, in the working plan or archived, oldest first: the coordinator reads
  -- their queries so none is proposed again. Archival keeps steps an earlier finding reads, so plan order is not age
  -- order. The archive's newest 64 come from its index and the plan holds at most
  -- 128, so the cost does not grow with the project's history.
  SELECT coalesce(jsonb_agg(x.task_id ORDER BY x.submitted_at, x.task_id), '[]'::jsonb) INTO newest FROM (
    SELECT u.task_id, u.submitted_at FROM (
        (SELECT k.task_id, k.submitted_at FROM enrichedlab_agent.broker_plan_archived_keys k
          WHERE k.tenant_id=p_tenant AND k.project_id=r.project_id AND k.task_id IS NOT NULL
          ORDER BY k.submitted_at DESC, k.task_id DESC LIMIT 64)
        UNION
        SELECT t.task_id, t.submitted_at FROM jsonb_array_elements(project->'checkpoint'->'state'->'plan') s
          JOIN enrichedlab_agent.broker_project_tasks pt
            ON pt.tenant_id=p_tenant AND pt.task_id=(s->>'task_id')::uuid AND pt.project_id=r.project_id
          JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=p_tenant AND t.task_id=pt.task_id
          WHERE s->>'task_id' IS NOT NULL) u
      ORDER BY u.submitted_at DESC, u.task_id DESC LIMIT 64) x;
  RETURN jsonb_build_object('run',to_jsonb(r)-'tenant_id','project',project,'profiles',profiles,'task_facts',facts,
    'plan_pages',pages,'newest_tasks',newest);
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_claim_research_run(text,text,integer,text) FROM PUBLIC;

-- The three-argument claim every earlier coordinator calls claims accepted runs only.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_claim_research_run(p_tenant text,p_worker text,p_seconds integer)
RETURNS jsonb LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT enrichedlab_agent.broker_claim_research_run(p_tenant,p_worker,p_seconds,'accepted')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_claim_research_run(text,text,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (85) ON CONFLICT DO NOTHING;
COMMIT;
