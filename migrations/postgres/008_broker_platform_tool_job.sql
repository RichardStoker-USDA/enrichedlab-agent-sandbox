BEGIN;

-- This is only a core-side receipt binding. The platform broker remains the
-- catalog and execution authority; no tool result or provenance event is made
-- by this migration.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_platform_tool_jobs (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  principal_id uuid NOT NULL,
  worker_id text NOT NULL,
  request_id uuid NOT NULL,
  conversation_id uuid NOT NULL,
  platform_job_id text NOT NULL,
  tool_id text NOT NULL,
  catalog_digest text NOT NULL,
  input_sha256 text NOT NULL,
  accepted_at timestamptz NOT NULL,
  bound_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation),
  UNIQUE (tenant_id, platform_job_id),
  FOREIGN KEY (tenant_id, task_id)
    REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, principal_id)
    REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (attempt > 0),
  CHECK (lease_generation > 0),
  CHECK (worker_id ~ '^[a-zA-Z0-9._:-]{1,128}$'),
  CHECK (platform_job_id ~ '^job_[0-9a-z]{26,64}$'),
  CHECK (tool_id ~ '^[a-z][a-z0-9_]{2,63}$'),
  CHECK (catalog_digest ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (input_sha256 ~ '^[0-9a-f]{64}$')
);

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_bind_platform_tool_job(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_attempt integer,
  p_generation bigint,
  p_request uuid,
  p_conversation uuid,
  p_job_id text,
  p_tool_id text,
  p_catalog_digest text,
  p_input_sha256 text,
  p_accepted_at timestamptz
)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_task enrichedlab_agent.broker_tasks%ROWTYPE;
BEGIN
  IF p_attempt < 1 OR p_generation < 1
     OR p_worker !~ '^[a-zA-Z0-9._:-]{1,128}$'
     OR p_job_id !~ '^job_[0-9a-z]{26,64}$'
     OR p_tool_id !~ '^[a-z][a-z0-9_]{2,63}$'
     OR p_catalog_digest !~ '^sha256:[0-9a-f]{64}$'
     OR p_input_sha256 !~ '^[0-9a-f]{64}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:platform_tool_binding_invalid';
  END IF;

  SELECT * INTO v_task
  FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant
    AND task_id = p_task
    AND lease_owner = p_worker
    AND attempt = p_attempt
    AND lease_generation = p_generation
    AND lease_expires_at > CURRENT_TIMESTAMP
    AND status IN ('leased', 'starting', 'running')
    AND desired_status = 'run'
  FOR UPDATE;
  IF NOT FOUND THEN
    RETURN false;
  END IF;

  INSERT INTO enrichedlab_agent.broker_task_platform_tool_jobs (
    tenant_id, task_id, attempt, lease_generation, principal_id, worker_id,
    request_id, conversation_id, platform_job_id, tool_id, catalog_digest,
    input_sha256, accepted_at
  ) VALUES (
    p_tenant, p_task, p_attempt, p_generation, v_task.principal_id, p_worker,
    p_request, p_conversation, p_job_id, p_tool_id, p_catalog_digest,
    p_input_sha256, p_accepted_at
  ) ON CONFLICT (tenant_id, task_id, attempt, lease_generation) DO NOTHING;

  RETURN EXISTS (
    SELECT 1
    FROM enrichedlab_agent.broker_task_platform_tool_jobs AS jobs
    WHERE jobs.tenant_id = p_tenant
      AND jobs.task_id = p_task
      AND jobs.attempt = p_attempt
      AND jobs.lease_generation = p_generation
      AND jobs.principal_id = v_task.principal_id
      AND jobs.worker_id = p_worker
      AND jobs.request_id = p_request
      AND jobs.conversation_id = p_conversation
      AND jobs.platform_job_id = p_job_id
      AND jobs.tool_id = p_tool_id
      AND jobs.catalog_digest = p_catalog_digest
      AND jobs.input_sha256 = p_input_sha256
      AND jobs.accepted_at = p_accepted_at
  );
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT p_version IN (2, 3, 7, 8) AND EXISTS (
    SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = p_version
  );
$$;

REVOKE ALL ON TABLE enrichedlab_agent.broker_task_platform_tool_jobs FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_bind_platform_tool_job(text,uuid,text,integer,bigint,uuid,uuid,text,text,text,text,timestamptz) FROM PUBLIC;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (8) ON CONFLICT DO NOTHING;
COMMIT;
