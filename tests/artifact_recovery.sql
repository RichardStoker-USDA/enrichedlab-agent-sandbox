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


SELECT pg_temp.require(enrichedlab_agent.broker_finish_task(
  'research-site', :'task_id', 'runtime-worker', :lease_generation, false,
  'delivery_failed', 'finalization_failed'), 'finish failed');
RESET ROLE;
SELECT jsonb_build_object(
  'schemaVersion','sandbox.enrichedlab.org/terminal-export/v1',
  'taskId', :'task_id', 'principalId', :'principal_id',
  'groupKey','pilot-lab','profileKey','general-research','attempt',:attempt,
  'leaseGeneration',:lease_generation,'workerId','runtime-worker',
  'namespace','example-lab','sandboxName','example-sandbox','sandboxUid','sandbox-uid',
  'sandboxGeneration',1,'podName','example-pod','podUid','pod-uid',
  'runtimeClassName','kata-clh','artifactPvcName','example-artifacts','exitCode',0,
  'finishedAt','2026-01-01T00:00:00Z','files',jsonb_build_array(jsonb_build_object(
    'relativePath','result.txt','visibility','researcher','role','result',
    'label','Synthetic result','mediaType','text/plain','sizeBytes',3,'sha256',repeat('a',64)))
)::text AS terminal \gset
SELECT encode(sha256(convert_to(:'terminal','UTF8')),'hex') AS digest \gset
SELECT jsonb_build_object('schemaVersion','sandbox.enrichedlab.org/output-custodian-receipt/v1',
  'taskId',:'task_id','attempt',:attempt,'leaseGeneration',:lease_generation,
  'terminalSha256',:'digest','fileCount',1,'sizeBytes',3)::text AS custody \gset
SELECT jsonb_build_object('terminalSha256',:'digest','pvcUid','original-pvc',
  'exporterPodUid','recovery-exporter','files',:'terminal'::jsonb->'files')::text AS observed \gset
SELECT gen_random_uuid() AS recovery \gset
SELECT set_config('qual.terminal',:'terminal',true), set_config('qual.custody',:'custody',true),
  set_config('qual.observed',:'observed',true), set_config('qual.recovery',:'recovery',true);
SELECT enrichedlab_agent.broker_recover_artifacts('research-site',:'recovery','operator://fixture',
  :lease_generation,:'terminal',:'custody',:'observed')::text AS receipt \gset
SELECT pg_temp.require(enrichedlab_agent.broker_recover_artifacts('research-site',:'recovery',
  'operator://fixture',:lease_generation,:'terminal',:'custody',:'observed')=:'receipt'::jsonb,
  'exact replay changed receipt');
SELECT pg_temp.require((SELECT count(*)=1 FROM enrichedlab_agent.broker_task_events
  WHERE task_id=:'task_id' AND kind='artifact_recovery' AND state='failed'), 'recovery event duplicated');
SELECT pg_temp.require((SELECT status='failed' AND result_class='delivery_failed'
  AND error_code='finalization_failed' AND lease_owner IS NULL FROM enrichedlab_agent.broker_tasks
  WHERE task_id=:'task_id'), 'recovery rewrote failure');
SELECT pg_temp.require((SELECT count(*)=1 FROM enrichedlab_agent.broker_artifacts
  WHERE task_id=:'task_id' AND relative_path LIKE '%/recovered/%'), 'artifact missing');
DO $$
DECLARE
  t text := current_setting('qual.terminal');
  c jsonb := current_setting('qual.custody')::jsonb;
  o jsonb := current_setting('qual.observed')::jsonb;
  r uuid := current_setting('qual.recovery')::uuid;
BEGIN
  BEGIN
    PERFORM enrichedlab_agent.broker_recover_artifacts('research-site',r,'operator://changed',1,t,c,o);
    RAISE EXCEPTION 'changed replay accepted';
  EXCEPTION WHEN unique_violation THEN NULL; END;
  BEGIN
    PERFORM enrichedlab_agent.broker_recover_artifacts('research-site',gen_random_uuid(),
      'operator://fixture',1,t,c || jsonb_build_object('terminalSha256',repeat('b',64)),o);
    RAISE EXCEPTION 'wrong terminal digest accepted';
  EXCEPTION WHEN invalid_parameter_value THEN NULL; END;
  BEGIN
    PERFORM enrichedlab_agent.broker_recover_artifacts('research-site',gen_random_uuid(),
      'operator://fixture',1,t,c,o || '{"files":[]}');
    RAISE EXCEPTION 'changed observed files accepted';
  EXCEPTION WHEN invalid_parameter_value THEN NULL; END;
  BEGIN
    PERFORM enrichedlab_agent.broker_recover_artifacts('research-site',gen_random_uuid(),
      'operator://fixture',2,t,c,o);
    RAISE EXCEPTION 'stale generation accepted';
  EXCEPTION WHEN invalid_parameter_value THEN NULL; END;
  UPDATE enrichedlab_agent.broker_tasks SET status='running'
    WHERE task_id=current_setting('qual.runtime_task')::uuid;
  BEGIN
    PERFORM enrichedlab_agent.broker_recover_artifacts('research-site',gen_random_uuid(),
      'operator://fixture',1,t,c,o);
    RAISE EXCEPTION 'active task accepted';
  EXCEPTION WHEN invalid_parameter_value THEN NULL; END;
  UPDATE enrichedlab_agent.broker_tasks SET status='completed'
    WHERE task_id=current_setting('qual.runtime_task')::uuid;
  BEGIN
    PERFORM enrichedlab_agent.broker_recover_artifacts('research-site',gen_random_uuid(),
      'operator://fixture',1,t,c,o);
    RAISE EXCEPTION 'completed task accepted';
  EXCEPTION WHEN invalid_parameter_value THEN NULL; END;
END $$;
SELECT pg_temp.require(NOT has_function_privilege(role,
  'enrichedlab_agent.broker_recover_artifacts(text,uuid,text,bigint,text,jsonb,jsonb)','EXECUTE'),
  'runtime role has recovery privilege') FROM unnest(ARRAY[
    'broker_api_qual','broker_worker_qual','broker_custodian_qual']) role;
ROLLBACK;
