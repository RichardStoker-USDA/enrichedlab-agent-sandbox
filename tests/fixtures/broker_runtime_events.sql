\set ON_ERROR_STOP on
BEGIN;

CREATE FUNCTION pg_temp.require(ok boolean, message text) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  IF ok IS DISTINCT FROM true THEN RAISE EXCEPTION '%', message; END IF;
END;
$$;

INSERT INTO enrichedlab_agent.broker_principal_invitations
  (tenant_id, email, group_key, role, enabled)
VALUES ('research-site', 'observer@example.invalid', 'pilot-lab', 'reviewer', true);
SET ROLE broker_api_qual;
SELECT enrichedlab_agent.broker_resolve_principal(
  'research-site', 'https://example.cloudflareaccess.com', 'runtime-subject',
  'researcher@example.invalid', 'Example Researcher',
  'person'
) AS principal_id \gset
SELECT enrichedlab_agent.broker_resolve_principal(
  'research-site', 'https://example.cloudflareaccess.com', 'other-runtime-subject',
  'observer@example.invalid', 'Example Observer',
  'person'
) AS observer_id \gset
SELECT enrichedlab_agent.broker_submit_person_task(
  'research-site', :'principal_id', 'pilot-lab', 'general-research',
  'Run a deterministic synthetic fixture.', 'runtime-request', repeat('f',64)
) AS task_id \gset
SELECT set_config('qual.runtime_task', :'task_id', true);
DO $$ BEGIN
  BEGIN
    PERFORM enrichedlab_agent.broker_record_runtime_event(
      'research-site', current_setting('qual.runtime_task')::uuid,
      'runtime-worker', 1, 'sandbox_created', '{}'::jsonb);
    RAISE EXCEPTION 'API role appended a runtime event';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
END $$;

SET ROLE broker_worker_qual;
SELECT lease_generation, attempt FROM enrichedlab_agent.broker_claim_task(
  'research-site', 'runtime-worker', 120
) \gset
SELECT pg_temp.require(enrichedlab_agent.broker_mark_task_running(
  'research-site', :'task_id', 'runtime-worker', :lease_generation), 'running lease missing');
SELECT jsonb_build_object(
  'schema_version', 1, 'attempt', :attempt, 'lease_generation', :lease_generation,
  'namespace', 'example-lab', 'sandbox_name', 'example-sandbox',
  'sandbox_uid', 'sandbox-uid', 'sandbox_generation', 1
)::text AS created \gset
SELECT (:'created'::jsonb || jsonb_build_object(
  'pod_name', 'example-pod', 'pod_uid', 'pod-uid', 'runtime_class_name', 'kata-clh'
))::text AS observed \gset
SELECT set_config('qual.runtime_created', :'created', true);
SELECT set_config('qual.runtime_observed', :'observed', true);
SELECT set_config('qual.runtime_generation', :'lease_generation', true);

SELECT pg_temp.require(enrichedlab_agent.broker_record_runtime_event(
  'research-site', :'task_id', 'runtime-worker', :lease_generation,
  'sandbox_created', :'created'), 'creation was not recorded');
SELECT pg_temp.require(enrichedlab_agent.broker_record_runtime_event(
  'research-site', :'task_id', 'runtime-worker', :lease_generation,
  'sandbox_created', :'created'), 'exact replay failed');
SELECT pg_temp.require(NOT enrichedlab_agent.broker_record_runtime_event(
  'research-site', :'task_id', 'wrong-worker', :lease_generation,
  'sandbox_created', :'created'), 'wrong worker appended an event');
SELECT pg_temp.require(NOT enrichedlab_agent.broker_record_runtime_event(
  'research-site', :'task_id', 'runtime-worker', :lease_generation + 1,
  'sandbox_created', :'created'), 'wrong generation appended an event');
SELECT pg_temp.require(NOT enrichedlab_agent.broker_record_runtime_event(
  'other-site', :'task_id', 'runtime-worker', :lease_generation,
  'sandbox_created', :'created'), 'wrong tenant appended an event');

DO $$
DECLARE
  task uuid := current_setting('qual.runtime_task')::uuid;
  generation bigint := current_setting('qual.runtime_generation')::bigint;
  created jsonb := current_setting('qual.runtime_created')::jsonb;
  observed jsonb := current_setting('qual.runtime_observed')::jsonb;
BEGIN
  BEGIN
    PERFORM * FROM enrichedlab_agent.broker_task_events;
    RAISE EXCEPTION 'worker read event tables directly';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
  BEGIN
    PERFORM enrichedlab_agent.broker_record_runtime_event('research-site', task,
      'runtime-worker', generation, 'sandbox_created', created || '{"prompt":"rejected"}');
    RAISE EXCEPTION 'unknown metadata field was stored';
  EXCEPTION WHEN invalid_parameter_value THEN NULL;
  END;
  BEGIN
    PERFORM enrichedlab_agent.broker_record_runtime_event('research-site', task,
      'runtime-worker', generation, 'sandbox_created', created || '{"sandbox_uid":"replacement"}');
    RAISE EXCEPTION 'changed replay was accepted';
  EXCEPTION WHEN unique_violation THEN NULL;
  END;
  BEGIN
    PERFORM enrichedlab_agent.broker_record_runtime_event('research-site', task,
      'runtime-worker', generation, 'sandbox_terminal_observed', observed || '{"exit_code":0}');
    RAISE EXCEPTION 'terminal event without a recorded Pod was accepted';
  EXCEPTION WHEN unique_violation THEN NULL;
  END;
END $$;

SELECT pg_temp.require(enrichedlab_agent.broker_record_runtime_event(
  'research-site', :'task_id', 'runtime-worker', :lease_generation,
  'sandbox_pod_observed', :'observed'), 'Pod was not recorded');
DO $$ BEGIN
  BEGIN
    PERFORM enrichedlab_agent.broker_record_runtime_event(
      'research-site', current_setting('qual.runtime_task')::uuid,
      'runtime-worker', current_setting('qual.runtime_generation')::bigint,
      'sandbox_terminal_observed', current_setting('qual.runtime_observed')::jsonb
        || '{"exit_code":0,"pod_uid":"replacement"}');
    RAISE EXCEPTION 'terminal event accepted a replacement Pod';
  EXCEPTION WHEN unique_violation THEN NULL;
  END;
END $$;
SELECT pg_temp.require(enrichedlab_agent.broker_record_runtime_event(
  'research-site', :'task_id', 'runtime-worker', :lease_generation,
  'sandbox_terminal_observed', :'observed'::jsonb || '{"exit_code":0}'), 'terminal not recorded');
SELECT pg_temp.require(enrichedlab_agent.broker_record_runtime_event(
  'research-site', :'task_id', 'runtime-worker', :lease_generation,
  'sandbox_recovery_needed', :'created'::jsonb || '{"reason":"finalization_failed"}'),
  'recovery was not recorded');

SET ROLE broker_api_qual;
SELECT pg_temp.require((SELECT count(*) = 4
  FROM enrichedlab_agent.broker_list_task_events('research-site', :'principal_id', :'task_id', 0, 100)
  WHERE kind = 'runtime'), 'owner did not see four runtime events');
SELECT set_config('qual.runtime_observer', :'observer_id', true);
DO $$ BEGIN
  BEGIN
    PERFORM * FROM enrichedlab_agent.broker_list_task_events(
      'research-site', current_setting('qual.runtime_observer')::uuid,
      current_setting('qual.runtime_task')::uuid, 0, 100);
    RAISE EXCEPTION 'another principal read the events';
  EXCEPTION WHEN no_data_found THEN
    IF SQLERRM <> 'broker:task_not_found' THEN RAISE; END IF;
  END;
END $$;

RESET ROLE;
SELECT pg_temp.require((SELECT count(*) = 4
  FROM enrichedlab_agent.broker_task_events
  WHERE task_id = :'task_id' AND kind = 'runtime'), 'replay duplicated an event');
SELECT pg_temp.require((SELECT count(*) = max(sequence) AND min(sequence) = 1
  FROM enrichedlab_agent.broker_task_events
  WHERE task_id = :'task_id'), 'task event sequence is not contiguous');
UPDATE enrichedlab_agent.broker_tasks
SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE task_id = :'task_id';
SET ROLE broker_worker_qual;
SELECT pg_temp.require(NOT enrichedlab_agent.broker_record_runtime_event(
  'research-site', :'task_id', 'runtime-worker', :lease_generation,
  'sandbox_created', :'created'), 'expired worker replay was accepted');
RESET ROLE;
ROLLBACK;
