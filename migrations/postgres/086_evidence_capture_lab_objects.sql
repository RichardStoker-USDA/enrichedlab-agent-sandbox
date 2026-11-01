-- Lab objects in the research record. Migration 083 added the lab object requests and receipts, append-guarded like
-- the analysis and query requests and receipts, after migration 081 fixed the captured set, so no row of either was
-- captured. This adds both to evidence.capture_tables(), gives each 081's insert trigger, and adds their two insert
-- event types (`broker.object_requests`, `broker.object_receipts`) to every existing capture producer grant, which
-- evidence.capture_producer never changes once written. A grant then lists 63 event types (the limit is 64).
--
-- Cutover, per opted-in tenant, as the migration owner after this commits: one
-- evidence.capture_catchup(tenant, ARRAY['broker_object_receipts', 'broker_object_requests']) records the rows written
-- before the triggers (origin `caught_up`); evidence.capture_missing must then report none. A row written after the
-- triggers is logged in its own transaction and recorded by the recorder as any other.
--
-- Every broker migrate run applies 081 again before this, which briefly restores 081's list. 081 drops and recreates
-- the triggers of its own tables only, so these two keep theirs; a recorder batch that meets one of their entries in
-- that window raises capture_table_unknown and rolls back whole, and the entries are recorded by the next batch.
--
-- Rollback: drop the two evidence_capture triggers, then run 081's capture_tables() definition again. Records and grant
-- event types stay; a grant may list an event type no producer appends.
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

DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_object_requests', 'broker_object_receipts'] LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS evidence_capture ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER evidence_capture AFTER INSERT ON enrichedlab_agent.%I '
      'FOR EACH ROW EXECUTE FUNCTION evidence.capture_note(%L)', t, t);
  END LOOP;
END $$;

-- The grant tables force row security even for their owner, so each tenant's capture grants are read and updated with
-- that tenant set (as migration 079 does). The list is capture_producer's: every insert event, then the three updates.
DO $$
DECLARE
  t text;
  wanted text[];
BEGIN
  SELECT array_agg(evidence.capture_event_type(ct.table_name, 'insert') ORDER BY ct.table_name)
      || ARRAY['broker.tool_group_grant_revoked', 'broker.dataset_intake_bound', 'broker.reviewed_query_retired']
    INTO wanted FROM evidence.capture_tables() ct;
  FOR t IN SELECT b.tenant_id FROM enrichedlab_agent.broker_tenants b ORDER BY b.tenant_id LOOP
    PERFORM set_config('enrichedlab.tenant_id', t, true);
    UPDATE evidence.producer_grants g SET event_types = wanted
      FROM evidence.producers p
      WHERE p.tenant_id = g.tenant_id AND p.producer_id = g.producer_id AND g.tenant_id = t
        AND p.identity = 'broker:capture' AND g.event_types IS DISTINCT FROM wanted;
  END LOOP;
  PERFORM set_config('enrichedlab.tenant_id', '', true);
END;
$$;

COMMIT;
