BEGIN;

-- A terminal custodian may recover only its own exact running lease. This is a
-- snapshot read after runtime terminal observation, never a second broker action.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_terminal_custody_task(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_attempt integer,
  p_generation bigint
)
RETURNS TABLE(
  task_id uuid, principal_id uuid, group_key text, profile_key text,
  objective text, logical_agent text, lease_generation bigint, attempt integer,
  max_task_attempts integer, max_provider_attempts integer, harness text, thinking text,
  route text, controller_profile text, gateway_profile text, image_ref text,
  cpu_limit text, memory_limit text, state_profile text, role text, max_run_seconds integer
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT
    tasks.task_id,
    tasks.principal_id,
    tasks.group_key,
    tasks.profile_key,
    ''::text AS objective,
    tasks.logical_agent,
    tasks.lease_generation,
    tasks.attempt,
    tasks.max_task_attempts,
    (tasks.profile_snapshot->>'max_provider_attempts')::integer,
    tasks.profile_snapshot->>'harness',
    tasks.profile_snapshot->>'thinking',
    tasks.profile_snapshot->>'route',
    tasks.profile_snapshot->>'controller_profile',
    tasks.profile_snapshot->>'gateway_profile',
    tasks.profile_snapshot->>'image_ref',
    tasks.profile_snapshot->>'cpu_limit',
    tasks.profile_snapshot->>'memory_limit',
    tasks.profile_snapshot->>'state_profile',
    tasks.profile_snapshot->>'role',
    (tasks.profile_snapshot->>'max_run_seconds')::integer
  FROM enrichedlab_agent.broker_tasks AS tasks
  WHERE tasks.tenant_id = p_tenant
    AND tasks.task_id = p_task
    AND tasks.lease_owner = p_worker
    AND tasks.attempt = p_attempt
    AND tasks.lease_generation = p_generation
    AND tasks.status = 'running'
    AND tasks.desired_status = 'run'
    AND tasks.lease_expires_at > CURRENT_TIMESTAMP
$$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_terminal_custody_task(text,uuid,text,integer,bigint)
FROM PUBLIC;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (6) ON CONFLICT DO NOTHING;
COMMIT;
