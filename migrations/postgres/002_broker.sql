CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_tenants (
  tenant_id text PRIMARY KEY,
  name text NOT NULL,
  enabled boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  CHECK (tenant_id ~ '^[a-z][a-z0-9_-]{1,63}$'),
  CHECK (length(name) BETWEEN 1 AND 160)
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_principals (
  tenant_id text NOT NULL,
  principal_id uuid NOT NULL DEFAULT gen_random_uuid(),
  issuer text NOT NULL,
  subject text NOT NULL,
  email text NOT NULL,
  display_name text,
  enabled boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  last_seen_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, principal_id),
  UNIQUE (tenant_id, issuer, subject),
  FOREIGN KEY (tenant_id) REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  CHECK (length(issuer) BETWEEN 1 AND 512),
  CHECK (length(subject) BETWEEN 1 AND 512),
  CHECK (length(email) BETWEEN 3 AND 320),
  CHECK (display_name IS NULL OR length(display_name) <= 512)
);

CREATE INDEX IF NOT EXISTS broker_principals_email
  ON enrichedlab_agent.broker_principals (tenant_id, lower(email));

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_research_groups (
  tenant_id text NOT NULL,
  group_key text NOT NULL,
  name text NOT NULL,
  max_active_agents integer NOT NULL,
  max_active_agents_per_principal integer NOT NULL,
  max_queued_tasks integer NOT NULL,
  retention_days integer NOT NULL,
  enabled boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, group_key),
  FOREIGN KEY (tenant_id) REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  CHECK (group_key ~ '^[a-z][a-z0-9_-]{1,63}$'),
  CHECK (length(name) BETWEEN 1 AND 160),
  CHECK (max_active_agents BETWEEN 1 AND 1024),
  CHECK (max_active_agents_per_principal BETWEEN 1 AND max_active_agents),
  CHECK (max_queued_tasks BETWEEN 1 AND 10000),
  CHECK (retention_days BETWEEN 1 AND 3650)
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_group_memberships (
  tenant_id text NOT NULL,
  group_key text NOT NULL,
  principal_id uuid NOT NULL,
  role text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, group_key, principal_id),
  FOREIGN KEY (tenant_id, group_key)
    REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, principal_id)
    REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (role IN ('researcher', 'lead', 'reviewer'))
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_principal_invitations (
  tenant_id text NOT NULL,
  email text NOT NULL,
  group_key text NOT NULL,
  role text NOT NULL,
  enabled boolean NOT NULL DEFAULT true,
  consumed_at timestamptz,
  consumed_by uuid,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, email, group_key),
  FOREIGN KEY (tenant_id, group_key)
    REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, consumed_by)
    REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (email = lower(email)),
  CHECK (length(email) BETWEEN 3 AND 320),
  CHECK (role IN ('researcher', 'lead', 'reviewer')),
  CHECK ((consumed_at IS NULL) = (consumed_by IS NULL))
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_agent_profiles (
  tenant_id text NOT NULL,
  profile_key text NOT NULL,
  name text NOT NULL,
  description text NOT NULL,
  harness text NOT NULL,
  thinking text,
  route text NOT NULL,
  controller_profile text NOT NULL,
  gateway_profile text NOT NULL,
  image_ref text NOT NULL,
  cpu_limit text NOT NULL,
  memory_limit text NOT NULL,
  state_profile text NOT NULL,
  role text NOT NULL,
  tool_bundle text NOT NULL,
  max_run_seconds integer NOT NULL,
  max_task_attempts integer NOT NULL,
  max_provider_attempts integer NOT NULL,
  enabled boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, profile_key),
  FOREIGN KEY (tenant_id) REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  CHECK (profile_key ~ '^[a-z][a-z0-9_-]{1,63}$'),
  CHECK (length(name) BETWEEN 1 AND 160),
  CHECK (length(description) BETWEEN 1 AND 2000),
  CHECK (harness IN ('open-code', 'pi')),
  CHECK (thinking IS NULL OR thinking IN ('off', 'low', 'medium', 'on', 'xhigh')),
  CHECK (image_ref LIKE '%@sha256:%'),
  CHECK (max_run_seconds BETWEEN 30 AND 604800),
  CHECK (max_task_attempts BETWEEN 1 AND 8),
  CHECK (max_provider_attempts BETWEEN 1 AND 8)
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_group_profile_grants (
  tenant_id text NOT NULL,
  group_key text NOT NULL,
  profile_key text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, group_key, profile_key),
  FOREIGN KEY (tenant_id, group_key)
    REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, profile_key)
    REFERENCES enrichedlab_agent.broker_agent_profiles (tenant_id, profile_key)
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_tasks (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL DEFAULT gen_random_uuid(),
  principal_id uuid NOT NULL,
  group_key text NOT NULL,
  profile_key text NOT NULL,
  objective text NOT NULL,
  status text NOT NULL DEFAULT 'queued',
  desired_status text NOT NULL DEFAULT 'run',
  attempt integer NOT NULL DEFAULT 0,
  max_task_attempts integer NOT NULL,
  logical_agent text NOT NULL,
  profile_snapshot jsonb NOT NULL,
  idempotency_key text NOT NULL,
  request_sha256 text NOT NULL,
  lease_owner text,
  lease_generation bigint NOT NULL DEFAULT 0,
  lease_expires_at timestamptz,
  event_sequence bigint NOT NULL DEFAULT 0,
  result_class text,
  error_code text,
  submitted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  started_at timestamptz,
  finished_at timestamptz,
  PRIMARY KEY (tenant_id, task_id),
  UNIQUE (tenant_id, principal_id, idempotency_key),
  FOREIGN KEY (tenant_id, principal_id)
    REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  FOREIGN KEY (tenant_id, group_key)
    REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, profile_key)
    REFERENCES enrichedlab_agent.broker_agent_profiles (tenant_id, profile_key),
  CHECK (length(objective) BETWEEN 1 AND 8192),
  CHECK (status IN (
    'queued', 'leased', 'starting', 'running', 'cancelling',
    'completed', 'failed', 'cancelled', 'review_required'
  )),
  CHECK (desired_status IN ('run', 'cancel')),
  CHECK (attempt >= 0 AND attempt <= max_task_attempts),
  CHECK (max_task_attempts BETWEEN 1 AND 8),
  CHECK (logical_agent ~ '^task-[0-9a-f]{32}$'),
  CHECK (length(idempotency_key) BETWEEN 1 AND 128),
  CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK ((lease_owner IS NULL) = (lease_expires_at IS NULL))
);

CREATE INDEX IF NOT EXISTS broker_tasks_claim
  ON enrichedlab_agent.broker_tasks (tenant_id, status, submitted_at);
CREATE INDEX IF NOT EXISTS broker_tasks_principal
  ON enrichedlab_agent.broker_tasks (tenant_id, principal_id, submitted_at DESC);
CREATE INDEX IF NOT EXISTS broker_tasks_active_group
  ON enrichedlab_agent.broker_tasks (tenant_id, group_key, status, lease_expires_at);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_events (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  sequence bigint NOT NULL,
  kind text NOT NULL,
  state text NOT NULL,
  code text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, sequence),
  FOREIGN KEY (tenant_id, task_id)
    REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  CHECK (length(kind) BETWEEN 1 AND 128),
  CHECK (length(state) BETWEEN 1 AND 64),
  CHECK (length(code) BETWEEN 1 AND 128),
  CHECK (jsonb_typeof(metadata) = 'object')
);

CREATE INDEX IF NOT EXISTS broker_task_events_time
  ON enrichedlab_agent.broker_task_events (tenant_id, task_id, occurred_at, sequence);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_artifacts (
  tenant_id text NOT NULL,
  artifact_id uuid NOT NULL DEFAULT gen_random_uuid(),
  task_id uuid NOT NULL,
  visibility text NOT NULL DEFAULT 'researcher',
  role text NOT NULL,
  label text NOT NULL,
  relative_path text NOT NULL,
  media_type text NOT NULL,
  size_bytes bigint NOT NULL,
  sha256 text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, artifact_id),
  UNIQUE (tenant_id, task_id, relative_path),
  FOREIGN KEY (tenant_id, task_id)
    REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  CHECK (visibility IN ('researcher', 'operator')),
  CHECK (role IN ('request', 'stdout', 'stderr', 'result', 'workspace', 'state', 'evidence')),
  CHECK (length(label) BETWEEN 1 AND 256),
  CHECK (length(relative_path) BETWEEN 1 AND 1024),
  CHECK (relative_path !~ '(^|/)\.\.(/|$)' AND relative_path !~ '^/'),
  CHECK (length(media_type) BETWEEN 1 AND 256),
  CHECK (size_bytes >= 0),
  CHECK (sha256 ~ '^[0-9a-f]{64}$')
);

ALTER TABLE enrichedlab_agent.broker_tenants ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_principals ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_research_groups ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_group_memberships ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_principal_invitations ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_agent_profiles ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_group_profile_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_tasks ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_task_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_artifacts ENABLE ROW LEVEL SECURITY;

CREATE OR REPLACE FUNCTION enrichedlab_agent.reject_broker_event_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  RAISE EXCEPTION 'broker task events are append-only';
END;
$$;

DROP TRIGGER IF EXISTS broker_task_events_no_update ON enrichedlab_agent.broker_task_events;
CREATE TRIGGER broker_task_events_no_update
  BEFORE UPDATE ON enrichedlab_agent.broker_task_events
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

DROP TRIGGER IF EXISTS broker_task_events_no_delete ON enrichedlab_agent.broker_task_events;
CREATE TRIGGER broker_task_events_no_delete
  BEFORE DELETE ON enrichedlab_agent.broker_task_events
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_resolve_principal(
  p_tenant text,
  p_issuer text,
  p_subject text,
  p_email text,
  p_display_name text
)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_principal uuid;
  v_email text := lower(p_email);
BEGIN
  SELECT principal_id INTO v_principal
  FROM enrichedlab_agent.broker_principals
  WHERE tenant_id = p_tenant AND issuer = p_issuer AND subject = p_subject
  FOR UPDATE;

  IF v_principal IS NULL THEN
    IF NOT EXISTS (
      SELECT 1
      FROM enrichedlab_agent.broker_principal_invitations
      WHERE tenant_id = p_tenant
        AND email = v_email
        AND enabled
        AND consumed_at IS NULL
    ) THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:not_invited';
    END IF;
    INSERT INTO enrichedlab_agent.broker_principals (
      tenant_id, issuer, subject, email, display_name
    ) VALUES (
      p_tenant, p_issuer, p_subject, v_email, nullif(p_display_name, '')
    )
    RETURNING principal_id INTO v_principal;
  ELSE
    IF NOT EXISTS (
      SELECT 1
      FROM enrichedlab_agent.broker_principal_invitations
      WHERE tenant_id = p_tenant
        AND email = v_email
        AND enabled
    ) THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:not_invited';
    END IF;
    UPDATE enrichedlab_agent.broker_principals
    SET email = v_email,
        display_name = nullif(p_display_name, ''),
        last_seen_at = clock_timestamp(),
        updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND principal_id = v_principal AND enabled;
    IF NOT FOUND THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:principal_disabled';
    END IF;
  END IF;

  DELETE FROM enrichedlab_agent.broker_group_memberships AS memberships
  WHERE memberships.tenant_id = p_tenant
    AND memberships.principal_id = v_principal
    AND NOT EXISTS (
      SELECT 1
      FROM enrichedlab_agent.broker_principal_invitations AS invitations
      WHERE invitations.tenant_id = memberships.tenant_id
        AND invitations.group_key = memberships.group_key
        AND invitations.email = v_email
        AND invitations.enabled
    );

  INSERT INTO enrichedlab_agent.broker_group_memberships (
    tenant_id, group_key, principal_id, role
  )
  SELECT tenant_id, group_key, v_principal, role
  FROM enrichedlab_agent.broker_principal_invitations
  WHERE tenant_id = p_tenant
    AND email = v_email
    AND enabled
  ON CONFLICT (tenant_id, group_key, principal_id)
  DO UPDATE SET role = EXCLUDED.role, updated_at = clock_timestamp();

  UPDATE enrichedlab_agent.broker_principal_invitations
  SET consumed_at = clock_timestamp(), consumed_by = v_principal, updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant
    AND email = v_email
    AND enabled
    AND consumed_at IS NULL;

  RETURN v_principal;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_principal(
  p_tenant text,
  p_principal uuid
)
RETURNS TABLE(principal_id uuid, email text, display_name text)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT principal_id, email, display_name
  FROM enrichedlab_agent.broker_principals
  WHERE tenant_id = p_tenant AND principal_id = p_principal AND enabled
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_groups(
  p_tenant text,
  p_principal uuid
)
RETURNS TABLE(
  group_key text,
  name text,
  role text,
  max_active_agents integer,
  max_active_agents_per_principal integer,
  max_queued_tasks integer,
  retention_days integer
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT groups.group_key, groups.name, memberships.role,
         groups.max_active_agents, groups.max_active_agents_per_principal,
         groups.max_queued_tasks, groups.retention_days
  FROM enrichedlab_agent.broker_group_memberships AS memberships
  JOIN enrichedlab_agent.broker_research_groups AS groups
    ON groups.tenant_id = memberships.tenant_id
   AND groups.group_key = memberships.group_key
  WHERE memberships.tenant_id = p_tenant
    AND memberships.principal_id = p_principal
    AND groups.enabled
  ORDER BY groups.name, groups.group_key
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_profiles(
  p_tenant text,
  p_principal uuid
)
RETURNS TABLE(
  profile_key text,
  name text,
  description text,
  tool_bundle text,
  max_run_seconds integer
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT DISTINCT profiles.profile_key, profiles.name, profiles.description,
         profiles.tool_bundle, profiles.max_run_seconds
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
  ORDER BY profiles.name, profiles.profile_key
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_submit_task(
  p_tenant text,
  p_principal uuid,
  p_group text,
  p_profile text,
  p_objective text,
  p_idempotency_key text,
  p_request_sha256 text
)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_task uuid;
  v_existing_sha text;
  v_group enrichedlab_agent.broker_research_groups%ROWTYPE;
  v_profile enrichedlab_agent.broker_agent_profiles%ROWTYPE;
  v_queued integer;
  v_sequence bigint;
BEGIN
  IF length(btrim(p_objective)) < 1 OR octet_length(p_objective) > 8192 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:objective_invalid';
  END IF;
  SELECT task_id, request_sha256 INTO v_task, v_existing_sha
  FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant
    AND principal_id = p_principal
    AND idempotency_key = p_idempotency_key;
  IF v_task IS NOT NULL THEN
    IF v_existing_sha <> p_request_sha256 THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
    RETURN v_task;
  END IF;

  SELECT * INTO v_group
  FROM enrichedlab_agent.broker_research_groups
  WHERE tenant_id = p_tenant AND group_key = p_group AND enabled
  FOR UPDATE;
  IF NOT FOUND OR NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_group_memberships
    WHERE tenant_id = p_tenant AND group_key = p_group AND principal_id = p_principal
  ) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:group_forbidden';
  END IF;

  SELECT profiles.* INTO v_profile
  FROM enrichedlab_agent.broker_agent_profiles AS profiles
  JOIN enrichedlab_agent.broker_group_profile_grants AS grants
    ON grants.tenant_id = profiles.tenant_id
   AND grants.profile_key = profiles.profile_key
  WHERE profiles.tenant_id = p_tenant
    AND profiles.profile_key = p_profile
    AND grants.group_key = p_group
    AND profiles.enabled;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:profile_forbidden';
  END IF;

  SELECT count(*)::integer INTO v_queued
  FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND group_key = p_group AND status = 'queued';
  IF v_queued >= v_group.max_queued_tasks THEN
    RAISE EXCEPTION USING ERRCODE = 'P0001', MESSAGE = 'broker:group_queue_full';
  END IF;

  v_task := gen_random_uuid();
  INSERT INTO enrichedlab_agent.broker_tasks (
    tenant_id, task_id, principal_id, group_key, profile_key, objective,
    max_task_attempts, logical_agent, profile_snapshot, idempotency_key,
    request_sha256, event_sequence
  ) VALUES (
    p_tenant, v_task, p_principal, p_group, p_profile, p_objective,
    v_profile.max_task_attempts, 'task-' || replace(v_task::text, '-', ''),
    jsonb_build_object(
      'harness', v_profile.harness,
      'thinking', v_profile.thinking,
      'route', v_profile.route,
      'controller_profile', v_profile.controller_profile,
      'gateway_profile', v_profile.gateway_profile,
      'image_ref', v_profile.image_ref,
      'cpu_limit', v_profile.cpu_limit,
      'memory_limit', v_profile.memory_limit,
      'state_profile', v_profile.state_profile,
      'role', v_profile.role,
      'tool_bundle', v_profile.tool_bundle,
      'max_run_seconds', v_profile.max_run_seconds,
      'max_provider_attempts', v_profile.max_provider_attempts
    ),
    p_idempotency_key, p_request_sha256, 1
  );
  INSERT INTO enrichedlab_agent.broker_task_events (
    tenant_id, task_id, sequence, kind, state, code
  ) VALUES (p_tenant, v_task, 1, 'lifecycle', 'queued', 'task_submitted');
  RETURN v_task;
END;
$$;

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
  WHERE tasks.tenant_id = p_tenant AND tasks.principal_id = p_principal
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
    AND tasks.principal_id = p_principal
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
  WHERE tenant_id = p_tenant AND task_id = p_task AND principal_id = p_principal
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
    WHERE tenant_id = p_tenant AND task_id = p_task AND principal_id = p_principal
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
    WHERE tenant_id = p_tenant AND task_id = p_task AND principal_id = p_principal
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
    AND tasks.principal_id = p_principal
$$;

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

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_renew_task(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint,
  p_lease_seconds integer
)
RETURNS boolean
LANGUAGE sql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  UPDATE enrichedlab_agent.broker_tasks
  SET lease_expires_at = clock_timestamp() + make_interval(secs => p_lease_seconds),
      updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant
    AND task_id = p_task
    AND lease_owner = p_worker
    AND lease_generation = p_generation
    AND status IN ('leased', 'starting', 'running', 'cancelling')
  RETURNING true
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_mark_task_running(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint
)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_sequence bigint;
BEGIN
  UPDATE enrichedlab_agent.broker_tasks
  SET status = CASE WHEN desired_status = 'cancel' THEN 'cancelling' ELSE 'running' END,
      started_at = coalesce(started_at, clock_timestamp()),
      updated_at = clock_timestamp(),
      event_sequence = event_sequence + 1
  WHERE tenant_id = p_tenant
    AND task_id = p_task
    AND lease_owner = p_worker
    AND lease_generation = p_generation
    AND status IN ('leased', 'starting')
  RETURNING event_sequence INTO v_sequence;
  IF NOT FOUND THEN RETURN false; END IF;
  INSERT INTO enrichedlab_agent.broker_task_events (
    tenant_id, task_id, sequence, kind, state, code
  ) SELECT p_tenant, task_id, v_sequence, 'lifecycle', status, 'agent_started'
    FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id = p_tenant AND task_id = p_task;
  RETURN true;
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
    SELECT desired_status = 'cancel'
    FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id = p_tenant
      AND task_id = p_task
      AND lease_owner = p_worker
      AND lease_generation = p_generation
  ), true)
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_finish_task(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint,
  p_passed boolean,
  p_result_class text,
  p_error_code text
)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_status text;
  v_sequence bigint;
BEGIN
  UPDATE enrichedlab_agent.broker_tasks
  SET status = CASE
        WHEN desired_status = 'cancel' THEN 'cancelled'
        WHEN p_passed THEN 'completed'
        ELSE 'failed'
      END,
      result_class = nullif(p_result_class, ''),
      error_code = nullif(p_error_code, ''),
      lease_owner = NULL,
      lease_expires_at = NULL,
      finished_at = clock_timestamp(),
      updated_at = clock_timestamp(),
      event_sequence = event_sequence + 1
  WHERE tenant_id = p_tenant
    AND task_id = p_task
    AND lease_owner = p_worker
    AND lease_generation = p_generation
    AND status IN ('leased', 'starting', 'running', 'cancelling')
  RETURNING status, event_sequence INTO v_status, v_sequence;
  IF NOT FOUND THEN RETURN false; END IF;
  INSERT INTO enrichedlab_agent.broker_task_events (
    tenant_id, task_id, sequence, kind, state, code,
    metadata
  ) VALUES (
    p_tenant, p_task, v_sequence, 'lifecycle', v_status,
    CASE WHEN v_status = 'completed' THEN 'agent_completed'
         WHEN v_status = 'cancelled' THEN 'agent_cancelled'
         ELSE 'agent_failed' END,
    jsonb_build_object('result_class', nullif(p_result_class, ''), 'error_code', nullif(p_error_code, ''))
  );
  RETURN true;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_register_artifact(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint,
  p_visibility text,
  p_role text,
  p_label text,
  p_relative_path text,
  p_media_type text,
  p_size_bytes bigint,
  p_sha256 text
)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_artifact uuid;
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_tasks
    WHERE tenant_id = p_tenant
      AND task_id = p_task
      AND lease_owner = p_worker
      AND lease_generation = p_generation
  ) THEN
    RAISE EXCEPTION USING ERRCODE = '40001', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  INSERT INTO enrichedlab_agent.broker_artifacts (
    tenant_id, task_id, visibility, role, label, relative_path, media_type, size_bytes, sha256
  ) VALUES (
    p_tenant, p_task, p_visibility, p_role, p_label, p_relative_path, p_media_type, p_size_bytes, p_sha256
  )
  ON CONFLICT (tenant_id, task_id, relative_path)
  DO UPDATE SET visibility = EXCLUDED.visibility, role = EXCLUDED.role, label = EXCLUDED.label,
                media_type = EXCLUDED.media_type, size_bytes = EXCLUDED.size_bytes,
                sha256 = EXCLUDED.sha256
  RETURNING artifact_id INTO v_artifact;
  RETURN v_artifact;
END;
$$;

REVOKE ALL ON ALL TABLES IN SCHEMA enrichedlab_agent FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA enrichedlab_agent FROM PUBLIC;

INSERT INTO enrichedlab_agent.schema_migrations (version)
VALUES (2)
ON CONFLICT (version) DO NOTHING;
