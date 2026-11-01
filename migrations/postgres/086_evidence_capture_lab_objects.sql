-- Lab objects in the research record. Migration 083 added the lab object requests and receipts, append-guarded like
-- the analysis and query requests and receipts, after migration 081 fixed the captured set, so no row of either was
-- captured. This adds both to evidence.capture_tables() and gives each 081's insert trigger.
--
-- Grants. A capture producer's grant lists the event types it may append, and 081 never changed a grant once written,
-- so a grant written before this (or by a recorder that read the earlier list while this ran) would refuse the two new
-- event types for good. evidence.capture_event_types() is now the one list (the insert event of every captured table,
-- then the three permitted updates; the evidence check requires it to equal the list capture_tables() derives). A scope
-- is ready only when its grant holds every type on it, and capture_producer brings a grant with an earlier list up to it
-- under a row lock taken without waiting (lock_not_available defers the recorder's batch, as any lock it meets does).
-- So a stale grant is repaired the next time its scope is recorded into or prepared, whatever the interleaving. This
-- migration also updates the existing grants at once, so most scopes stay ready. A grant then lists 63 types of its 64.
--
-- Cutover, per opted-in tenant, as the migration owner after this commits: evidence.capture_prepare_scopes until none
-- remain, one evidence.capture_catchup(tenant, ARRAY['broker_object_receipts', 'broker_object_requests']) for the rows
-- written before the triggers (origin `caught_up`), then evidence.capture_missing, which must report none. Schema
-- version 86 gates the coordinator and the lab object worker, and the evidence service starts its recorder only when
-- capture_event_types exists.
--
-- Every broker migrate run applies 081 again before this, which briefly restores 081's functions. 081 recreates only
-- its own tables' triggers, so these two keep theirs; a recorder batch that meets one of their entries in that window
-- raises capture_table_unknown and rolls back whole, and a grant written in that window is repaired as above.
--
-- Rollback, in order: move the coordinator, workers and evidence service to an image without 086; drop the two
-- evidence_capture triggers, so no entry is logged; run the recorder until evidence.capture_log holds no entry of either
-- table (count them as the migration owner); then run 081's capture_tables, capture_producer and capture_scope_ready
-- again, drop evidence.capture_event_types, run 085's broker_schema_ready and delete schema_migrations version 86.
-- Records stay; grants keep the two types, which no producer then appends. Restoring the list before the log is drained
-- would leave those entries unrecordable (capture_table_unknown on every batch that meets them).
BEGIN;

CREATE OR REPLACE FUNCTION evidence.capture_tables()
RETURNS TABLE (table_name text, key_columns text[], column_types jsonb)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
  SELECT it.table_name, it.key_columns, it.column_types FROM evidence.import_tables() it
  UNION ALL
  SELECT c.relname::text,
    (SELECT array_agg(a.attname::text ORDER BY k.ord) FROM pg_index i
       CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord)
       JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
       WHERE i.indrelid = c.oid AND i.indisprimary),
    (SELECT jsonb_object_agg(a.attname, format_type(a.atttypid, NULL)) FROM pg_attribute a
       WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped)
  FROM (VALUES ('broker_tool_groups'), ('broker_tool_group_members'), ('broker_tool_group_grants'),
               ('broker_object_requests'), ('broker_object_receipts')) AS listed(name)
  JOIN pg_class c ON c.relname = listed.name AND c.relkind IN ('r', 'p')
  JOIN pg_namespace n ON n.oid = c.relnamespace AND n.nspname = 'enrichedlab_agent'
$$;
REVOKE ALL ON FUNCTION evidence.capture_tables() FROM PUBLIC;

-- Written out rather than derived, so a readiness check per scope reads no catalog.
CREATE OR REPLACE FUNCTION evidence.capture_event_types() RETURNS text[] LANGUAGE sql IMMUTABLE
SET search_path = pg_catalog AS $$
  SELECT ARRAY[
    'broker.admin_audit', 'broker.analysis_receipts', 'broker.analysis_requests', 'broker.artifact_digests',
    'broker.artifact_kinds', 'broker.artifact_recoveries', 'broker.attempt_executions', 'broker.attempt_inputs',
    'broker.attempt_outputs', 'broker.campaign_events', 'broker.campaign_planner_call_starts',
    'broker.campaign_planner_calls', 'broker.campaign_planner_texts', 'broker.dataset_receipts',
    'broker.dataset_requests', 'broker.datasets', 'broker.document_receipts', 'broker.document_request_keys',
    'broker.document_requests', 'broker.figure_manuscript_events', 'broker.figure_receipts',
    'broker.figure_requests', 'broker.inbox_acknowledgements', 'broker.lab_note_events', 'broker.lab_notes',
    'broker.library_label_events', 'broker.library_paper_events', 'broker.library_papers',
    'broker.literature_reviews', 'broker.object_receipts', 'broker.object_requests', 'broker.plan_archived_keys',
    'broker.plan_events', 'broker.plan_pages', 'broker.plan_revisions', 'broker.profile_templates',
    'broker.project_audit', 'broker.project_checkpoints', 'broker.project_packages', 'broker.project_tasks',
    'broker.provenance_events', 'broker.query_receipts', 'broker.research_contexts', 'broker.research_orientations',
    'broker.research_run_events', 'broker.research_turns', 'broker.reviewed_queries', 'broker.sql_source_cards',
    'broker.sql_sources', 'broker.task_events', 'broker.task_generated_code', 'broker.task_methods',
    'broker.task_origins', 'broker.task_output_closures', 'broker.task_output_pieces', 'broker.task_queries',
    'broker.task_reviews', 'broker.tool_group_grants', 'broker.tool_group_members', 'broker.tool_groups',
    'broker.tool_group_grant_revoked', 'broker.dataset_intake_bound', 'broker.reviewed_query_retired'
  ]::text[]
$$;
REVOKE ALL ON FUNCTION evidence.capture_event_types() FROM PUBLIC;

CREATE OR REPLACE FUNCTION evidence.capture_scope_ready(p_tenant text, p_group text, p_scope_kind text, p_project uuid,
  p_run uuid)
RETURNS boolean LANGUAGE sql STABLE SET search_path = pg_catalog, evidence AS $$
  SELECT EXISTS (SELECT 1 FROM evidence.producers p JOIN evidence.producer_grants g
      ON g.tenant_id = p.tenant_id AND g.producer_id = p.producer_id
    WHERE p.tenant_id = p_tenant AND p.identity = 'broker:capture' AND p.enabled AND p.kind = 'broker'
      AND g.group_key = p_group AND g.scope_kind = p_scope_kind AND g.project_id IS NOT DISTINCT FROM p_project
      AND g.run_id IS NULL AND g.all_runs = (p_scope_kind = 'project')
      AND g.event_types @> evidence.capture_event_types())
    AND EXISTS (SELECT 1 FROM evidence.streams s WHERE s.tenant_id = p_tenant AND s.group_key = p_group
      AND s.scope_kind = p_scope_kind AND s.project_id IS NOT DISTINCT FROM p_project
      AND s.run_id IS NOT DISTINCT FROM p_run)
$$;
REVOKE ALL ON FUNCTION evidence.capture_scope_ready(text, text, text, uuid, uuid) FROM PUBLIC;

-- The tenant's capture producer, registered on first use, with a grant for this scope holding every capture event type.
-- A grant with an earlier list is brought up to it; its row is locked without waiting. Refuses when the producer was
-- disabled, so a disabled capture producer stops writes rather than leaving them unrecorded.
CREATE OR REPLACE FUNCTION evidence.capture_producer(p_tenant text, p_group text, p_scope_kind text, p_project uuid)
RETURNS uuid LANGUAGE plpgsql SET search_path = pg_catalog, evidence, enrichedlab_agent AS $$
DECLARE
  held evidence.producers%ROWTYPE;
  wanted constant text[] := evidence.capture_event_types();
  stale uuid;
BEGIN
  INSERT INTO evidence.producers (tenant_id, identity, kind, database_role)
    VALUES (p_tenant, 'broker:capture', 'broker', 'evidence_capture')
    ON CONFLICT (tenant_id, identity) DO NOTHING;
  SELECT * INTO held FROM evidence.producers WHERE tenant_id = p_tenant AND identity = 'broker:capture';
  IF NOT held.enabled OR held.kind <> 'broker' THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'evidence:capture_producer_disabled';
  END IF;
  INSERT INTO evidence.producer_grants (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, all_runs,
      event_types, actor_role)
    VALUES (p_tenant, held.producer_id, p_group, p_scope_kind, p_project, NULL, p_scope_kind = 'project', wanted,
      'broker')
    ON CONFLICT (tenant_id, producer_id, group_key, scope_kind, project_id, run_id, all_runs) DO NOTHING;
  SELECT g.grant_id INTO stale FROM evidence.producer_grants g
    WHERE g.tenant_id = p_tenant AND g.producer_id = held.producer_id AND g.group_key = p_group
      AND g.scope_kind = p_scope_kind AND g.project_id IS NOT DISTINCT FROM p_project AND g.run_id IS NULL
      AND g.all_runs = (p_scope_kind = 'project') AND NOT g.event_types @> wanted
    FOR UPDATE NOWAIT;
  IF FOUND THEN
    UPDATE evidence.producer_grants g SET event_types = wanted WHERE g.tenant_id = p_tenant AND g.grant_id = stale;
  END IF;
  RETURN held.producer_id;
END;
$$;
REVOKE ALL ON FUNCTION evidence.capture_producer(text, text, text, uuid) FROM PUBLIC;

DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_object_requests', 'broker_object_receipts'] LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS evidence_capture ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER evidence_capture AFTER INSERT ON enrichedlab_agent.%I '
      'FOR EACH ROW EXECUTE FUNCTION evidence.capture_note(%L)', t, t);
  END LOOP;
END $$;

-- The existing grants, at once. The grant tables force row security even for their owner, so each tenant's capture
-- grants are updated with that tenant set (as migration 079 does). A grant this misses is repaired on its next use.
DO $$
DECLARE t text;
BEGIN
  FOR t IN SELECT b.tenant_id FROM enrichedlab_agent.broker_tenants b ORDER BY b.tenant_id LOOP
    PERFORM set_config('enrichedlab.tenant_id', t, true);
    UPDATE evidence.producer_grants g SET event_types = evidence.capture_event_types()
      FROM evidence.producers p
      WHERE p.tenant_id = g.tenant_id AND p.producer_id = g.producer_id AND g.tenant_id = t
        AND p.identity = 'broker:capture' AND NOT g.event_types @> evidence.capture_event_types();
  END LOOP;
  PERFORM set_config('enrichedlab.tenant_id', '', true);
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (86) ON CONFLICT DO NOTHING;
COMMIT;
