-- Working copies. Each attempt of an analysis has two run volumes in its tenant: a checksummed copy
-- of its input and the volume its outputs were written to. They serve custody during the run and recovery after it;
-- the results, the project package, lineage, reviews and kept decision texts are the record and never depend on
-- them. The operator's daily sweep reads each run volume's retention date, clears a volume after that date once its
-- analysis is finished and nothing still mounts it, and records here what it observed and cleared, so a scientist
-- reads when an analysis's working copy goes and when it went. Only the operator writes; project readers read.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_working_copies (
  tenant_id text NOT NULL,
  volume text NOT NULL,
  task_id uuid NOT NULL,
  role text NOT NULL,
  retention_until timestamptz NOT NULL,
  observed_at timestamptz NOT NULL,
  cleared_at timestamptz,
  clear_receipt_sha256 text,
  PRIMARY KEY (tenant_id, volume),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  CHECK (role IN ('immutable-input', 'durable-artifacts')),
  CHECK (volume ~ '^(input|artifacts)-(run|retained)-[a-z0-9]([a-z0-9-]{0,54}[a-z0-9])?$'),
  CHECK ((cleared_at IS NULL) = (clear_receipt_sha256 IS NULL)),
  CHECK (clear_receipt_sha256 IS NULL OR clear_receipt_sha256 ~ '^sha256:[0-9a-f]{64}$')
);
CREATE INDEX IF NOT EXISTS broker_task_working_copies_task
  ON enrichedlab_agent.broker_task_working_copies (tenant_id, task_id);
ALTER TABLE enrichedlab_agent.broker_task_working_copies ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_task_working_copies FROM PUBLIC;

-- The sweep's view of the analyses its volumes belong to: state, finish time and how many files custody kept.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_working_copy_tasks(p_tenant text, p_tasks uuid[])
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id', t.task_id, 'status', t.status, 'finished_at', t.finished_at,
      'retained_files', (SELECT count(*) FROM enrichedlab_agent.broker_artifacts a
                         WHERE a.tenant_id = t.tenant_id AND a.task_id = t.task_id AND a.role = 'result'))
      ORDER BY t.task_id), '[]'::jsonb)
  FROM enrichedlab_agent.broker_tasks t
  WHERE t.tenant_id = p_tenant AND t.task_id = ANY (coalesce(p_tasks, '{}'));
$$;

-- The sweep records each run volume it observed and each it cleared. A cleared volume stays cleared, and a volume
-- never moves to another analysis or role; a retention date changes only while the volume exists.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_working_copies(p_tenant text, p_items jsonb)
RETURNS integer LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  item jsonb;
  prior enrichedlab_agent.broker_task_working_copies%ROWTYPE;
  n integer := 0;
BEGIN
  IF p_items IS NULL OR jsonb_typeof(p_items) <> 'array' OR jsonb_array_length(p_items) > 4096 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:working_copy_invalid';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(p_items) LOOP
    IF jsonb_typeof(item) <> 'object' OR NOT (item ?& ARRAY['volume', 'task_id', 'role', 'retention_until'])
       OR (item ? 'cleared_at') <> (item ? 'clear_receipt_sha256') THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:working_copy_invalid';
    END IF;
    SELECT * INTO prior FROM enrichedlab_agent.broker_task_working_copies
    WHERE tenant_id = p_tenant AND volume = item->>'volume' FOR UPDATE;
    IF FOUND THEN
      IF prior.task_id <> (item->>'task_id')::uuid OR prior.role <> item->>'role' THEN
        RAISE EXCEPTION USING ERRCODE = '23514', MESSAGE = 'broker:working_copy_mismatch';
      END IF;
      IF prior.cleared_at IS NULL THEN
        UPDATE enrichedlab_agent.broker_task_working_copies
        SET retention_until = (item->>'retention_until')::timestamptz, observed_at = clock_timestamp(),
            cleared_at = (item->>'cleared_at')::timestamptz, clear_receipt_sha256 = item->>'clear_receipt_sha256'
        WHERE tenant_id = p_tenant AND volume = prior.volume;
        n := n + 1;
      END IF;
    ELSE
      INSERT INTO enrichedlab_agent.broker_task_working_copies
        (tenant_id, volume, task_id, role, retention_until, observed_at, cleared_at, clear_receipt_sha256)
      VALUES (p_tenant, item->>'volume', (item->>'task_id')::uuid, item->>'role',
              (item->>'retention_until')::timestamptz, clock_timestamp(),
              (item->>'cleared_at')::timestamptz, item->>'clear_receipt_sha256');
      n := n + 1;
    END IF;
  END LOOP;
  RETURN n;
END;
$$;

-- A project reader sees, per analysis, until when its working copy is kept or when it was cleared.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_working_copies(p_tenant text, p_actor uuid, p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found'; END IF;
  RETURN (SELECT coalesce(jsonb_agg(jsonb_build_object('task_id', c.task_id, 'kept_until', c.kept_until,
      'cleared_at', c.cleared_at, 'volumes', c.volumes) ORDER BY c.task_id), '[]'::jsonb)
    FROM (SELECT w.task_id, max(w.retention_until) AS kept_until, count(*) AS volumes,
                 CASE WHEN bool_and(w.cleared_at IS NOT NULL) THEN max(w.cleared_at) END AS cleared_at
          FROM enrichedlab_agent.broker_task_working_copies w
          JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = w.tenant_id AND pt.task_id = w.task_id
          WHERE w.tenant_id = p_tenant AND pt.project_id = p_project
          GROUP BY w.task_id) c);
END $$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'enrichedlab_agent'
      AND p.proname IN ('broker_working_copy_tasks', 'broker_record_working_copies', 'broker_project_working_copies')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (33) ON CONFLICT DO NOTHING;
COMMIT;
