-- Evidence upgrade qualification, part 2 (after migration 079 on the part 1 database): the receipt producer keeps
-- appending receipts of the live shape, the reviewed schema was registered once by the migration, and a free-form
-- payload is now refused.
\set ON_ERROR_STOP on
SET client_min_messages = warning;
\set receipt_sample `cat /workspace/tests/fixtures/sandbox_receipt_sample.json`
CREATE OR REPLACE FUNCTION pg_temp.refused(p_sql text, p_message text) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE p_sql;
  RAISE EXCEPTION 'expected refusal % did not happen for: %', p_message, p_sql;
EXCEPTION WHEN OTHERS THEN
  IF SQLERRM NOT LIKE p_message || '%' THEN
    RAISE EXCEPTION 'expected % but got % for: %', p_message, SQLERRM, p_sql;
  END IF;
END;
$$;
SELECT set_config('qual.receipt', :'receipt_sample', false);
DO $$ BEGIN
  PERFORM set_config('enrichedlab.tenant_id', 'upgrade-site', false);
  IF obj_description(to_regnamespace('evidence'), 'pg_namespace') <> 'enrichedlab evidence layout 6'
    OR (SELECT count(*) FROM evidence.event_schemas WHERE tenant_id = 'upgrade-site' AND event_type = 'sandbox.receipt') <> 1 THEN
    RAISE EXCEPTION 'the upgrade did not lay out 6 with one receipt schema';
  END IF;
END $$;
SET SESSION AUTHORIZATION evidence_adapter_qual;
DO $$
DECLARE r jsonb; p text := current_setting('qual.receipt');
BEGIN
  -- The receipt written before 079, retried, is answered with its receipt.
  r := evidence.append('upgrade-site', 'openshell/receipts-upgrade', 'lab-u', 'standalone', NULL, NULL, 'sandbox.receipt',
    'upgrade-key-0001', '2026-10-24T12:00:00Z', p, encode(sha256(convert_to(p, 'UTF8')), 'hex'));
  IF NOT (r->>'replayed')::boolean THEN RAISE EXCEPTION 'the pre-upgrade receipt was not replayed: %', r; END IF;
  r := evidence.append('upgrade-site', 'openshell/receipts-upgrade', 'lab-u', 'standalone', NULL, NULL, 'sandbox.receipt',
    'upgrade-key-0002', '2026-10-24T12:01:00Z', p, encode(sha256(convert_to(p, 'UTF8')), 'hex'));
  IF (r->>'sequence')::int <> 2 THEN RAISE EXCEPTION 'the receipt after the upgrade was not appended: %', r; END IF;
  PERFORM pg_temp.refused($q$SELECT evidence.append('upgrade-site', 'openshell/receipts-upgrade', 'lab-u', 'standalone', NULL, NULL, 'sandbox.receipt', 'upgrade-key-0003', '2026-10-24T12:02:00Z', '{"receipt":"free form"}', encode(sha256(convert_to('{"receipt":"free form"}', 'UTF8')), 'hex'))$q$,
    'evidence:payload_not_metadata');
END $$;
RESET SESSION AUTHORIZATION;
SELECT 'evidence upgrade qualification passed' AS result;
