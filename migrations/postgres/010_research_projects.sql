BEGIN;

-- Projects are durable collaboration scopes. Existing unbound tasks stay private.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_projects (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL DEFAULT gen_random_uuid(),
  group_key text NOT NULL,
  owner_id uuid NOT NULL,
  title text NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 160),
  visibility text NOT NULL CHECK (visibility IN ('private','lab')),
  revision bigint NOT NULL DEFAULT 0,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, project_id),
  FOREIGN KEY (tenant_id, group_key) REFERENCES enrichedlab_agent.broker_research_groups,
  FOREIGN KEY (tenant_id, owner_id) REFERENCES enrichedlab_agent.broker_principals
);
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_project_grants (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  principal_id uuid NOT NULL,
  capabilities text[] NOT NULL CHECK (capabilities <@ ARRAY['read','review','contribute','steer','cancel']::text[]),
  PRIMARY KEY (tenant_id, project_id, principal_id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects,
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals
);
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_project_tasks (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  task_id uuid NOT NULL,
  action_key text NOT NULL CHECK (action_key ~ '^[a-zA-Z0-9_-]{1,100}$'),
  parent_task_id uuid,
  PRIMARY KEY (tenant_id, task_id),
  UNIQUE (tenant_id, project_id, action_key),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects,
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks,
  FOREIGN KEY (tenant_id, parent_task_id) REFERENCES enrichedlab_agent.broker_project_tasks
);
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_project_checkpoints (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  revision bigint NOT NULL,
  author_id uuid NOT NULL,
  request_id uuid NOT NULL,
  state jsonb NOT NULL CHECK (jsonb_typeof(state) = 'object' AND octet_length(state::text) <= 131072),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, project_id, revision),
  UNIQUE (tenant_id, project_id, request_id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects,
  FOREIGN KEY (tenant_id, author_id) REFERENCES enrichedlab_agent.broker_principals
);
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_project_audit (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  event_id uuid NOT NULL DEFAULT gen_random_uuid(),
  actor_id uuid NOT NULL,
  kind text NOT NULL,
  detail jsonb NOT NULL,
  occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, event_id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects,
  FOREIGN KEY (tenant_id, actor_id) REFERENCES enrichedlab_agent.broker_principals
);
-- This enrollment table has no runtime write function. The operator enrolls a
-- verified human principal; neither group leadership nor an agent grants it.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_project_administrators (
  tenant_id text NOT NULL,
  principal_id uuid NOT NULL,
  PRIMARY KEY (tenant_id, principal_id),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals
);
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_project_inspections (
  tenant_id text NOT NULL,
  project_id uuid NOT NULL,
  principal_id uuid NOT NULL,
  expires_at timestamptz NOT NULL,
  PRIMARY KEY (tenant_id, project_id, principal_id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects,
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_project_administrators
);

DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_projects','broker_project_grants','broker_project_tasks',
    'broker_project_checkpoints','broker_project_audit','broker_project_administrators','broker_project_inspections'] LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I ENABLE ROW LEVEL SECURITY',t);
    EXECUTE format('REVOKE ALL ON enrichedlab_agent.%I FROM PUBLIC',t);
  END LOOP;
  FOREACH t IN ARRAY ARRAY['broker_project_checkpoints','broker_project_audit','broker_project_tasks'] LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.%I',t);
    EXECUTE format('CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.%I FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation()',t);
  END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_can(p_tenant text,p_actor uuid,p_project uuid,p_capability text)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_projects p
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=p.tenant_id AND a.principal_id=p_actor AND a.enabled
    JOIN enrichedlab_agent.broker_tenants t ON t.tenant_id=p.tenant_id AND t.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id=p.tenant_id AND g.group_key=p.group_key AND g.enabled
    WHERE p.tenant_id=p_tenant AND p.project_id=p_project AND (
      (EXISTS (SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
        WHERE m.tenant_id=p.tenant_id AND m.group_key=p.group_key AND m.principal_id=p_actor)
       AND (p.owner_id=p_actor OR
         (p_capability='read' AND p.visibility='lab') OR
         EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_grants r
           WHERE r.tenant_id=p.tenant_id AND r.project_id=p.project_id AND r.principal_id=p_actor
             AND (p_capability=ANY(r.capabilities) OR (p_capability='read' AND cardinality(r.capabilities)>0)))))
      OR (p_capability='read' AND EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_project_inspections i
        JOIN enrichedlab_agent.broker_project_administrators d USING (tenant_id,principal_id)
        WHERE i.tenant_id=p.tenant_id AND i.project_id=p.project_id AND i.principal_id=p_actor AND i.expires_at>statement_timestamp()))
    ) AND p_capability IN ('read','review','contribute','steer','cancel','manage')
  )
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_can(p_tenant text,p_actor uuid,p_task uuid,p_capability text)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
 SELECT EXISTS (
   SELECT 1 FROM enrichedlab_agent.broker_tasks t
   LEFT JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id=t.tenant_id AND pt.task_id=t.task_id
   WHERE t.tenant_id=p_tenant AND t.task_id=p_task AND (
     (pt.project_id IS NOT NULL AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,pt.project_id,p_capability))
     OR (pt.project_id IS NULL AND t.principal_id=p_actor AND EXISTS (
       SELECT 1 FROM enrichedlab_agent.broker_principals a
       JOIN enrichedlab_agent.broker_tenants tenant ON tenant.tenant_id=a.tenant_id AND tenant.enabled
       JOIN enrichedlab_agent.broker_group_memberships m ON m.tenant_id=a.tenant_id AND m.principal_id=a.principal_id AND m.group_key=t.group_key
       JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id=m.tenant_id AND g.group_key=m.group_key AND g.enabled
       WHERE a.tenant_id=p_tenant AND a.principal_id=p_actor AND a.enabled
     ))
   ) AND p_capability IN ('read','cancel')
 )
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_create_project(p_tenant text,p_actor uuid,p_id uuid,p_group text,p_title text,p_visibility text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE old enrichedlab_agent.broker_projects%ROWTYPE;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_get_groups(p_tenant,p_actor) WHERE group_key=p_group)
     OR NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_get_principal(p_tenant,p_actor)) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:group_forbidden';
  END IF;
  INSERT INTO enrichedlab_agent.broker_projects(tenant_id,project_id,group_key,owner_id,title,visibility)
  VALUES(p_tenant,p_id,p_group,p_actor,p_title,p_visibility) ON CONFLICT DO NOTHING;
  SELECT * INTO old FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=p_id;
  IF old.owner_id<>p_actor OR old.group_key<>p_group OR old.title<>p_title OR old.visibility<>p_visibility THEN
    RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:idempotency_conflict';
  END IF;
  RETURN p_id;
END $$;

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
    'task_count',(SELECT count(*) FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=p_id)
  );
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_projects(p_tenant text,p_actor uuid,p_limit integer)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF p_limit NOT BETWEEN 1 AND 200 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:list_limit_invalid'; END IF;
  RETURN QUERY SELECT (to_jsonb(p)-'tenant_id') FROM enrichedlab_agent.broker_projects p
    WHERE p.tenant_id=p_tenant AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,'read')
    ORDER BY p.updated_at DESC,p.project_id LIMIT p_limit;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_grant_project(p_tenant text,p_actor uuid,p_id uuid,p_member uuid,p_capabilities text[])
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=p_id FOR UPDATE;
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_id,'manage') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  IF NOT EXISTS(SELECT 1 FROM enrichedlab_agent.broker_projects p JOIN enrichedlab_agent.broker_group_memberships m USING(tenant_id,group_key)
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=m.tenant_id AND a.principal_id=m.principal_id AND a.enabled
    WHERE p.tenant_id=p_tenant AND p.project_id=p_id AND m.principal_id=p_member) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:group_forbidden';
  END IF;
  INSERT INTO enrichedlab_agent.broker_project_grants VALUES(p_tenant,p_id,p_member,p_capabilities)
    ON CONFLICT(tenant_id,project_id,principal_id) DO UPDATE SET capabilities=excluded.capabilities;
  INSERT INTO enrichedlab_agent.broker_project_audit(tenant_id,project_id,actor_id,kind,detail)
    VALUES(p_tenant,p_id,p_actor,'access_changed',jsonb_build_object('member_id',p_member,'capabilities',p_capabilities));
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_inspect_project(p_tenant text,p_actor uuid,p_id uuid,p_reason text)
RETURNS timestamptz LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE expires timestamptz := clock_timestamp()+interval '30 minutes';
BEGIN
  IF length(btrim(p_reason)) NOT BETWEEN 8 AND 500 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:inspection_reason_invalid';
  END IF;
  IF NOT EXISTS(SELECT 1 FROM enrichedlab_agent.broker_project_administrators a JOIN enrichedlab_agent.broker_principals p USING(tenant_id,principal_id)
    WHERE a.tenant_id=p_tenant AND a.principal_id=p_actor AND p.enabled AND p.issuer<>'kubernetes://serviceaccount')
    OR NOT EXISTS(SELECT 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=p_id) THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  INSERT INTO enrichedlab_agent.broker_project_inspections VALUES(p_tenant,p_id,p_actor,expires)
    ON CONFLICT(tenant_id,project_id,principal_id) DO UPDATE SET expires_at=excluded.expires_at;
  INSERT INTO enrichedlab_agent.broker_project_audit(tenant_id,project_id,actor_id,kind,detail)
    VALUES(p_tenant,p_id,p_actor,'admin_inspection',jsonb_build_object('reason',p_reason,'expires_at',expires));
  RETURN expires;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_checkpoint_project(p_tenant text,p_actor uuid,p_id uuid,p_expected bigint,p_request uuid,p_state jsonb)
RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE current_revision bigint; old enrichedlab_agent.broker_project_checkpoints%ROWTYPE; step jsonb;
BEGIN
  SELECT revision INTO current_revision FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=p_id FOR UPDATE;
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_id,'steer') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  SELECT * INTO old FROM enrichedlab_agent.broker_project_checkpoints WHERE tenant_id=p_tenant AND project_id=p_id AND request_id=p_request;
  IF FOUND THEN
    IF old.author_id<>p_actor OR old.state<>p_state OR old.revision<>p_expected+1 THEN
      RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:idempotency_conflict';
    END IF;
    RETURN old.revision;
  END IF;
  IF current_revision<>p_expected THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  IF p_state->>'schema_version' IS DISTINCT FROM '1' OR jsonb_typeof(p_state->'plan') IS DISTINCT FROM 'array' THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:checkpoint_invalid'; END IF;
  FOR step IN SELECT value FROM jsonb_array_elements(p_state->'plan') LOOP
    IF (step->>'status'='completed' AND step->>'task_id' IS NULL) OR (step->>'task_id' IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
      WHERE pt.tenant_id=p_tenant AND pt.project_id=p_id AND pt.task_id=(step->>'task_id')::uuid
        AND pt.action_key=step->>'action_key'
        AND (step->>'status'<>'completed' OR t.status IN ('completed','review_required'))
    )) THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:checkpoint_invalid'; END IF;
  END LOOP;
  INSERT INTO enrichedlab_agent.broker_project_checkpoints VALUES(p_tenant,p_id,current_revision+1,p_actor,p_request,p_state,clock_timestamp());
  UPDATE enrichedlab_agent.broker_projects SET revision=revision+1,updated_at=clock_timestamp() WHERE tenant_id=p_tenant AND project_id=p_id;
  RETURN current_revision+1;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_history(p_tenant text,p_actor uuid,p_id uuid,p_before bigint,p_limit integer)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_id,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  IF p_limit NOT BETWEEN 1 AND 100 OR p_before<0 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:list_limit_invalid'; END IF;
  RETURN QUERY SELECT to_jsonb(c)-'tenant_id' FROM enrichedlab_agent.broker_project_checkpoints c
    WHERE c.tenant_id=p_tenant AND c.project_id=p_id AND (p_before=0 OR c.revision<p_before) ORDER BY c.revision DESC LIMIT p_limit;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_audit_page(p_tenant text,p_actor uuid,p_id uuid,p_limit integer)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_id,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  IF p_limit NOT BETWEEN 1 AND 200 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:list_limit_invalid'; END IF;
  RETURN QUERY SELECT to_jsonb(a)-'tenant_id' FROM enrichedlab_agent.broker_project_audit a
    WHERE a.tenant_id=p_tenant AND a.project_id=p_id ORDER BY occurred_at DESC,event_id LIMIT p_limit;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_submit_project_task(p_tenant text,p_actor uuid,p_project uuid,p_action text,p_parent uuid,p_group text,p_profile text,p_objective text,p_key text,p_sha text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE task uuid; prior enrichedlab_agent.broker_tasks%ROWTYPE;
BEGIN
  -- Serializes action keys across collaborators and checkpoints. The worker can
  -- see the task only after the binding and admission transaction commits.
  PERFORM 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=p_project AND group_key=p_group FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'contribute') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found'; END IF;
  SELECT t.* INTO prior FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project AND pt.action_key=p_action;
  IF FOUND THEN
    IF prior.request_sha256<>p_sha THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:idempotency_conflict'; END IF;
    RETURN prior.task_id;
  END IF;
  IF p_parent IS NOT NULL AND NOT EXISTS(SELECT 1 FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=p_project AND task_id=p_parent) THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:task_not_found'; END IF;
  task:=enrichedlab_agent.broker_submit_task(p_tenant,p_actor,p_group,p_profile,p_objective,p_key,p_sha);
  IF EXISTS(SELECT 1 FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND task_id=task) THEN
    RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:idempotency_conflict'; END IF;
  INSERT INTO enrichedlab_agent.broker_project_tasks VALUES(p_tenant,p_project,task,p_action,p_parent);
  UPDATE enrichedlab_agent.broker_projects SET updated_at=clock_timestamp() WHERE tenant_id=p_tenant AND project_id=p_project;
  RETURN task;
END $$;


-- Admission and cancellation use the current project and catalog grant, never
-- authority copied into a checkpoint or a model prompt.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_execution_allowed(p_tenant text,p_task uuid)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
 SELECT EXISTS(
   SELECT 1 FROM enrichedlab_agent.broker_tasks t
   JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=t.tenant_id AND a.principal_id=t.principal_id AND a.enabled
   JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id=t.tenant_id AND g.group_key=t.group_key AND g.enabled
   JOIN enrichedlab_agent.broker_group_memberships m ON m.tenant_id=t.tenant_id AND m.group_key=t.group_key AND m.principal_id=t.principal_id
   JOIN enrichedlab_agent.broker_agent_profiles p ON p.tenant_id=t.tenant_id AND p.profile_key=t.profile_key AND p.enabled
   JOIN enrichedlab_agent.broker_group_profile_grants r ON r.tenant_id=t.tenant_id AND r.group_key=t.group_key AND r.profile_key=t.profile_key
   JOIN enrichedlab_agent.broker_tenants tenant ON tenant.tenant_id=t.tenant_id AND tenant.enabled
   LEFT JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id=t.tenant_id AND pt.task_id=t.task_id
   WHERE t.tenant_id=p_tenant AND t.task_id=p_task
     AND (pt.project_id IS NULL OR enrichedlab_agent.broker_project_can(p_tenant,t.principal_id,pt.project_id,'contribute'))
 )
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_execution_allowed(text,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_claim_task(
  p_tenant text,
  p_worker text,
  p_lease_seconds integer
)
RETURNS TABLE(
  task_id uuid, principal_id uuid, group_key text, profile_key text,
  objective text, logical_agent text, lease_generation bigint, attempt integer,
  max_task_attempts integer, max_provider_attempts integer, harness text, thinking text,
  route text, controller_profile text,
  gateway_profile text, image_ref text, cpu_limit text, memory_limit text,
  state_profile text, role text, max_run_seconds integer
)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_task enrichedlab_agent.broker_tasks%ROWTYPE;
BEGIN
  IF p_lease_seconds < 60 OR p_lease_seconds > 900 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:lease_invalid';
  END IF;

  WITH expired AS (
    SELECT tasks.tenant_id, tasks.task_id
    FROM enrichedlab_agent.broker_tasks AS tasks
    WHERE tasks.tenant_id = p_tenant
      AND tasks.status IN ('leased', 'starting', 'running', 'cancelling')
      AND tasks.lease_expires_at < clock_timestamp()
    FOR UPDATE SKIP LOCKED
  ), updated AS (
    UPDATE enrichedlab_agent.broker_tasks AS tasks
    SET status = CASE
          WHEN tasks.desired_status = 'cancel' THEN 'cancelled'
          WHEN tasks.attempt >= tasks.max_task_attempts THEN 'failed'
          ELSE 'queued'
        END,
        result_class = CASE WHEN tasks.attempt >= tasks.max_task_attempts THEN 'lease_exhausted' ELSE tasks.result_class END,
        error_code = CASE WHEN tasks.attempt >= tasks.max_task_attempts THEN 'worker_lease_expired' ELSE tasks.error_code END,
        lease_owner = NULL,
        lease_expires_at = NULL,
        finished_at = CASE
          WHEN tasks.desired_status = 'cancel' OR tasks.attempt >= tasks.max_task_attempts
          THEN clock_timestamp() ELSE NULL END,
        updated_at = clock_timestamp(),
        event_sequence = tasks.event_sequence + 1
    FROM expired
    WHERE tasks.tenant_id = expired.tenant_id AND tasks.task_id = expired.task_id
    RETURNING tasks.tenant_id, tasks.task_id, tasks.event_sequence, tasks.status
  )
  INSERT INTO enrichedlab_agent.broker_task_events (
    tenant_id, task_id, sequence, kind, state, code
  )
  SELECT updated.tenant_id, updated.task_id, updated.event_sequence,
         'lifecycle', updated.status,
         CASE WHEN updated.status = 'queued' THEN 'lease_expired_requeued'
              WHEN updated.status = 'cancelled' THEN 'lease_expired_cancelled'
              ELSE 'lease_attempts_exhausted' END
  FROM updated;

  SELECT tasks.* INTO v_task
  FROM enrichedlab_agent.broker_tasks AS tasks
  JOIN enrichedlab_agent.broker_research_groups AS groups
    ON groups.tenant_id = tasks.tenant_id AND groups.group_key = tasks.group_key
  WHERE tasks.tenant_id = p_tenant
    AND tasks.status = 'queued'
    AND tasks.desired_status = 'run'
    AND tasks.attempt < tasks.max_task_attempts
    AND groups.enabled
    AND enrichedlab_agent.broker_execution_allowed(p_tenant,tasks.task_id)
    AND (
      SELECT count(*)
      FROM enrichedlab_agent.broker_tasks AS active
      WHERE active.tenant_id = tasks.tenant_id
        AND active.group_key = tasks.group_key
        AND active.status IN ('leased', 'starting', 'running', 'cancelling')
        AND active.lease_expires_at >= clock_timestamp()
    ) < groups.max_active_agents
    AND (
      SELECT count(*)
      FROM enrichedlab_agent.broker_tasks AS active
      WHERE active.tenant_id = tasks.tenant_id
        AND active.group_key = tasks.group_key
        AND active.principal_id = tasks.principal_id
        AND active.status IN ('leased', 'starting', 'running', 'cancelling')
        AND active.lease_expires_at >= clock_timestamp()
    ) < groups.max_active_agents_per_principal
  ORDER BY tasks.submitted_at, tasks.task_id
  FOR UPDATE OF tasks SKIP LOCKED
  LIMIT 1;

  IF NOT FOUND THEN
    RETURN;
  END IF;

  UPDATE enrichedlab_agent.broker_tasks AS tasks
  SET status = 'leased',
      attempt = tasks.attempt + 1,
      lease_owner = p_worker,
      lease_generation = tasks.lease_generation + 1,
      lease_expires_at = clock_timestamp() + make_interval(secs => p_lease_seconds),
      updated_at = clock_timestamp(),
      event_sequence = tasks.event_sequence + 1
  WHERE tasks.tenant_id = p_tenant AND tasks.task_id = v_task.task_id
  RETURNING tasks.* INTO v_task;

  INSERT INTO enrichedlab_agent.broker_task_events (
    tenant_id, task_id, sequence, kind, state, code,
    metadata
  ) VALUES (
    p_tenant, v_task.task_id, v_task.event_sequence,
    'lifecycle', 'leased', 'task_claimed',
    jsonb_build_object('attempt', v_task.attempt, 'lease_generation', v_task.lease_generation)
  );

  RETURN QUERY SELECT
    v_task.task_id, v_task.principal_id, v_task.group_key, v_task.profile_key,
    v_task.objective, v_task.logical_agent, v_task.lease_generation, v_task.attempt,
    v_task.max_task_attempts,
    (v_task.profile_snapshot->>'max_provider_attempts')::integer,
    v_task.profile_snapshot->>'harness', v_task.profile_snapshot->>'thinking',
    v_task.profile_snapshot->>'route', v_task.profile_snapshot->>'controller_profile',
    v_task.profile_snapshot->>'gateway_profile', v_task.profile_snapshot->>'image_ref',
    v_task.profile_snapshot->>'cpu_limit', v_task.profile_snapshot->>'memory_limit',
    v_task.profile_snapshot->>'state_profile', v_task.profile_snapshot->>'role',
    (v_task.profile_snapshot->>'max_run_seconds')::integer;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_cancel_requested(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint
)
RETURNS boolean
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT coalesce((
    SELECT desired_status = 'cancel' OR NOT enrichedlab_agent.broker_execution_allowed(p_tenant,p_task)
    FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id = p_tenant
      AND task_id = p_task
      AND lease_owner = p_worker
      AND lease_generation = p_generation
  ), true)
$$;

-- Existing task-read functions are replaced below with the same signatures.

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_tasks(
  p_tenant text,
  p_principal uuid,
  p_limit integer
)
RETURNS TABLE(
  task_id uuid, group_key text, profile_key text, status text, objective text,
  attempt integer, max_task_attempts integer, max_provider_attempts integer,
  logical_agent text, result_class text,
  error_code text, submitted_at timestamptz, updated_at timestamptz,
  started_at timestamptz, finished_at timestamptz
)
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF p_limit < 1 OR p_limit > 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:list_limit_invalid';
  END IF;
  RETURN QUERY
  SELECT tasks.task_id, tasks.group_key, tasks.profile_key, tasks.status,
         NULL::text, tasks.attempt, tasks.max_task_attempts,
         (tasks.profile_snapshot->>'max_provider_attempts')::integer, tasks.logical_agent,
         tasks.result_class, tasks.error_code, tasks.submitted_at,
         tasks.updated_at, tasks.started_at, tasks.finished_at
  FROM enrichedlab_agent.broker_tasks AS tasks
  WHERE tasks.tenant_id = p_tenant AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,tasks.task_id,'read')
  ORDER BY tasks.submitted_at DESC, tasks.task_id
  LIMIT p_limit;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_task(
  p_tenant text,
  p_principal uuid,
  p_task uuid
)
RETURNS TABLE(
  task_id uuid, group_key text, profile_key text, status text, objective text,
  attempt integer, max_task_attempts integer, max_provider_attempts integer,
  logical_agent text, result_class text,
  error_code text, submitted_at timestamptz, updated_at timestamptz,
  started_at timestamptz, finished_at timestamptz
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT tasks.task_id, tasks.group_key, tasks.profile_key, tasks.status,
         tasks.objective, tasks.attempt, tasks.max_task_attempts,
         (tasks.profile_snapshot->>'max_provider_attempts')::integer, tasks.logical_agent,
         tasks.result_class, tasks.error_code, tasks.submitted_at,
         tasks.updated_at, tasks.started_at, tasks.finished_at
  FROM enrichedlab_agent.broker_tasks AS tasks
  WHERE tasks.tenant_id = p_tenant
    AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,tasks.task_id,'read')
    AND tasks.task_id = p_task
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_cancel(
  p_tenant text,
  p_principal uuid,
  p_task uuid
)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_status text;
  v_sequence bigint;
BEGIN
  SELECT status INTO v_status
  FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND task_id = p_task AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,p_task,'cancel')
  FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  IF v_status IN ('completed', 'failed', 'cancelled', 'review_required') THEN
    RETURN v_status;
  END IF;
  UPDATE enrichedlab_agent.broker_tasks
  SET desired_status = 'cancel',
      status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
      finished_at = CASE WHEN status = 'queued' THEN clock_timestamp() ELSE finished_at END,
      updated_at = clock_timestamp(),
      event_sequence = event_sequence + 1
  WHERE tenant_id = p_tenant AND task_id = p_task
  RETURNING status, event_sequence INTO v_status, v_sequence;
  INSERT INTO enrichedlab_agent.broker_task_events (
    tenant_id, task_id, sequence, kind, state, code
  ) VALUES (p_tenant, p_task, v_sequence, 'lifecycle', v_status, 'cancel_requested');
  RETURN v_status;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_task_events(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_after bigint,
  p_limit integer
)
RETURNS TABLE(
  sequence bigint, kind text, state text, code text, occurred_at timestamptz
)
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF p_limit < 1 OR p_limit > 1000 OR p_after < 0 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:event_cursor_invalid';
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id = p_tenant AND task_id = p_task AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,p_task,'read')
  ) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  RETURN QUERY
  SELECT events.sequence, events.kind, events.state, events.code, events.occurred_at
  FROM enrichedlab_agent.broker_task_events AS events
  WHERE events.tenant_id = p_tenant
    AND events.task_id = p_task
    AND events.sequence > p_after
  ORDER BY events.sequence
  LIMIT p_limit;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_artifacts(
  p_tenant text,
  p_principal uuid,
  p_task uuid
)
RETURNS TABLE(
  artifact_id uuid, role text, label text, relative_path text, media_type text,
  size_bytes bigint, sha256 text, created_at timestamptz
)
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id = p_tenant AND task_id = p_task AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,p_task,'read')
  ) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;
  RETURN QUERY
  SELECT artifacts.artifact_id, artifacts.role, artifacts.label,
         artifacts.relative_path, artifacts.media_type, artifacts.size_bytes,
         artifacts.sha256, artifacts.created_at
  FROM enrichedlab_agent.broker_artifacts AS artifacts
  WHERE artifacts.tenant_id = p_tenant
    AND artifacts.task_id = p_task
    AND artifacts.visibility = 'researcher'
  ORDER BY artifacts.created_at, artifacts.artifact_id;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_artifact(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_artifact uuid
)
RETURNS TABLE(
  artifact_id uuid, role text, label text, relative_path text, media_type text,
  size_bytes bigint, sha256 text, created_at timestamptz
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT artifacts.artifact_id, artifacts.role, artifacts.label,
         artifacts.relative_path, artifacts.media_type, artifacts.size_bytes,
         artifacts.sha256, artifacts.created_at
  FROM enrichedlab_agent.broker_artifacts AS artifacts
  JOIN enrichedlab_agent.broker_tasks AS tasks
    ON tasks.tenant_id = artifacts.tenant_id AND tasks.task_id = artifacts.task_id
  WHERE artifacts.tenant_id = p_tenant
    AND artifacts.task_id = p_task
    AND artifacts.artifact_id = p_artifact
    AND artifacts.visibility = 'researcher'
    AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,tasks.task_id,'read')
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_scientific_history_status(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_attempt integer DEFAULT NULL,
  p_generation bigint DEFAULT NULL
)
RETURNS TABLE(
  coverage text,
  attempt integer,
  lease_generation bigint,
  state_run_id text
)
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_task enrichedlab_agent.broker_tasks%ROWTYPE;
  v_history enrichedlab_agent.broker_task_scientific_history%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO v_task
  FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND task_id = p_task AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,p_task,'read');
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;

  IF (p_attempt IS NULL) <> (p_generation IS NULL) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:scientific_history_cursor_invalid';
  END IF;
  SELECT * INTO v_history
  FROM enrichedlab_agent.broker_task_scientific_history AS history
  WHERE history.tenant_id = p_tenant
    AND history.task_id = p_task
    AND history.attempt = coalesce(p_attempt, v_task.attempt)
    AND history.lease_generation = coalesce(p_generation, v_task.lease_generation);
  IF NOT FOUND THEN
    RETURN QUERY SELECT 'unavailable'::text, NULL::integer, NULL::bigint, NULL::text;
    RETURN;
  END IF;

  IF EXISTS (
    SELECT 1
    FROM enrichedlab_agent.provenance_runs AS runs
    WHERE runs.tenant_id = p_tenant
      AND runs.run_id = v_history.state_run_id
      AND runs.subject = v_history.subject
  ) THEN
    RETURN QUERY SELECT 'available'::text, v_history.attempt,
      v_history.lease_generation, v_history.state_run_id;
  ELSIF v_task.status IN ('leased', 'starting', 'running', 'cancelling') THEN
    RETURN QUERY SELECT 'pending'::text, v_history.attempt,
      v_history.lease_generation, v_history.state_run_id;
  ELSE
    RETURN QUERY SELECT 'unavailable'::text, v_history.attempt,
      v_history.lease_generation, v_history.state_run_id;
  END IF;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_scientific_history_events(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_attempt integer,
  p_generation bigint,
  p_after bigint,
  p_limit integer
)
RETURNS TABLE(
  sequence bigint,
  event_id uuid,
  segment bigint,
  harness text,
  provider text,
  model text,
  engine text,
  parser text,
  kind text,
  evidence_role text,
  visibility text,
  occurred_at text,
  hash text
)
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_history enrichedlab_agent.broker_task_scientific_history%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF p_limit < 1 OR p_limit > 100 OR p_after < 0 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:scientific_history_cursor_invalid';
  END IF;
  IF NOT EXISTS (
    SELECT 1
    FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id = p_tenant AND task_id = p_task AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,p_task,'read')
  ) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;

  SELECT history.* INTO v_history
  FROM enrichedlab_agent.broker_task_scientific_history AS history
  JOIN enrichedlab_agent.broker_tasks AS tasks
    ON tasks.tenant_id = history.tenant_id AND tasks.task_id = history.task_id
  JOIN enrichedlab_agent.provenance_runs AS runs
    ON runs.tenant_id = history.tenant_id
   AND runs.run_id = history.state_run_id
   AND runs.subject = history.subject
  WHERE history.tenant_id = p_tenant
    AND history.task_id = p_task
    AND history.attempt = p_attempt
    AND history.lease_generation = p_generation;
  IF NOT FOUND THEN
    RETURN;
  END IF;

  RETURN QUERY
  SELECT events.sequence, events.event_id, events.segment, events.harness,
         events.provider, events.model, events.engine, events.parser, events.kind,
         events.evidence_role, events.visibility, events.occurred_at, events.hash
  FROM enrichedlab_agent.provenance_events AS events
  WHERE events.tenant_id = p_tenant
    AND events.run_id = v_history.state_run_id
    AND events.subject = v_history.subject
    AND events.segment >= v_history.first_segment
    AND events.segment <= coalesce(
      v_history.last_segment,
      (
        SELECT min(next_history.first_segment) - 1
        FROM enrichedlab_agent.broker_task_scientific_history AS next_history
        WHERE next_history.tenant_id = v_history.tenant_id
          AND next_history.task_id = v_history.task_id
          AND next_history.state_run_id = v_history.state_run_id
          AND next_history.subject = v_history.subject
          AND next_history.first_segment > v_history.first_segment
      ),
      2147483647
    )
    AND events.sequence > p_after
  ORDER BY events.sequence
  LIMIT p_limit;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_scientific_history_events_with_evidence(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_attempt integer,
  p_generation bigint,
  p_after bigint,
  p_limit integer
)
RETURNS TABLE(
  sequence bigint,
  event_id uuid,
  segment bigint,
  harness text,
  provider text,
  model text,
  engine text,
  parser text,
  kind text,
  evidence_role text,
  visibility text,
  occurred_at text,
  hash text,
  evidence_count bigint
)
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_history enrichedlab_agent.broker_task_scientific_history%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF p_limit < 1 OR p_limit > 100 OR p_after < 0 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:scientific_history_cursor_invalid';
  END IF;
  IF NOT EXISTS (
    SELECT 1
    FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id = p_tenant AND task_id = p_task AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,p_task,'read')
  ) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;

  SELECT history.* INTO v_history
  FROM enrichedlab_agent.broker_task_scientific_history AS history
  JOIN enrichedlab_agent.provenance_runs AS runs
    ON runs.tenant_id = history.tenant_id
   AND runs.run_id = history.state_run_id
   AND runs.subject = history.subject
  WHERE history.tenant_id = p_tenant
    AND history.task_id = p_task
    AND history.attempt = p_attempt
    AND history.lease_generation = p_generation;
  IF NOT FOUND THEN
    RETURN;
  END IF;

  RETURN QUERY
  SELECT events.sequence, events.event_id, events.segment, events.harness,
         events.provider, events.model, events.engine, events.parser, events.kind,
         events.evidence_role, events.visibility, events.occurred_at, events.hash,
         count(evidence.artifact_id)::bigint
  FROM enrichedlab_agent.provenance_events AS events
  LEFT JOIN enrichedlab_agent.broker_task_scientific_evidence AS evidence
    ON evidence.tenant_id = p_tenant
   AND evidence.task_id = p_task
   AND evidence.attempt = p_attempt
   AND evidence.lease_generation = p_generation
   AND evidence.event_id = events.event_id
  WHERE events.tenant_id = p_tenant
    AND events.run_id = v_history.state_run_id
    AND events.subject = v_history.subject
    AND events.segment >= v_history.first_segment
    AND events.segment <= coalesce(
      v_history.last_segment,
      (
        SELECT min(next_history.first_segment) - 1
        FROM enrichedlab_agent.broker_task_scientific_history AS next_history
        WHERE next_history.tenant_id = v_history.tenant_id
          AND next_history.task_id = v_history.task_id
          AND next_history.state_run_id = v_history.state_run_id
          AND next_history.subject = v_history.subject
          AND next_history.first_segment > v_history.first_segment
      ),
      2147483647
    )
    AND events.sequence > p_after
  GROUP BY events.sequence, events.event_id, events.segment, events.harness,
           events.provider, events.model, events.engine, events.parser, events.kind,
           events.evidence_role, events.visibility, events.occurred_at, events.hash
  ORDER BY events.sequence
  LIMIT p_limit;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_scientific_history_evidence(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_attempt integer,
  p_generation bigint,
  p_event uuid,
  p_after uuid DEFAULT NULL,
  p_limit integer DEFAULT 100
)
RETURNS TABLE(
  artifact_id uuid,
  sha256 text,
  size_bytes bigint,
  media_type text,
  role text,
  relation text
)
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_history enrichedlab_agent.broker_task_scientific_history%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF p_limit < 1 OR p_limit > 100 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:scientific_history_evidence_cursor_invalid';
  END IF;
  IF NOT EXISTS (
    SELECT 1
    FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id = p_tenant AND task_id = p_task AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,p_task,'read')
  ) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:task_not_found';
  END IF;

  SELECT history.* INTO v_history
  FROM enrichedlab_agent.broker_task_scientific_history AS history
  JOIN enrichedlab_agent.provenance_runs AS runs
    ON runs.tenant_id = history.tenant_id
   AND runs.run_id = history.state_run_id
   AND runs.subject = history.subject
  WHERE history.tenant_id = p_tenant
    AND history.task_id = p_task
    AND history.attempt = p_attempt
    AND history.lease_generation = p_generation;
  IF NOT FOUND THEN
    RETURN;
  END IF;

  RETURN QUERY
  SELECT artifacts.artifact_id, artifacts.sha256, artifacts.size_bytes,
         artifacts.media_type, evidence.artifact_role, evidence.relation
  FROM enrichedlab_agent.broker_task_scientific_evidence AS evidence
  JOIN enrichedlab_agent.broker_artifacts AS artifacts
    ON artifacts.tenant_id = evidence.tenant_id
   AND artifacts.artifact_id = evidence.artifact_id
   AND artifacts.task_id = evidence.task_id
   AND artifacts.visibility = 'researcher'
   AND artifacts.role = 'state'
   AND artifacts.sha256 = evidence.artifact_sha256
   AND artifacts.size_bytes = evidence.artifact_size_bytes
  JOIN enrichedlab_agent.broker_task_scientific_history AS history
    ON history.tenant_id = evidence.tenant_id
   AND history.task_id = evidence.task_id
   AND history.attempt = evidence.attempt
   AND history.lease_generation = evidence.lease_generation
  JOIN enrichedlab_agent.provenance_events AS events
    ON events.tenant_id = v_history.tenant_id
   AND events.run_id = v_history.state_run_id
   AND events.subject = v_history.subject
   AND events.event_id = evidence.event_id
   AND events.sequence = evidence.event_sequence
   AND events.hash = evidence.event_hash
  WHERE evidence.tenant_id = p_tenant
    AND evidence.task_id = p_task
    AND evidence.attempt = p_attempt
    AND evidence.lease_generation = p_generation
    AND evidence.event_id = p_event
    AND (p_after IS NULL OR artifacts.artifact_id > p_after)
    AND events.segment >= v_history.first_segment
    AND events.segment <= coalesce(
      v_history.last_segment,
      (
        SELECT min(next_history.first_segment) - 1
        FROM enrichedlab_agent.broker_task_scientific_history AS next_history
        WHERE next_history.tenant_id = v_history.tenant_id
          AND next_history.task_id = v_history.task_id
          AND next_history.state_run_id = v_history.state_run_id
          AND next_history.subject = v_history.subject
          AND next_history.first_segment > v_history.first_segment
      ),
      2147483647
    )
    AND (
      events.kind = 'agent.tool.finished'
      OR (
        events.kind = 'agent.tool.observed'
        AND events.metadata ->> 'status' IN ('completed', 'error', 'failed')
      )
    )
  ORDER BY artifacts.artifact_id
  LIMIT p_limit;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
 SELECT p_version IN (2,3,7,8,9,10) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_can(text,uuid,uuid,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_can(text,uuid,uuid,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_create_project(text,uuid,uuid,text,text,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_project(text,uuid,uuid) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_projects(text,uuid,integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_grant_project(text,uuid,uuid,uuid,text[]) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_inspect_project(text,uuid,uuid,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_checkpoint_project(text,uuid,uuid,bigint,uuid,jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_history(text,uuid,uuid,bigint,integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_audit_page(text,uuid,uuid,integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_submit_project_task(text,uuid,uuid,text,uuid,text,text,text,text,text) FROM PUBLIC;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES(10) ON CONFLICT DO NOTHING;
COMMIT;
