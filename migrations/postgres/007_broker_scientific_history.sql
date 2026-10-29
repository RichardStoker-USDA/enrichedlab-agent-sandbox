BEGIN;

-- A binding is created only by the legacy logical-agent worker after it reads the
-- controller-issued agent status. It never creates or copies provenance events.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_scientific_history (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  principal_id uuid NOT NULL,
  logical_agent text NOT NULL,
  state_run_id text NOT NULL,
  subject text NOT NULL,
  first_segment bigint NOT NULL,
  last_segment bigint,
  bound_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation),
  FOREIGN KEY (tenant_id, task_id)
    REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, principal_id)
    REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (attempt >= 0),
  CHECK (lease_generation > 0),
  CHECK (logical_agent ~ '^task-[0-9a-f]{32}$'),
  CHECK (state_run_id ~ '^agent-[0-9a-f]{32}$'),
  CHECK (subject ~ '^agent-[0-9a-f]{32}$'),
  CHECK (first_segment >= 1 AND (last_segment IS NULL OR last_segment >= first_segment))
);

CREATE INDEX IF NOT EXISTS broker_task_scientific_history_current
  ON enrichedlab_agent.broker_task_scientific_history
    (tenant_id, task_id, attempt, lease_generation);

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_bind_scientific_history(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_attempt integer,
  p_generation bigint,
  p_principal uuid,
  p_logical_agent text,
  p_state_run_id text,
  p_subject text,
  p_first_segment bigint
)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_task enrichedlab_agent.broker_tasks%ROWTYPE;
BEGIN
  IF p_attempt < 0 OR p_generation < 1
     OR p_logical_agent !~ '^task-[0-9a-f]{32}$'
     OR p_state_run_id !~ '^agent-[0-9a-f]{32}$'
     OR p_subject !~ '^agent-[0-9a-f]{32}$'
     OR p_first_segment < 1 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:scientific_history_binding_invalid';
  END IF;

  SELECT * INTO v_task
  FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant
    AND task_id = p_task
    AND principal_id = p_principal
    AND logical_agent = p_logical_agent
    AND lease_owner = p_worker
    AND attempt = p_attempt
    AND lease_generation = p_generation
    AND lease_expires_at > CURRENT_TIMESTAMP
    AND status = 'running'
    AND desired_status = 'run'
  FOR UPDATE;
  IF NOT FOUND THEN
    RETURN false;
  END IF;
  INSERT INTO enrichedlab_agent.broker_task_scientific_history (
    tenant_id, task_id, attempt, lease_generation, principal_id,
    logical_agent, state_run_id, subject, first_segment
  ) VALUES (
    p_tenant, p_task, p_attempt, p_generation, p_principal,
    p_logical_agent, p_state_run_id, p_subject, p_first_segment
  ) ON CONFLICT (tenant_id, task_id, attempt, lease_generation) DO NOTHING;

  RETURN EXISTS (
    SELECT 1
    FROM enrichedlab_agent.broker_task_scientific_history AS history
    WHERE history.tenant_id = p_tenant
      AND history.task_id = p_task
      AND history.attempt = p_attempt
      AND history.lease_generation = p_generation
      AND history.principal_id = p_principal
      AND history.logical_agent = p_logical_agent
      AND history.state_run_id = p_state_run_id
      AND history.subject = p_subject
      AND history.first_segment = p_first_segment
  );
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_finalize_scientific_history(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_attempt integer,
  p_generation bigint,
  p_last_segment bigint
)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_history enrichedlab_agent.broker_task_scientific_history%ROWTYPE;
BEGIN
  SELECT history.* INTO v_history
  FROM enrichedlab_agent.broker_task_scientific_history AS history
  JOIN enrichedlab_agent.broker_tasks AS tasks
    ON tasks.tenant_id = history.tenant_id AND tasks.task_id = history.task_id
  WHERE history.tenant_id = p_tenant
    AND history.task_id = p_task
    AND history.attempt = p_attempt
    AND history.lease_generation = p_generation
    AND tasks.lease_owner = p_worker
    AND tasks.attempt = p_attempt
    AND tasks.lease_generation = p_generation
    AND tasks.lease_expires_at > CURRENT_TIMESTAMP
    AND tasks.status IN ('running', 'cancelling')
  FOR UPDATE OF history;
  IF NOT FOUND OR p_last_segment < v_history.first_segment THEN
    RETURN false;
  END IF;
  UPDATE enrichedlab_agent.broker_task_scientific_history
  SET last_segment = p_last_segment
  WHERE tenant_id = p_tenant
    AND task_id = p_task
    AND attempt = p_attempt
    AND lease_generation = p_generation
    AND (last_segment IS NULL OR last_segment = p_last_segment);
  RETURN FOUND;
END;
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
  WHERE tenant_id = p_tenant AND task_id = p_task AND principal_id = p_principal;
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
    WHERE tenant_id = p_tenant AND task_id = p_task AND principal_id = p_principal
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

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT p_version IN (2, 3, 7) AND EXISTS (
    SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = p_version
  );
$$;

REVOKE ALL ON TABLE enrichedlab_agent.broker_task_scientific_history FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_bind_scientific_history(text,uuid,text,integer,bigint,uuid,text,text,text,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_finalize_scientific_history(text,uuid,text,integer,bigint,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_scientific_history_status(text,uuid,uuid,integer,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_scientific_history_events(text,uuid,uuid,integer,bigint,bigint,integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_schema_ready(integer) FROM PUBLIC;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (7) ON CONFLICT DO NOTHING;
COMMIT;
