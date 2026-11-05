-- The research lead's working memory. For a lab in the coordinator's --working-memory-labs, a checked call after each
-- committed lead decision writes what the lead is in the middle of (open threads, what the evidence ruled out and by
-- which ref, what the next steps depend on, what is unknown), and the coordinator keeps it on that turn's record as
-- `working_memory` with `working_memory_sha256`. A refused or failed call leaves only its fixed codes in
-- `working_memory_call`, so the newest record that holds a memory is the last accepted one. This function reads it
-- back for the run's holder under the run's lease: the turn, the memory and its digest, or null when no turn of the
-- run kept one. It reads only the run's own turn records and writes nothing. Schema version 88 gates coordinators
-- that read it.
--
-- Rollback: move coordinators to an image without 088, drop the function, run 087's broker_schema_ready again and
-- delete schema_migrations version 88. Turn records keep their working_memory fields; nothing else reads them.
BEGIN;
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_working_memory(
  p_tenant text, p_run uuid, p_worker text, p_generation bigint)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE answer jsonb;
BEGIN
  -- Every comparison is NULL-safe: a missing worker or generation must refuse, not pass.
  PERFORM 1 FROM enrichedlab_agent.broker_research_runs
    WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner IS NOT DISTINCT FROM p_worker
      AND generation IS NOT DISTINCT FROM p_generation AND (lease_until > clock_timestamp()) IS TRUE FOR SHARE;
  IF NOT FOUND OR p_worker IS NULL OR p_generation IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  SELECT jsonb_build_object('turn', t.turn, 'memory', t.record->'working_memory',
           'sha256', t.record->>'working_memory_sha256')
    INTO answer
  FROM enrichedlab_agent.broker_research_turns t
  WHERE t.tenant_id = p_tenant AND t.run_id = p_run AND jsonb_typeof(t.record->'working_memory') = 'object'
  ORDER BY t.turn DESC
  LIMIT 1;
  RETURN answer;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_working_memory(text, uuid, text, bigint) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (88) ON CONFLICT DO NOTHING;
COMMIT;
