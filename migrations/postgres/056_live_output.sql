-- Live output. While a sandboxed analysis runs, the worker that holds its lease reads the
-- runner's output and records it in numbered pieces, so a reader can watch it before the run ends. Only sanitized
-- text is kept: printable characters, LF, CR, TAB and SGR color codes. Each attempt keeps at most 4 MiB of text; past
-- that the worker counts what it left out. When the runner stops, the worker closes the attempt with the count, size
-- and SHA-256 of its stored text, which this database recomputes before it accepts them. The saved output files stay
-- the full record; live pieces are a bounded copy for watching.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_output_pieces (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  seq bigint NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  stream text NOT NULL,
  text text NOT NULL,
  -- Raw output bytes this piece first represents, and how many of them sanitizing removed.
  bytes integer NOT NULL,
  dropped integer NOT NULL,
  at timestamptz NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, seq),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  CHECK (seq > 0),
  CHECK (attempt >= 0),
  CHECK (lease_generation > 0),
  CHECK (stream IN ('stdout', 'stderr', 'combined')),
  CHECK (octet_length(text) BETWEEN 1 AND 16384),
  CHECK (text !~ '[\x01-\x08\x0b\x0c\x0e-\x1a\x1c-\x1f\x7f\u0080-\u009f]'),
  -- Unicode Bidi_Control characters reorder how the text around them is shown.
  CHECK (text !~ '[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]'),
  CHECK (regexp_replace(text, '\x1b\[[0-9;:]*m', '', 'g') !~ '\x1b'),
  CHECK (bytes BETWEEN 0 AND 16777216),
  CHECK (dropped BETWEEN 0 AND bytes)
);
-- A table an earlier draft of this migration made has no Bidi_Control check; it gains one here. Adding it is refused
-- while a stored piece holds such a character.
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
                  WHERE conrelid = 'enrichedlab_agent.broker_task_output_pieces'::regclass AND contype = 'c'
                    AND position('\u202a-\u202e' in pg_get_constraintdef(oid)) > 0) THEN
    ALTER TABLE enrichedlab_agent.broker_task_output_pieces
      ADD CHECK (text !~ '[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]');
  END IF;
END $$;
ALTER TABLE enrichedlab_agent.broker_task_output_pieces ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_task_output_pieces FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_task_output_pieces;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_task_output_pieces
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_task_output_closures (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  pieces integer NOT NULL,
  size bigint NOT NULL,
  sha256 text NOT NULL,
  -- Raw output bytes left out because the attempt passed its 4 MiB limit.
  dropped bigint NOT NULL,
  closed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  CHECK (attempt >= 0),
  CHECK (lease_generation > 0),
  CHECK (pieces >= 0),
  CHECK (size BETWEEN 0 AND 4194304),
  CHECK (sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (dropped >= 0)
);
ALTER TABLE enrichedlab_agent.broker_task_output_closures ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_task_output_closures FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_task_output_closures;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_task_output_closures
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- The worker holding the task's active lease appends pieces to the current attempt, numbered after the task's last
-- piece. `p_pieces` is a JSON array of {stream, text, bytes, dropped, at}, at most 256 per call. `p_first` is how many
-- of the attempt's pieces the worker knows are stored. A batch may start with pieces already stored after those, as
-- when a reply was lost and the worker sends them again with newer ones: they must equal the stored pieces in every
-- field and are not stored twice, and the rest are added. Any other gap is refused. Returns the last number.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_task_output(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint,
  p_first integer,
  p_pieces jsonb
)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_attempt integer;
  v_expires timestamptz;
  v_last bigint;
  v_kept bigint;
  v_adding bigint;
  v_count integer;
  v_size integer;
  v_stored integer;
BEGIN
  -- The lease must be active, as for runtime events (migration 003), and still unexpired once the row is locked.
  SELECT attempt, lease_expires_at INTO v_attempt, v_expires FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND task_id = p_task AND lease_owner = p_worker AND lease_generation = p_generation
    AND status IN ('leased', 'starting', 'running', 'cancelling') AND lease_expires_at > clock_timestamp()
  FOR SHARE;
  IF NOT FOUND OR v_expires <= clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE = '40001', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF jsonb_typeof(p_pieces) IS DISTINCT FROM 'array' OR jsonb_array_length(p_pieces) NOT BETWEEN 1 AND 256 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:task_output_invalid';
  END IF;
  -- One writer numbers a task's pieces at a time.
  PERFORM pg_advisory_xact_lock(hashtextextended('broker_task_output:' || p_tenant || ':' || p_task::text, 0));
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_task_output_closures
             WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = v_attempt) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:task_output_closed';
  END IF;
  SELECT coalesce(max(seq), 0) INTO v_last FROM enrichedlab_agent.broker_task_output_pieces
  WHERE tenant_id = p_tenant AND task_id = p_task;
  SELECT count(*) INTO v_count FROM enrichedlab_agent.broker_task_output_pieces
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = v_attempt;
  v_size := jsonb_array_length(p_pieces);
  IF p_first IS NULL OR p_first < 0 OR p_first > v_count THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:task_output_sequence';
  END IF;
  -- The batch's first pieces that are already stored, after the `p_first` the worker knows of.
  v_stored := least(v_count - p_first, v_size);
  BEGIN
    IF v_stored > 0 AND EXISTS (
      SELECT 1
      FROM (SELECT tail.stream, tail.text, tail.bytes, tail.dropped, tail.at,
                   row_number() OVER (ORDER BY tail.seq) AS n
            FROM (SELECT * FROM enrichedlab_agent.broker_task_output_pieces
                  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = v_attempt
                  ORDER BY seq OFFSET p_first LIMIT v_stored) AS tail) AS stored
      FULL JOIN (SELECT x.stream, x.text, x.bytes, x.dropped, x.at, x.n
                 FROM ROWS FROM (
                   jsonb_to_recordset(p_pieces) AS (stream text, text text, bytes integer, dropped integer,
                     at timestamptz)
                 ) WITH ORDINALITY AS x(stream, text, bytes, dropped, at, n)
                 WHERE x.n <= v_stored) AS sent
        ON stored.n = sent.n
      WHERE (stored.stream, stored.text, stored.bytes, stored.dropped, stored.at)
        IS DISTINCT FROM (sent.stream, sent.text, sent.bytes, sent.dropped, sent.at)) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:task_output_sequence';
    END IF;
  EXCEPTION WHEN invalid_text_representation OR invalid_datetime_format OR datetime_field_overflow
    OR numeric_value_out_of_range THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:task_output_invalid';
  END;
  IF v_stored = v_size THEN
    RETURN v_last;
  END IF;
  SELECT coalesce(sum(octet_length(text)), 0) INTO v_kept FROM enrichedlab_agent.broker_task_output_pieces
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = v_attempt;
  SELECT coalesce(sum(octet_length(x.text)), 0) INTO v_adding
  FROM ROWS FROM (jsonb_to_recordset(p_pieces) AS (text text)) WITH ORDINALITY AS x(text, n)
  WHERE x.n > v_stored;
  IF v_kept + v_adding > 4194304 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:task_output_too_large';
  END IF;
  BEGIN
    INSERT INTO enrichedlab_agent.broker_task_output_pieces (
      tenant_id, task_id, seq, attempt, lease_generation, stream, text, bytes, dropped, at)
    SELECT p_tenant, p_task, v_last + x.n - v_stored, v_attempt, p_generation, x.stream, x.text, x.bytes, x.dropped,
      x.at
    FROM ROWS FROM (
      jsonb_to_recordset(p_pieces) AS (stream text, text text, bytes integer, dropped integer, at timestamptz)
    ) WITH ORDINALITY AS x(stream, text, bytes, dropped, at, n)
    WHERE x.n > v_stored;
  EXCEPTION WHEN check_violation OR not_null_violation OR invalid_text_representation
    OR invalid_datetime_format OR datetime_field_overflow OR numeric_value_out_of_range THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:task_output_invalid';
  END;
  RETURN v_last + v_size - v_stored;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_task_output(text,uuid,text,bigint,integer,jsonb) FROM PUBLIC;

-- The worker holding the task's active lease closes the current attempt once its runner has stopped. The count, size
-- and SHA-256 must equal what the database recomputes from the attempt's stored pieces, concatenated in order. The same
-- closure again is accepted.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_close_task_output(
  p_tenant text,
  p_task uuid,
  p_worker text,
  p_generation bigint,
  p_pieces integer,
  p_size bigint,
  p_sha256 text,
  p_dropped bigint
)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_attempt integer;
  v_expires timestamptz;
  v_pieces integer;
  v_size bigint;
  v_sha text;
  v_existing enrichedlab_agent.broker_task_output_closures%ROWTYPE;
BEGIN
  SELECT attempt, lease_expires_at INTO v_attempt, v_expires FROM enrichedlab_agent.broker_tasks
  WHERE tenant_id = p_tenant AND task_id = p_task AND lease_owner = p_worker AND lease_generation = p_generation
    AND status IN ('leased', 'starting', 'running', 'cancelling') AND lease_expires_at > clock_timestamp()
  FOR SHARE;
  IF NOT FOUND OR v_expires <= clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE = '40001', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('broker_task_output:' || p_tenant || ':' || p_task::text, 0));
  SELECT count(*), coalesce(sum(octet_length(text)), 0),
    encode(sha256(convert_to(coalesce(string_agg(text, '' ORDER BY seq), ''), 'UTF8')), 'hex')
  INTO v_pieces, v_size, v_sha
  FROM enrichedlab_agent.broker_task_output_pieces
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = v_attempt;
  IF p_pieces IS DISTINCT FROM v_pieces OR p_size IS DISTINCT FROM v_size OR p_sha256 IS DISTINCT FROM v_sha
     OR p_dropped IS NULL OR p_dropped < 0 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:task_output_digest_mismatch';
  END IF;
  SELECT * INTO v_existing FROM enrichedlab_agent.broker_task_output_closures
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = v_attempt;
  IF FOUND THEN
    IF v_existing.pieces = v_pieces AND v_existing.sha256 = v_sha AND v_existing.dropped = p_dropped THEN
      RETURN v_sha;
    END IF;
    RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:task_output_closed';
  END IF;
  INSERT INTO enrichedlab_agent.broker_task_output_closures (
    tenant_id, task_id, attempt, lease_generation, pieces, size, sha256, dropped)
  VALUES (p_tenant, p_task, v_attempt, p_generation, v_pieces, v_size, v_sha, p_dropped);
  RETURN v_sha;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_close_task_output(text,uuid,text,bigint,integer,bigint,text,bigint) FROM PUBLIC;

-- A task reader's pieces after `p_after`, at most `p_limit` (1 to 500), with the task's status and attempt, the current
-- attempt's closure, and the task's saved output files. Null when the principal may not read the task.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_output_after(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_after bigint,
  p_limit integer
)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object(
    'status', t.status,
    'attempt', t.attempt,
    'pieces', coalesce((
      SELECT jsonb_agg(jsonb_build_object('seq', p.seq, 'attempt', p.attempt, 'stream', p.stream, 'text', p.text,
        'bytes', p.bytes, 'dropped', p.dropped, 'at', p.at) ORDER BY p.seq)
      FROM (
        SELECT * FROM enrichedlab_agent.broker_task_output_pieces AS q
        WHERE q.tenant_id = t.tenant_id AND q.task_id = t.task_id AND q.seq > greatest(coalesce(p_after, 0), 0)
        ORDER BY q.seq
        LIMIT least(greatest(coalesce(p_limit, 1), 1), 500)
      ) AS p), '[]'::jsonb),
    'closure', (
      SELECT jsonb_build_object('attempt', c.attempt, 'pieces', c.pieces, 'size', c.size, 'sha256', c.sha256,
        'dropped', c.dropped)
      FROM enrichedlab_agent.broker_task_output_closures AS c
      WHERE c.tenant_id = t.tenant_id AND c.task_id = t.task_id AND c.attempt = t.attempt),
    'artifacts', coalesce((
      SELECT jsonb_agg(jsonb_build_object('artifact', a.artifact_id, 'role', a.role, 'sha256', a.sha256,
        'size', a.size_bytes) ORDER BY a.created_at, a.artifact_id)
      FROM enrichedlab_agent.broker_artifacts AS a
      WHERE a.tenant_id = t.tenant_id AND a.task_id = t.task_id AND a.visibility = 'researcher'
        AND a.role IN ('stdout', 'stderr')), '[]'::jsonb))
  FROM enrichedlab_agent.broker_tasks AS t
  WHERE t.tenant_id = p_tenant AND t.task_id = p_task
    AND enrichedlab_agent.broker_task_can(p_tenant, p_principal, p_task, 'read')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_output_after(text,uuid,uuid,bigint,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (56) ON CONFLICT DO NOTHING;
COMMIT;
