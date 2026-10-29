-- Scientist review: retained coordinator deferrals, administrator audit, collaborator
-- listing, and one consistent read snapshot for the project evidence package.
BEGIN;

-- Every coordinator deferral, including rejected model replies, stays visible after
-- the run's failure counter resets. Rows are append-only machine records.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_research_run_events (
  tenant_id text NOT NULL,
  run_id uuid NOT NULL,
  sequence bigint NOT NULL CHECK (sequence > 0),
  code text NOT NULL CHECK (code ~ '^[a-z][a-z0-9_]{1,63}$'),
  reason text CHECK (reason IS NULL OR reason ~ '^[a-z][a-z0-9_]{1,63}$'),
  generation bigint NOT NULL,
  turn integer NOT NULL,
  failures integer NOT NULL,
  status_after text NOT NULL,
  occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, run_id, sequence),
  FOREIGN KEY (tenant_id, run_id) REFERENCES enrichedlab_agent.broker_research_runs
);

-- Tenant-level administrator actions that are not tied to one project.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_admin_audit (
  tenant_id text NOT NULL,
  event_id uuid NOT NULL DEFAULT gen_random_uuid(),
  actor_id uuid NOT NULL,
  kind text NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{1,63}$'),
  detail jsonb NOT NULL CHECK (jsonb_typeof(detail) = 'object'),
  occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, event_id),
  FOREIGN KEY (tenant_id, actor_id) REFERENCES enrichedlab_agent.broker_principals
);

DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_research_run_events','broker_admin_audit'] LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I ENABLE ROW LEVEL SECURITY',t);
    EXECUTE format('REVOKE ALL ON enrichedlab_agent.%I FROM PUBLIC',t);
    EXECUTE format('DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.%I',t);
    EXECUTE format('CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.%I FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation()',t);
  END LOOP;
END $$;

-- The reason is a fixed code chosen by the coordinator, never provider or model text.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_yield_research_run(p_tenant text,p_run uuid,p_worker text,p_generation bigint,p_code text,p_reason text)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; next_sequence bigint;
  previous record;
BEGIN
  IF p_code IS NULL OR p_code NOT IN ('waiting_for_tools','model_unavailable','proposal_invalid','checkpoint_changed','turn_limit','review_required')
     OR (p_reason IS NOT NULL AND p_reason !~ '^[a-z][a-z0-9_]{1,63}$') THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
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
  INSERT INTO enrichedlab_agent.broker_research_run_events(tenant_id,run_id,sequence,code,reason,generation,turn,failures,status_after)
    VALUES(p_tenant,p_run,next_sequence,p_code,p_reason,p_generation,r.turn,r.failures,r.status);
END $$;

-- The earlier signature stays valid for a coordinator that has not been upgraded.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_yield_research_run(p_tenant text,p_run uuid,p_worker text,p_generation bigint,p_code text)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  PERFORM enrichedlab_agent.broker_yield_research_run(p_tenant,p_run,p_worker,p_generation,p_code,NULL::text);
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_event_history(p_tenant text,p_actor uuid,p_run uuid,p_after bigint,p_limit integer)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_research_runs r WHERE r.tenant_id=p_tenant AND r.run_id=p_run
    AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,r.project_id,'read')) THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  IF p_after<0 OR p_limit NOT BETWEEN 1 AND 200 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:list_limit_invalid'; END IF;
  RETURN QUERY SELECT to_jsonb(e)-'tenant_id' FROM enrichedlab_agent.broker_research_run_events e
    WHERE e.tenant_id=p_tenant AND e.run_id=p_run AND e.sequence>p_after ORDER BY e.sequence LIMIT p_limit;
END $$;

-- Administration is a separate enrollment. Group leadership, project ownership and
-- workload identities never qualify.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_is_project_administrator(p_tenant text,p_actor uuid)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_project_administrators a
    JOIN enrichedlab_agent.broker_principals p ON p.tenant_id=a.tenant_id AND p.principal_id=a.principal_id AND p.enabled
    JOIN enrichedlab_agent.broker_tenants t ON t.tenant_id=a.tenant_id AND t.enabled
    WHERE a.tenant_id=p_tenant AND a.principal_id=p_actor AND p.issuer<>'kubernetes://serviceaccount')
$$;

-- Listing titles of private projects is itself an administrator action, so every
-- directory read is audited. Contents stay behind a reasoned, expiring inspection.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_admin_project_directory(p_tenant text,p_actor uuid,p_limit integer)
RETURNS SETOF jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE total bigint;
BEGIN
  IF NOT enrichedlab_agent.broker_is_project_administrator(p_tenant,p_actor) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:administrator_required'; END IF;
  IF p_limit NOT BETWEEN 1 AND 500 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:list_limit_invalid'; END IF;
  SELECT count(*) INTO total FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant;
  INSERT INTO enrichedlab_agent.broker_admin_audit(tenant_id,actor_id,kind,detail)
    VALUES(p_tenant,p_actor,'project_directory_viewed',jsonb_build_object('project_count',total,'limit',p_limit));
  RETURN QUERY SELECT jsonb_build_object(
      'project_id',p.project_id,'title',p.title,'group_key',p.group_key,'group_name',g.name,'visibility',p.visibility,
      'owner',jsonb_build_object('principal_id',o.principal_id,'display_name',o.display_name,'email',o.email,
        'kind',CASE WHEN o.issuer='kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END),
      'created_at',p.created_at,'updated_at',p.updated_at,'revision',p.revision,
      'task_count',(SELECT count(*) FROM enrichedlab_agent.broker_project_tasks pt WHERE pt.tenant_id=p.tenant_id AND pt.project_id=p.project_id),
      'inspection_expires_at',(SELECT i.expires_at FROM enrichedlab_agent.broker_project_inspections i
        WHERE i.tenant_id=p.tenant_id AND i.project_id=p.project_id AND i.principal_id=p_actor AND i.expires_at>clock_timestamp()),
      'readable',enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,'read'))
    FROM enrichedlab_agent.broker_projects p
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id=p.tenant_id AND g.group_key=p.group_key
    JOIN enrichedlab_agent.broker_principals o ON o.tenant_id=p.tenant_id AND o.principal_id=p.owner_id
    WHERE p.tenant_id=p_tenant ORDER BY p.updated_at DESC,p.project_id LIMIT p_limit;
END $$;

-- The project view reports an active inspection so the client can show the read-only
-- notice after a reload. It is NULL for every other kind of access.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_project(p_tenant text,p_actor uuid,p_id uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE result jsonb;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_id,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  SELECT to_jsonb(p)-'tenant_id' INTO result FROM enrichedlab_agent.broker_projects p WHERE tenant_id=p_tenant AND project_id=p_id;
  RETURN result || jsonb_build_object(
    'capabilities',(SELECT coalesce(jsonb_agg(c),'[]') FROM unnest(ARRAY['read','review','contribute','steer','cancel','manage']) c WHERE enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_id,c)),
    'checkpoint',(SELECT jsonb_build_object('revision',c.revision,'author_id',c.author_id,'created_at',c.created_at,'state',c.state) FROM enrichedlab_agent.broker_project_checkpoints c WHERE c.tenant_id=p_tenant AND c.project_id=p_id ORDER BY revision DESC LIMIT 1),
    'tasks',(SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.submitted_at DESC,t.task_id DESC),'[]') FROM (
      SELECT task_id,pt.action_key,pt.parent_task_id,t.profile_key AS profile,t.status,t.attempt,t.updated_at,t.submitted_at
      FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
      WHERE pt.tenant_id=p_tenant AND pt.project_id=p_id ORDER BY t.submitted_at DESC,t.task_id DESC LIMIT 200
    ) t),
    'task_count',(SELECT count(*) FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=p_id),
    'inspection_expires_at',(SELECT i.expires_at FROM enrichedlab_agent.broker_project_inspections i
      WHERE i.tenant_id=p_tenant AND i.project_id=p_id AND i.principal_id=p_actor AND i.expires_at>clock_timestamp())
  );
END $$;

-- An administrator can close their own inspection before it expires.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_end_project_inspection(p_tenant text,p_actor uuid,p_project uuid)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE ended timestamptz := clock_timestamp();
BEGIN
  UPDATE enrichedlab_agent.broker_project_inspections SET expires_at=ended
    WHERE tenant_id=p_tenant AND project_id=p_project AND principal_id=p_actor AND expires_at>ended;
  IF NOT FOUND THEN RETURN false; END IF;
  INSERT INTO enrichedlab_agent.broker_project_audit(tenant_id,project_id,actor_id,kind,detail)
    VALUES(p_tenant,p_project,p_actor,'admin_inspection_ended',jsonb_build_object('ended_at',ended));
  RETURN true;
END $$;

-- The owner sees who holds access and which group members can be invited. Workload
-- identities can hold qualification grants but are never offered as invitees.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_members(p_tenant text,p_actor uuid,p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE project enrichedlab_agent.broker_projects%ROWTYPE;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'manage') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  SELECT * INTO project FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=p_project;
  RETURN jsonb_build_object(
    'owner',(SELECT jsonb_build_object('principal_id',a.principal_id,'display_name',a.display_name,'email',a.email)
      FROM enrichedlab_agent.broker_principals a WHERE a.tenant_id=p_tenant AND a.principal_id=project.owner_id),
    'grants',(SELECT coalesce(jsonb_agg(jsonb_build_object('principal_id',a.principal_id,'display_name',a.display_name,'email',a.email,
        'kind',CASE WHEN a.issuer='kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END,
        'enabled',a.enabled,'capabilities',to_jsonb(r.capabilities)) ORDER BY lower(coalesce(a.display_name,a.email)),a.principal_id),'[]'::jsonb)
      FROM enrichedlab_agent.broker_project_grants r
      JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=r.tenant_id AND a.principal_id=r.principal_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project AND cardinality(r.capabilities)>0),
    'eligible',(SELECT coalesce(jsonb_agg(jsonb_build_object('principal_id',a.principal_id,'display_name',a.display_name,'email',a.email)
        ORDER BY lower(coalesce(a.display_name,a.email)),a.principal_id),'[]'::jsonb)
      FROM enrichedlab_agent.broker_group_memberships m
      JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=m.tenant_id AND a.principal_id=m.principal_id AND a.enabled
      WHERE m.tenant_id=p_tenant AND m.group_key=project.group_key AND a.principal_id<>project.owner_id
        AND a.issuer<>'kubernetes://serviceaccount'
        AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_grants r WHERE r.tenant_id=p_tenant
          AND r.project_id=p_project AND r.principal_id=a.principal_id AND cardinality(r.capabilities)>0)));
END $$;

-- One consistent read of every retained project record needed for an offline evidence
-- package. STABLE functions see a single snapshot for the whole call. Artifact bytes
-- stay on disk and are read and verified by the API against these rows.
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
        'run',to_jsonb(r)-'tenant_id'-'lease_owner'-'lease_until',
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

-- The API records each completed package build in the project's access history.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_project_export(p_tenant text,p_actor uuid,p_project uuid,p_detail jsonb)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE event uuid;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  -- Every comparison is NULL-safe: a missing field must refuse, not pass.
  IF jsonb_typeof(p_detail) IS DISTINCT FROM 'object'
     OR EXISTS (SELECT 1 FROM jsonb_object_keys(p_detail) k WHERE k NOT IN ('package_sha256','manifest_sha256','bytes','files','snapshot_at','format'))
     OR NOT coalesce((p_detail->>'package_sha256') ~ '^[0-9a-f]{64}$', false)
     OR NOT coalesce((p_detail->>'manifest_sha256') ~ '^[0-9a-f]{64}$', false)
     OR jsonb_typeof(p_detail->'bytes') IS DISTINCT FROM 'number'
     OR jsonb_typeof(p_detail->'files') IS DISTINCT FROM 'number'
     OR jsonb_typeof(p_detail->'snapshot_at') IS DISTINCT FROM 'string'
     OR (p_detail->>'format') IS DISTINCT FROM 'tar+gzip' THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:project_export_invalid'; END IF;
  INSERT INTO enrichedlab_agent.broker_project_audit(tenant_id,project_id,actor_id,kind,detail)
    VALUES(p_tenant,p_project,p_actor,'evidence_package_exported',p_detail) RETURNING event_id INTO event;
  RETURN event;
END $$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE n.nspname='enrichedlab_agent' AND p.proname IN ('broker_yield_research_run','broker_research_run_event_history',
      'broker_is_project_administrator','broker_admin_project_directory','broker_end_project_inspection',
      'broker_project_members','broker_project_package_snapshot','broker_record_project_export')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (14) ON CONFLICT DO NOTHING;
COMMIT;
