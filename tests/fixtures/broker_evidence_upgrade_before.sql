-- Evidence upgrade qualification, part 1 (a database at layout 5, migrations 001 to 078 applied by file): a site with a
-- receipt producer bound to an adapter role, appending one receipt of the live shape as layout 5 allows.
\set ON_ERROR_STOP on
SET client_min_messages = warning;
SELECT set_config('enrichedlab.tenant_id', 'upgrade-site', false);
INSERT INTO enrichedlab_agent.broker_tenants (tenant_id, name) VALUES ('upgrade-site', 'Upgrade site');
INSERT INTO enrichedlab_agent.broker_research_groups (tenant_id, group_key, name, max_active_agents,
    max_active_agents_per_principal, max_queued_tasks, retention_days)
  VALUES ('upgrade-site', 'lab-u', 'Lab U', 1, 1, 1, 30);
INSERT INTO evidence.producers (tenant_id, producer_id, identity, kind, database_role)
  VALUES ('upgrade-site', '00000000-0000-4000-8000-0000000000f1', 'openshell/receipts-upgrade', 'sandbox', NULL);
INSERT INTO evidence.adapter_bindings (tenant_id, producer_id, role_name)
  VALUES ('upgrade-site', '00000000-0000-4000-8000-0000000000f1', 'evidence_adapter_qual');
INSERT INTO evidence.producer_grants (tenant_id, producer_id, group_key, scope_kind, event_types, actor_role)
  VALUES ('upgrade-site', '00000000-0000-4000-8000-0000000000f1', 'lab-u', 'standalone', ARRAY['sandbox.receipt'], 'sandbox');
GRANT USAGE ON SCHEMA evidence TO evidence_adapter_qual;
GRANT EXECUTE ON FUNCTION evidence.append(text,text,text,text,uuid,uuid,text,text,timestamptz,text,text) TO evidence_adapter_qual;
SELECT evidence.ensure_record_partitions(1);
\set receipt_sample `cat /workspace/tests/fixtures/sandbox_receipt_sample.json`
SET SESSION AUTHORIZATION evidence_adapter_qual;
SELECT evidence.append('upgrade-site', 'openshell/receipts-upgrade', 'lab-u', 'standalone', NULL, NULL, 'sandbox.receipt',
  'upgrade-key-0001', '2026-10-24T12:00:00Z', :'receipt_sample', encode(sha256(convert_to(:'receipt_sample', 'UTF8')), 'hex'))->>'sequence' AS before_079;
RESET SESSION AUTHORIZATION;
