-- Orientation and supervision. A run's first call, for a lab the coordinator orients, is an orientation:
-- the question restated, the sources, tools and papers it expects to use, what is unknown, and at most three profiling
-- steps. Its checked record is kept here once per run, readable by the coordinator under the run's lease and by the
-- project's lab members, never by anyone else, since it carries the model's text about the lab's papers (the literature
-- review's rule, migration 070). The supervisor reads the run's deferrals and the tasks each turn submitted, and may
-- pause the run with a named reason.
-- Rollback: an earlier coordinator reads neither the table nor the functions; they may stay.
BEGIN;
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_research_orientations (
  tenant_id text NOT NULL,
  run_id uuid NOT NULL,
  record jsonb NOT NULL CHECK (jsonb_typeof(record) = 'object' AND octet_length(record::text) <= 65536),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, run_id),
  FOREIGN KEY (tenant_id, run_id) REFERENCES enrichedlab_agent.broker_research_runs (tenant_id, run_id)
);
ALTER TABLE enrichedlab_agent.broker_research_orientations ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_research_orientations FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_research_orientations;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_research_orientations
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- A null record reads the run's orientation; a checked or refused record keeps it, once. Both need the current lease and
-- a run whose actor is a member of the project's lab.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_orientation_for_run(
  p_tenant text, p_run uuid, p_worker text, p_generation bigint, p_record jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE held jsonb; answer jsonb;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_research_runs
    WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
      AND generation = p_generation AND lease_until > clock_timestamp() FOR UPDATE;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost'; END IF;
  held := enrichedlab_agent.broker_research_run_library(p_tenant, p_run, p_worker, p_generation, '{}'::uuid[]);
  IF held->>'member' IS DISTINCT FROM 'true' THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:context_lab_members_only';
  END IF;
  IF p_record IS NOT NULL THEN
    IF (p_record->>'schema_version') IS DISTINCT FROM '1'
      OR jsonb_typeof(p_record->'specialist') IS DISTINCT FROM 'array'
      OR coalesce(p_record->>'status', '') NOT IN ('read', 'refused') THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:orientation_invalid';
    END IF;
    IF p_record->>'status' = 'refused' THEN
      IF p_record ? 'orientation' OR jsonb_typeof(p_record->'faults') IS DISTINCT FROM 'array'
        OR jsonb_array_length(p_record->'faults') = 0
        OR EXISTS (SELECT 1 FROM jsonb_array_elements(p_record->'faults') f WHERE jsonb_typeof(f) <> 'string') THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:orientation_invalid';
      END IF;
    ELSIF jsonb_typeof(p_record->'orientation') IS DISTINCT FROM 'object'
      OR jsonb_typeof(p_record->'step_keys') IS DISTINCT FROM 'array'
      OR jsonb_array_length(p_record->'step_keys') > 3
      OR EXISTS (SELECT 1 FROM jsonb_array_elements(p_record->'step_keys') k
        WHERE jsonb_typeof(k) <> 'string' OR (k #>> '{}') !~ '^[a-zA-Z0-9_-]{1,100}$') THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:orientation_invalid';
    END IF;
    INSERT INTO enrichedlab_agent.broker_research_orientations (tenant_id, run_id, record)
      VALUES (p_tenant, p_run, p_record) ON CONFLICT DO NOTHING;
  END IF;
  SELECT record INTO answer FROM enrichedlab_agent.broker_research_orientations
    WHERE tenant_id = p_tenant AND run_id = p_run;
  RETURN answer;
END;
$$;

-- A reader of the project who is a member of its lab reads the run's checked orientation.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_orientation(p_tenant text, p_actor uuid, p_run uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE project uuid; grp text; answer jsonb;
BEGIN
  SELECT project_id INTO project FROM enrichedlab_agent.broker_research_runs WHERE tenant_id = p_tenant AND run_id = p_run;
  IF project IS NULL OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  SELECT group_key INTO grp FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = project;
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = m.tenant_id AND a.principal_id = m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND g.enabled
    WHERE m.tenant_id = p_tenant AND m.group_key = grp AND m.principal_id = p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:context_lab_members_only';
  END IF;
  SELECT record INTO answer FROM enrichedlab_agent.broker_research_orientations
    WHERE tenant_id = p_tenant AND run_id = p_run AND record->>'status' = 'read';
  IF answer IS NULL THEN RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:orientation_not_found'; END IF;
  RETURN answer;
END;
$$;

-- What the supervisor counts, under the run's lease: the run's deferrals other than waits, in order (the newest 128),
-- and each committed turn's outcome with the tasks it submitted. Codes, reasons and identifiers only.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_faults(
  p_tenant text, p_run uuid, p_worker text, p_generation bigint)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_research_runs
    WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
      AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost'; END IF;
  RETURN jsonb_build_object(
    'events', coalesce((SELECT jsonb_agg(jsonb_build_object('sequence', e.sequence, 'code', e.code, 'reason', e.reason,
        'turn', e.turn) ORDER BY e.sequence)
      FROM (SELECT * FROM enrichedlab_agent.broker_research_run_events
            WHERE tenant_id = p_tenant AND run_id = p_run AND code <> 'waiting_for_tools'
            ORDER BY sequence DESC LIMIT 128) e), '[]'::jsonb),
    'turns', coalesce((SELECT jsonb_agg(jsonb_build_object('turn', t.turn, 'outcome', t.record->>'outcome',
        'tasks', coalesce((SELECT jsonb_agg(s->>'task_id' ORDER BY n)
                           FROM jsonb_array_elements(CASE WHEN jsonb_typeof(t.record->'submitted_tasks') = 'array'
                                                          THEN t.record->'submitted_tasks' ELSE '[]'::jsonb END)
                                WITH ORDINALITY AS x(s, n)), '[]'::jsonb),
        -- The results that failed their step's check as this turn's call read them (`failed_results`, task and
        -- reference only), so a failure stays counted after its step leaves the working plan.
        'failed', coalesce((SELECT jsonb_agg(jsonb_build_object('task_id', f->>'task_id', 'ref', f->>'ref') ORDER BY n)
                            FROM jsonb_array_elements(CASE WHEN jsonb_typeof(t.record->'failed_results') = 'array'
                                                           THEN t.record->'failed_results' ELSE '[]'::jsonb END)
                                 WITH ORDINALITY AS x(f, n)
                            WHERE jsonb_typeof(f->'task_id') = 'string' AND jsonb_typeof(f->'ref') = 'string'), '[]'::jsonb)) ORDER BY t.turn)
      FROM enrichedlab_agent.broker_research_turns t WHERE t.tenant_id = p_tenant AND t.run_id = p_run), '[]'::jsonb));
END;
$$;

-- The supervisor's pause: the run stops for a person with a named reason and the event records it.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_supervisor_pause_research_run(
  p_tenant text, p_run uuid, p_worker text, p_generation bigint, p_reason text)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; next_sequence bigint;
BEGIN
  IF p_reason IS NULL OR p_reason NOT IN ('repeated_refusal', 'refusals_total', 'failed_checks', 'no_progress') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:research_run_invalid';
  END IF;
  UPDATE enrichedlab_agent.broker_research_runs SET status = 'paused', code = 'supervisor_paused',
    lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
      AND generation = p_generation AND lease_until > clock_timestamp()
    RETURNING * INTO r;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost'; END IF;
  SELECT coalesce(max(e.sequence), 0) + 1 INTO next_sequence FROM enrichedlab_agent.broker_research_run_events e
    WHERE e.tenant_id = p_tenant AND e.run_id = p_run;
  INSERT INTO enrichedlab_agent.broker_research_run_events (tenant_id, run_id, sequence, code, reason, generation, turn,
    failures, status_after)
    VALUES (p_tenant, p_run, next_sequence, 'supervisor_paused', p_reason, p_generation, r.turn, r.failures, r.status);
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_orientation_for_run(text, uuid, text, bigint, jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_orientation(text, uuid, uuid) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_faults(text, uuid, text, bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_supervisor_pause_research_run(text, uuid, text, bigint, text) FROM PUBLIC;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (74) ON CONFLICT DO NOTHING;
COMMIT;
