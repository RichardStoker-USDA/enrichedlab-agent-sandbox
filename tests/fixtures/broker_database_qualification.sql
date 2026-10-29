\set ON_ERROR_STOP on

INSERT INTO enrichedlab_agent.broker_principal_invitations (
  tenant_id, email, group_key, role, enabled
) VALUES (
  'research-site', 'reviewer@example.invalid', 'pilot-lab', 'reviewer', true
);

SET ROLE broker_api_qual;
SELECT enrichedlab_agent.broker_resolve_principal(
  'research-site',
  'https://example.cloudflareaccess.com',
  'example-subject-a',
  'researcher@example.invalid',
  'Example Researcher',
  'person'
) AS principal_id \gset
SELECT enrichedlab_agent.broker_resolve_principal(
  'research-site',
  'https://example.cloudflareaccess.com',
  'example-subject-b',
  'reviewer@example.invalid',
  'Example Reviewer',
  'person'
) AS reviewer_id \gset

SELECT enrichedlab_agent.broker_submit_person_task(
  'research-site', :'principal_id', 'pilot-lab', 'general-research',
  'Check the supplied fixture with deterministic tools.', 'request-a', repeat('a', 64)
) AS task_a \gset
SELECT enrichedlab_agent.broker_submit_person_task(
  'research-site', :'principal_id', 'pilot-lab', 'general-research',
  'Check the supplied fixture with deterministic tools.', 'request-a', repeat('a', 64)
) AS task_a_again \gset
SELECT enrichedlab_agent.broker_submit_person_task(
  'research-site', :'principal_id', 'pilot-lab', 'general-research',
  'Run an independent check of the fixture.', 'request-b', repeat('b', 64)
) AS task_b \gset
SELECT enrichedlab_agent.broker_submit_person_task(
  'research-site', :'principal_id', 'pilot-lab', 'general-research',
  'Run a third independent check of the fixture.', 'request-c', repeat('c', 64)
) AS task_c \gset

SELECT set_config('qual.principal_id', :'principal_id', false);
SELECT set_config('qual.task_a', :'task_a', false);
SELECT set_config('qual.task_a_again', :'task_a_again', false);
SELECT set_config('qual.reviewer_id', :'reviewer_id', false);

DO $$
BEGIN
  IF current_setting('qual.task_a') <> current_setting('qual.task_a_again') THEN
    RAISE EXCEPTION 'idempotent submission returned a different task';
  END IF;
  BEGIN
    PERFORM enrichedlab_agent.broker_submit_person_task(
      'research-site', current_setting('qual.principal_id')::uuid,
      'pilot-lab', 'general-research',
      'A changed request must conflict.', 'request-a', repeat('d', 64)
    );
    RAISE EXCEPTION 'idempotency mismatch was accepted';
  EXCEPTION WHEN unique_violation THEN
    NULL;
  END;
  BEGIN
    PERFORM * FROM enrichedlab_agent.broker_tasks;
    RAISE EXCEPTION 'API role read broker tables directly';
  EXCEPTION WHEN insufficient_privilege THEN
    NULL;
  END;
  BEGIN
    PERFORM * FROM enrichedlab_agent.broker_claim_task('research-site', 'bad-worker', 120);
    RAISE EXCEPTION 'API role called the worker claim function';
  EXCEPTION WHEN insufficient_privilege THEN
    NULL;
  END;
  IF EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_get_task(
      'research-site', current_setting('qual.reviewer_id')::uuid,
      current_setting('qual.task_a')::uuid
    )
  ) THEN
    RAISE EXCEPTION 'a second principal read the first principal task';
  END IF;
  BEGIN
    PERFORM enrichedlab_agent.broker_request_cancel(
      'research-site', current_setting('qual.reviewer_id')::uuid,
      current_setting('qual.task_a')::uuid
    );
    RAISE EXCEPTION 'a second principal cancelled the first principal task';
  EXCEPTION WHEN no_data_found THEN
    NULL;
  END;
END;
$$;

RESET ROLE;

DO $$
BEGIN
  BEGIN
    UPDATE enrichedlab_agent.broker_task_events
    SET code = 'changed'
    WHERE tenant_id = 'research-site'
      AND task_id = current_setting('qual.task_a')::uuid;
    RAISE EXCEPTION 'append-only event mutation was accepted';
  EXCEPTION WHEN raise_exception THEN
    IF SQLERRM <> 'broker task events are append-only' THEN
      RAISE;
    END IF;
  END;
END;
$$;
SET ROLE broker_worker_qual;
SELECT task_id AS claimed_a, lease_generation AS generation_a
FROM enrichedlab_agent.broker_claim_task('research-site', 'worker-a', 120) \gset
SELECT task_id AS claimed_b, lease_generation AS generation_b
FROM enrichedlab_agent.broker_claim_task('research-site', 'worker-a', 120) \gset
SELECT count(*) AS third_claims
FROM enrichedlab_agent.broker_claim_task('research-site', 'worker-a', 120) \gset

SELECT set_config('qual.claimed_a', :'claimed_a', false);
SELECT set_config('qual.generation_a', :'generation_a', false);
SELECT set_config('qual.claimed_b', :'claimed_b', false);
SELECT set_config('qual.generation_b', :'generation_b', false);
SELECT set_config('qual.third_claims', :'third_claims', false);

DO $$
BEGIN
  IF current_setting('qual.third_claims') <> '0' THEN
    RAISE EXCEPTION 'per-principal active quota was not enforced';
  END IF;
  IF NOT enrichedlab_agent.broker_mark_task_running(
    'research-site', current_setting('qual.claimed_a')::uuid, 'worker-a',
    current_setting('qual.generation_a')::bigint
  ) THEN
    RAISE EXCEPTION 'valid lease could not enter running state';
  END IF;
  IF enrichedlab_agent.broker_mark_task_running(
    'research-site', current_setting('qual.claimed_a')::uuid, 'worker-a',
    current_setting('qual.generation_a')::bigint + 1
  ) THEN
    RAISE EXCEPTION 'stale lease generation changed task state';
  END IF;
  PERFORM enrichedlab_agent.broker_register_artifact(
    'research-site', current_setting('qual.claimed_a')::uuid, 'worker-a',
    current_setting('qual.generation_a')::bigint,
    'researcher', 'result', 'Qualified result',
    'tasks/' || current_setting('qual.claimed_a') || '/result.json',
    'application/json', 2, repeat('e', 64)
  );
  IF NOT enrichedlab_agent.broker_finish_task(
    'research-site', current_setting('qual.claimed_a')::uuid, 'worker-a',
    current_setting('qual.generation_a')::bigint,
    true, 'passed', ''
  ) THEN
    RAISE EXCEPTION 'valid lease could not finish task';
  END IF;
  IF enrichedlab_agent.broker_finish_task(
    'research-site', current_setting('qual.claimed_a')::uuid, 'worker-a',
    current_setting('qual.generation_a')::bigint,
    true, 'passed', ''
  ) THEN
    RAISE EXCEPTION 'stale worker committed a second result';
  END IF;
  BEGIN
    PERFORM * FROM enrichedlab_agent.broker_get_principal(
      'research-site', current_setting('qual.principal_id')::uuid
    );
    RAISE EXCEPTION 'worker role called an API identity function';
  EXCEPTION WHEN insufficient_privilege THEN
    NULL;
  END;
END;
$$;

RESET ROLE;
SET ROLE broker_api_qual;
SELECT enrichedlab_agent.broker_request_cancel(
  'research-site', :'principal_id', :'claimed_b'
) AS cancel_status \gset
SELECT set_config('qual.cancel_status', :'cancel_status', false);

DO $$
DECLARE
  v_events integer;
  v_artifacts integer;
BEGIN
  IF current_setting('qual.cancel_status') <> 'cancelling' THEN
    RAISE EXCEPTION 'running task cancellation did not enter cancelling state';
  END IF;
  SELECT count(*) INTO v_events
  FROM enrichedlab_agent.broker_list_task_events(
    'research-site', current_setting('qual.principal_id')::uuid,
    current_setting('qual.claimed_a')::uuid, 0, 100
  );
  IF v_events < 3 THEN
    RAISE EXCEPTION 'task lifecycle events are incomplete';
  END IF;
  SELECT count(*) INTO v_artifacts
  FROM enrichedlab_agent.broker_list_artifacts(
    'research-site', current_setting('qual.principal_id')::uuid,
    current_setting('qual.claimed_a')::uuid
  );
  IF v_artifacts <> 1 THEN
    RAISE EXCEPTION 'researcher-visible artifact custody is incomplete';
  END IF;
END;
$$;

RESET ROLE;
