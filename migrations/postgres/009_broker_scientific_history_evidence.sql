BEGIN;

-- These links are written only by the current lease holder after it has retained
-- the exact immutable state-export bytes in broker artifact custody. They describe
-- observation order; they do not assert a tool caused a file change.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_scientific_evidence (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  event_id uuid NOT NULL,
  event_sequence bigint NOT NULL,
  event_hash text NOT NULL,
  snapshot_event_id uuid NOT NULL,
  artifact_id uuid NOT NULL,
  artifact_sha256 text NOT NULL,
  artifact_size_bytes bigint NOT NULL,
  artifact_role text NOT NULL,
  relation text NOT NULL DEFAULT 'observed_after',
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation, event_id, artifact_id),
  FOREIGN KEY (tenant_id, task_id)
    REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, artifact_id)
    REFERENCES enrichedlab_agent.broker_artifacts (tenant_id, artifact_id),
  CHECK (attempt >= 0),
  CHECK (lease_generation > 0),
  CHECK (event_sequence > 0),
  CHECK (event_hash ~ '^[0-9a-f]{64}$'),
  CHECK (artifact_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (artifact_size_bytes >= 0),
  CHECK (length(artifact_role) BETWEEN 1 AND 128),
  CHECK (relation = 'observed_after')
);

CREATE INDEX IF NOT EXISTS broker_task_scientific_evidence_event
  ON enrichedlab_agent.broker_task_scientific_evidence
    (tenant_id, task_id, attempt, lease_generation, event_id, artifact_id);

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_scientific_history_evidence(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_attempt integer,
  p_generation bigint,
  p_event_id uuid,
  p_event_sequence bigint,
  p_event_hash text,
  p_snapshot_event_id uuid,
  p_artifact uuid,
  p_artifact_sha256 text,
  p_artifact_size_bytes bigint,
  p_artifact_role text
)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_history enrichedlab_agent.broker_task_scientific_history%ROWTYPE;
BEGIN
  IF p_attempt < 0 OR p_generation < 1 OR p_event_sequence < 1
     OR p_event_hash !~ '^[0-9a-f]{64}$'
     OR p_artifact_sha256 !~ '^[0-9a-f]{64}$'
     OR p_artifact_size_bytes < 0
     OR length(p_artifact_role) NOT BETWEEN 1 AND 128 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:scientific_history_evidence_invalid';
  END IF;

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
  IF NOT FOUND THEN
    RETURN false;
  END IF;

  IF NOT EXISTS (
    SELECT 1
    FROM enrichedlab_agent.provenance_events AS events
    WHERE events.tenant_id = p_tenant
      AND events.run_id = v_history.state_run_id
      AND events.subject = v_history.subject
      AND events.event_id = p_event_id
      AND events.sequence = p_event_sequence
      AND events.hash = p_event_hash
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
  ) THEN
    RETURN false;
  END IF;

  IF NOT EXISTS (
    SELECT 1
    FROM enrichedlab_agent.broker_artifacts AS artifacts
    WHERE artifacts.tenant_id = p_tenant
      AND artifacts.task_id = p_task
      AND artifacts.artifact_id = p_artifact
      AND artifacts.visibility = 'researcher'
      AND artifacts.role = 'state'
      AND artifacts.sha256 = p_artifact_sha256
      AND artifacts.size_bytes = p_artifact_size_bytes
  ) THEN
    RETURN false;
  END IF;

  INSERT INTO enrichedlab_agent.broker_task_scientific_evidence (
    tenant_id, task_id, attempt, lease_generation, event_id, event_sequence,
    event_hash, snapshot_event_id, artifact_id, artifact_sha256,
    artifact_size_bytes, artifact_role
  ) VALUES (
    p_tenant, p_task, p_attempt, p_generation, p_event_id, p_event_sequence,
    p_event_hash, p_snapshot_event_id, p_artifact, p_artifact_sha256,
    p_artifact_size_bytes, p_artifact_role
  ) ON CONFLICT (tenant_id, task_id, attempt, lease_generation, event_id, artifact_id)
  DO NOTHING;

  RETURN EXISTS (
    SELECT 1
    FROM enrichedlab_agent.broker_task_scientific_evidence AS evidence
    WHERE evidence.tenant_id = p_tenant
      AND evidence.task_id = p_task
      AND evidence.attempt = p_attempt
      AND evidence.lease_generation = p_generation
      AND evidence.event_id = p_event_id
      AND evidence.event_sequence = p_event_sequence
      AND evidence.event_hash = p_event_hash
      AND evidence.snapshot_event_id = p_snapshot_event_id
      AND evidence.artifact_id = p_artifact
      AND evidence.artifact_sha256 = p_artifact_sha256
      AND evidence.artifact_size_bytes = p_artifact_size_bytes
      AND evidence.artifact_role = p_artifact_role
      AND evidence.relation = 'observed_after'
  );
END;
$$;

-- Migration 007's reader remains callable for already-granted API roles. This
-- evidence-aware reader has a distinct identity because PostgreSQL cannot
-- replace a function whose OUT row type gained evidence_count.
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
    WHERE tenant_id = p_tenant AND task_id = p_task AND principal_id = p_principal
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
    WHERE tenant_id = p_tenant AND task_id = p_task AND principal_id = p_principal
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
RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT p_version IN (2, 3, 7, 8, 9) AND EXISTS (
    SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = p_version
  );
$$;

REVOKE ALL ON TABLE enrichedlab_agent.broker_task_scientific_evidence FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_scientific_history_evidence(text,uuid,text,integer,bigint,uuid,bigint,text,uuid,uuid,text,bigint,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_scientific_history_events_with_evidence(text,uuid,uuid,integer,bigint,bigint,integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_scientific_history_evidence(text,uuid,uuid,integer,bigint,uuid,uuid,integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_schema_ready(integer) FROM PUBLIC;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (9) ON CONFLICT DO NOTHING;
COMMIT;
