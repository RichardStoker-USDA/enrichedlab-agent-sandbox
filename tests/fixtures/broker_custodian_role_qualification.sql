\set ON_ERROR_STOP on

SET ROLE broker_custodian_qual;

DO $$
DECLARE
  function_grants integer;
BEGIN
  SELECT count(*) INTO function_grants
  FROM pg_proc AS function
  JOIN pg_namespace AS namespace ON namespace.oid = function.pronamespace
  CROSS JOIN LATERAL aclexplode(coalesce(function.proacl, acldefault('f', function.proowner))) AS acl
  WHERE namespace.nspname = 'enrichedlab_agent'
    AND acl.grantee = (SELECT oid FROM pg_roles WHERE rolname = current_user)
    AND acl.privilege_type = 'EXECUTE';

  IF function_grants <> 6 THEN
    RAISE EXCEPTION 'custodian role has % direct function grants, expected six', function_grants;
  END IF;

  IF NOT has_function_privilege(current_user, 'enrichedlab_agent.broker_custody_task(text,uuid,text,integer,bigint)', 'EXECUTE')
    OR NOT has_function_privilege(current_user, 'enrichedlab_agent.broker_terminal_custody_task(text,uuid,text,integer,bigint)', 'EXECUTE')
    OR NOT has_function_privilege(current_user, 'enrichedlab_agent.broker_task_cancel_requested(text,uuid,text,bigint)', 'EXECUTE')
    OR NOT has_function_privilege(current_user, 'enrichedlab_agent.broker_renew_task(text,uuid,text,bigint,integer)', 'EXECUTE')
    OR NOT has_function_privilege(current_user, 'enrichedlab_agent.broker_custody_admitted_code(text,uuid,text,integer,bigint)', 'EXECUTE')
    OR NOT has_function_privilege(current_user, 'enrichedlab_agent.broker_record_attempt_input(text,uuid,text,integer,bigint,text,text,text,bigint,text,jsonb)', 'EXECUTE') THEN
    RAISE EXCEPTION 'custodian role is missing a required lease-fenced function';
  END IF;

  IF has_function_privilege(current_user, 'enrichedlab_agent.broker_schema_ready(integer)', 'EXECUTE')
    OR has_function_privilege(current_user, 'enrichedlab_agent.broker_claim_task(text,text,integer)', 'EXECUTE')
    OR has_function_privilege(current_user, 'enrichedlab_agent.broker_finish_task(text,uuid,text,bigint,boolean,text,text)', 'EXECUTE')
    OR has_function_privilege(current_user, 'enrichedlab_agent.broker_get_task(text,uuid,uuid)', 'EXECUTE')
    OR has_function_privilege(current_user, 'enrichedlab_agent.broker_task_generated_code(text,uuid,uuid)', 'EXECUTE')
    OR has_function_privilege(current_user, 'enrichedlab_agent.broker_task_admitted_code(text,uuid,text,integer,bigint)', 'EXECUTE')
    OR has_function_privilege(current_user, 'enrichedlab_agent.broker_record_attempt_execution(text,uuid,text,bigint,text,text,text,text,text)', 'EXECUTE')
    OR has_function_privilege(current_user, 'enrichedlab_agent.broker_register_artifact(text,uuid,text,bigint,text,text,text,text,text,bigint,text)', 'EXECUTE')
    OR has_function_privilege(current_user, 'enrichedlab_agent.broker_task_lineage_v2(text,uuid,uuid)', 'EXECUTE') THEN
    RAISE EXCEPTION 'custodian role received an unrelated broker function';
  END IF;

  BEGIN
    PERFORM * FROM enrichedlab_agent.broker_tasks;
    RAISE EXCEPTION 'custodian role read broker tables directly';
  EXCEPTION WHEN insufficient_privilege THEN
    NULL;
  END;
END;
$$;

RESET ROLE;
