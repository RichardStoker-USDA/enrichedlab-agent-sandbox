-- A lab's own database as a query source declares every relation its reader may read: the onboarding
-- tool accepts up to 128, and migration 038's cap of 64, sized for public reference snapshots, refused a lab database's
-- 86 at the plan's dry run. A source may now list up to 128 relations. One query still reads at most 64
-- (`broker_task_queries`), and the coordinator shows a source's relations to the research lead within its share of a
-- call. Source rows are immutable records and none is rewritten; adding the constraint checks the rows already there.
-- The table's lock waits at most two seconds, so a long transaction fails the migration instead of queuing readers.
-- Rollback: put back the cap of 64 while no source lists more; an older broker reads these rows as before.
BEGIN;
SET LOCAL lock_timeout = '2s';

ALTER TABLE enrichedlab_agent.broker_sql_sources DROP CONSTRAINT broker_sql_sources_relations_check;
ALTER TABLE enrichedlab_agent.broker_sql_sources ADD CONSTRAINT broker_sql_sources_relations_check
  CHECK (jsonb_typeof(relations) = 'array' AND jsonb_array_length(relations) BETWEEN 1 AND 128);

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (62) ON CONFLICT DO NOTHING;
COMMIT;
