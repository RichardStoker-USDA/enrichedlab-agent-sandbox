-- A lab source's schema card follows its data. The card command now writes into each card the spec it measured with
-- (`spec`) and, for a live lab source, the version row its database stated in the same transaction (`measured_at`);
-- registration keeps both in columns. The run's sources carry the card's version and the version the source's newest
-- completed query read, so the coordinator can tell the lead when the card is behind the data. The query worker
-- re-measures a live source's card once per new version, as the source's own reader with the newest card's spec, and
-- registers it through the checks the operator's registration applies; a refused or failed refresh leaves the previous
-- card in use and records why. Nothing here changes a source's declaration: a relation or column the reader can no
-- longer read is recorded as drift for an operator, never repaired.
BEGIN;

ALTER TABLE enrichedlab_agent.broker_sql_source_cards ADD COLUMN IF NOT EXISTS source_version jsonb;
ALTER TABLE enrichedlab_agent.broker_sql_source_cards ADD COLUMN IF NOT EXISTS spec jsonb;
ALTER TABLE enrichedlab_agent.broker_sql_source_cards DROP CONSTRAINT IF EXISTS broker_sql_source_cards_source_version_check;
ALTER TABLE enrichedlab_agent.broker_sql_source_cards ADD CONSTRAINT broker_sql_source_cards_source_version_check
  CHECK (source_version IS NULL OR (jsonb_typeof(source_version) = 'object' AND octet_length(source_version::text) <= 2048));
ALTER TABLE enrichedlab_agent.broker_sql_source_cards DROP CONSTRAINT IF EXISTS broker_sql_source_cards_spec_check;
ALTER TABLE enrichedlab_agent.broker_sql_source_cards ADD CONSTRAINT broker_sql_source_cards_spec_check
  CHECK (spec IS NULL OR (jsonb_typeof(spec) = 'object' AND octet_length(spec::text) <= 65536));

-- One row per (source, version) a refresh was started for; at most one running per source. A running row older than 15
-- minutes belongs to a worker that stopped and is closed as abandoned by the next start.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_sql_source_card_refreshes (
  tenant_id text NOT NULL,
  source_key text NOT NULL,
  version_sha256 text NOT NULL CHECK (version_sha256 ~ '^[0-9a-f]{64}$'),
  source_version jsonb NOT NULL CHECK (jsonb_typeof(source_version) = 'object' AND octet_length(source_version::text) <= 2048),
  state text NOT NULL CHECK (state IN ('running', 'registered', 'refused', 'failed', 'abandoned')),
  code text CHECK (code IS NULL OR code ~ '^[a-z0-9_]{1,80}$'),
  card_sha256 text CHECK (card_sha256 IS NULL OR card_sha256 ~ '^sha256:[0-9a-f]{64}$'),
  spec jsonb NOT NULL CHECK (jsonb_typeof(spec) = 'object' AND octet_length(spec::text) <= 65536),
  worker text NOT NULL CHECK (char_length(worker) BETWEEN 1 AND 200),
  started_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  finished_at timestamptz,
  PRIMARY KEY (tenant_id, source_key, version_sha256),
  FOREIGN KEY (tenant_id, source_key) REFERENCES enrichedlab_agent.broker_sql_sources (tenant_id, source_key),
  CHECK ((state = 'running') = (finished_at IS NULL)),
  CHECK ((state = 'registered') = (card_sha256 IS NOT NULL)),
  CHECK ((state IN ('running', 'registered')) = (code IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS broker_sql_source_card_refreshes_one_running
  ON enrichedlab_agent.broker_sql_source_card_refreshes (tenant_id, source_key) WHERE state = 'running';
ALTER TABLE enrichedlab_agent.broker_sql_source_card_refreshes ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_sql_source_card_refreshes FROM PUBLIC;

-- The card shape the coordinator reads, checked element by element against the source (migration 053's checks, moved
-- here so the operator's registration and the worker's refresh apply the same ones).
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_sql_source_card_check(s enrichedlab_agent.broker_sql_sources, p_card jsonb)
RETURNS void
LANGUAGE plpgsql
STABLE
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF jsonb_typeof(p_card) IS DISTINCT FROM 'object'
    OR p_card->>'schema' IS DISTINCT FROM 'enrichedlab.sql-source-card.v1'
    OR jsonb_typeof(p_card->'relations') IS DISTINCT FROM 'array'
    OR jsonb_typeof(p_card->'joins') IS DISTINCT FROM 'array'
    OR jsonb_typeof(p_card->'values') IS DISTINCT FROM 'array'
    OR jsonb_typeof(p_card->'lookups') IS DISTINCT FROM 'array'
    OR octet_length(p_card::text) > 262144
    OR p_card->>'source' IS DISTINCT FROM s.source_key THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_invalid';
  END IF;
  IF p_card->>'manifest_sha256' IS DISTINCT FROM enrichedlab_agent.broker_sql_source_card_key(s.snapshot) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_snapshot_mismatch';
  END IF;
  IF EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'relations') r
      WHERE jsonb_typeof(r) IS DISTINCT FROM 'object' OR jsonb_typeof(r->'name') IS DISTINCT FROM 'string'
        OR jsonb_typeof(r->'rows') IS DISTINCT FROM 'number' OR (r->>'rows') !~ '^(0|[1-9][0-9]{0,17})$'
        OR NOT EXISTS (SELECT 1 FROM jsonb_array_elements(s.relations) x WHERE x->>'name' = r->>'name'))
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'joins') j
      WHERE jsonb_typeof(j) IS DISTINCT FROM 'object' OR jsonb_typeof(j->'on') IS DISTINCT FROM 'string'
        OR jsonb_typeof(j->'rows') IS DISTINCT FROM 'number' OR jsonb_typeof(j->'matched') IS DISTINCT FROM 'number'
        OR CASE WHEN (j->>'rows') ~ '^(0|[1-9][0-9]{0,17})$' AND (j->>'matched') ~ '^(0|[1-9][0-9]{0,17})$'
             THEN (j->>'matched')::numeric > (j->>'rows')::numeric ELSE true END)
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'values') vc
      WHERE jsonb_typeof(vc) IS DISTINCT FROM 'object' OR jsonb_typeof(vc->'column') IS DISTINCT FROM 'string'
        OR jsonb_typeof(vc->'values') IS DISTINCT FROM 'array')
    OR EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'lookups') l
      WHERE jsonb_typeof(l) IS DISTINCT FROM 'object' OR jsonb_typeof(l->'relation') IS DISTINCT FROM 'string'
        OR jsonb_typeof(l->'columns') IS DISTINCT FROM 'array' OR jsonb_typeof(l->'entries') IS DISTINCT FROM 'array'
        OR NOT EXISTS (SELECT 1 FROM jsonb_array_elements(s.relations) x WHERE x->>'name' = l->>'relation')) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_invalid';
  END IF;
  IF EXISTS (SELECT 1 FROM jsonb_array_elements(p_card->'lookups') l
      WHERE jsonb_array_length(l->'columns') < 2
        OR EXISTS (SELECT 1 FROM jsonb_array_elements(l->'columns') c WHERE jsonb_typeof(c) IS DISTINCT FROM 'string')
        OR EXISTS (SELECT 1 FROM jsonb_array_elements(l->'entries') e
          WHERE jsonb_typeof(e) IS DISTINCT FROM 'array'
            OR jsonb_array_length(e) IS DISTINCT FROM jsonb_array_length(l->'columns'))) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_invalid';
  END IF;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_sql_source_card_check(enrichedlab_agent.broker_sql_sources, jsonb) FROM PUBLIC;

-- A spec the card command accepts: an object naming the source, with only joins, values and lookups lists.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_sql_source_card_spec_valid(p_source text, p_spec jsonb)
RETURNS boolean
LANGUAGE sql
IMMUTABLE
SET search_path = pg_catalog
AS $$
  SELECT jsonb_typeof(p_spec) = 'object' AND octet_length(p_spec::text) <= 65536 AND p_spec->>'source' = p_source
    AND NOT EXISTS (SELECT 1 FROM jsonb_object_keys(p_spec) k WHERE k NOT IN ('source', 'joins', 'values', 'lookups'))
    AND coalesce(jsonb_typeof(p_spec->'joins'), 'array') = 'array'
    AND coalesce(jsonb_typeof(p_spec->'values'), 'array') = 'array'
    AND coalesce(jsonb_typeof(p_spec->'lookups'), 'array') = 'array'
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_sql_source_card_spec_valid(text, jsonb) FROM PUBLIC;

-- Operator: register a card, as migration 053 defined it, keeping the card's `measured_at` and `spec` in their columns.
-- A live lab source's card must carry both, so the worker can refresh it; a pinned public source's card carries no
-- `measured_at`. The same card registered again returns the stored row.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_register_sql_source_card(p_tenant text, p_card jsonb)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  s enrichedlab_agent.broker_sql_sources%ROWTYPE;
  v enrichedlab_agent.broker_sql_source_cards%ROWTYPE;
  digest text;
BEGIN
  SELECT * INTO s FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id = p_tenant AND source_key = p_card->>'source';
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_not_found';
  END IF;
  PERFORM enrichedlab_agent.broker_sql_source_card_check(s, p_card);
  IF s.snapshot->>'kind' = 'live' THEN
    IF jsonb_typeof(p_card->'measured_at') IS DISTINCT FROM 'object' OR octet_length((p_card->'measured_at')::text) > 2048 THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_version_required';
    END IF;
    IF NOT coalesce(enrichedlab_agent.broker_sql_source_card_spec_valid(s.source_key, p_card->'spec'), false) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_spec_required';
    END IF;
  ELSIF p_card ? 'measured_at' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_version_unexpected';
  END IF;
  digest := 'sha256:' || encode(sha256(convert_to(p_card::text, 'UTF8')), 'hex');
  SELECT * INTO v FROM enrichedlab_agent.broker_sql_source_cards
  WHERE tenant_id = p_tenant AND source_key = s.source_key AND card_sha256 = digest;
  IF NOT FOUND THEN
    INSERT INTO enrichedlab_agent.broker_sql_source_cards (tenant_id, source_key, card_sha256, manifest_sha256, card,
      source_version, spec)
    VALUES (p_tenant, s.source_key, digest, p_card->>'manifest_sha256', p_card, p_card->'measured_at',
      CASE WHEN enrichedlab_agent.broker_sql_source_card_spec_valid(s.source_key, p_card->'spec') THEN p_card->'spec' END)
    RETURNING * INTO v;
  END IF;
  RETURN jsonb_build_object('source', v.source_key, 'card_sha256', v.card_sha256, 'created_at', v.created_at,
    'source_version', v.source_version);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_register_sql_source_card(text, jsonb) FROM PUBLIC;

-- Worker: start re-measuring a live source's card for a version a completed query receipt of that source read. Returns
-- null when nothing is to be done: the source is not live, its newest card has no spec or already has this version, a
-- refresh for this version already ran, or another is running. Otherwise returns what the card command needs: the
-- source as the card command takes it, the newest card's spec and the version's digest.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_begin_sql_source_card_refresh(p_tenant text, p_source text,
  p_version jsonb, p_worker text)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  s enrichedlab_agent.broker_sql_sources%ROWTYPE;
  c enrichedlab_agent.broker_sql_source_cards%ROWTYPE;
  version_digest text;
BEGIN
  IF jsonb_typeof(p_version) IS DISTINCT FROM 'object' OR octet_length(p_version::text) > 2048
    OR char_length(coalesce(p_worker, '')) NOT BETWEEN 1 AND 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:card_refresh_invalid';
  END IF;
  SELECT * INTO s FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id = p_tenant AND source_key = p_source;
  IF NOT FOUND OR s.snapshot->>'kind' IS DISTINCT FROM 'live' THEN
    RETURN NULL;
  END IF;
  -- The version must be one the source's own reader returned to a completed query, never one the caller composed.
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_query_receipts q
      JOIN enrichedlab_agent.broker_task_queries t ON t.tenant_id = q.tenant_id AND t.task_id = q.task_id
      WHERE q.tenant_id = p_tenant AND t.source_key = p_source AND q.status = 'completed'
        AND q.source_version = p_version) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:card_refresh_version_unrecorded';
  END IF;
  SELECT * INTO c FROM enrichedlab_agent.broker_sql_source_cards
  WHERE tenant_id = p_tenant AND source_key = p_source
    AND manifest_sha256 = enrichedlab_agent.broker_sql_source_card_key(s.snapshot)
  ORDER BY created_at DESC, card_sha256 LIMIT 1;
  IF NOT FOUND OR c.spec IS NULL OR c.source_version = p_version THEN
    RETURN NULL;
  END IF;
  UPDATE enrichedlab_agent.broker_sql_source_card_refreshes
  SET state = 'abandoned', code = 'abandoned', finished_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND source_key = p_source AND state = 'running'
    AND started_at < clock_timestamp() - interval '15 minutes';
  version_digest := encode(sha256(convert_to(p_version::text, 'UTF8')), 'hex');
  BEGIN
    INSERT INTO enrichedlab_agent.broker_sql_source_card_refreshes (tenant_id, source_key, version_sha256,
      source_version, state, spec, worker)
    VALUES (p_tenant, p_source, version_digest, p_version, 'running', c.spec, p_worker);
  EXCEPTION WHEN unique_violation THEN
    RETURN NULL;
  END;
  RETURN jsonb_build_object(
    'version_sha256', version_digest,
    'spec', c.spec,
    'source', jsonb_build_object('source', s.source_key, 'schema', s.schema_name, 'relations', s.relations,
      'statement_timeout_ms', s.statement_timeout_ms, 'snapshot', s.snapshot),
    'connection_ref', s.connection_ref);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_begin_sql_source_card_refresh(text, text, jsonb, text) FROM PUBLIC;

-- Worker: finish a running refresh. With a card, the card must pass the registration's checks for the source's current
-- key, state the version the refresh started for (`measured_at`) and carry the spec the refresh handed out and kept in
-- its row; it is registered with both. Without one, `p_code` says why (`version_moved`, `declaration_drift`, or a failure code) and the
-- previous card stays in use.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_finish_sql_source_card_refresh(p_tenant text, p_source text,
  p_version_sha256 text, p_worker text, p_card jsonb, p_code text)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  s enrichedlab_agent.broker_sql_sources%ROWTYPE;
  f enrichedlab_agent.broker_sql_source_card_refreshes%ROWTYPE;
  spec jsonb;
  digest text;
  outcome text;
  reason text;
BEGIN
  SELECT * INTO f FROM enrichedlab_agent.broker_sql_source_card_refreshes
  WHERE tenant_id = p_tenant AND source_key = p_source AND version_sha256 = p_version_sha256
    AND state = 'running' AND worker = p_worker
  FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:card_refresh_not_running';
  END IF;
  SELECT * INTO s FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id = p_tenant AND source_key = p_source;
  -- The spec the refresh handed out, kept in its row: a card measured with any other spec is refused.
  spec := f.spec;
  IF p_card IS NULL THEN
    reason := coalesce(p_code, 'card_refresh_failed');
    IF reason !~ '^[a-z0-9_]{1,80}$' THEN
      reason := 'card_refresh_failed';
    END IF;
    outcome := CASE WHEN reason IN ('version_moved', 'declaration_drift') THEN 'refused' ELSE 'failed' END;
  ELSIF p_card->'measured_at' IS DISTINCT FROM f.source_version THEN
    outcome := 'refused';
    reason := 'version_moved';
  ELSIF p_card->'spec' IS DISTINCT FROM spec THEN
    outcome := 'refused';
    reason := 'card_spec_changed';
  ELSE
    BEGIN
      PERFORM enrichedlab_agent.broker_sql_source_card_check(s, p_card);
    EXCEPTION WHEN SQLSTATE '22023' THEN
      outcome := 'refused';
      reason := 'card_invalid';
    END;
  END IF;
  IF outcome IS NULL THEN
    digest := 'sha256:' || encode(sha256(convert_to(p_card::text, 'UTF8')), 'hex');
    INSERT INTO enrichedlab_agent.broker_sql_source_cards (tenant_id, source_key, card_sha256, manifest_sha256, card,
      source_version, spec)
    VALUES (p_tenant, p_source, digest, p_card->>'manifest_sha256', p_card, f.source_version, spec)
    ON CONFLICT (tenant_id, source_key, card_sha256) DO NOTHING;
    UPDATE enrichedlab_agent.broker_sql_source_card_refreshes
    SET state = 'registered', card_sha256 = digest, finished_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND source_key = p_source AND version_sha256 = p_version_sha256;
    RETURN jsonb_build_object('state', 'registered', 'card_sha256', digest);
  END IF;
  UPDATE enrichedlab_agent.broker_sql_source_card_refreshes
  SET state = outcome, code = reason, finished_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND source_key = p_source AND version_sha256 = p_version_sha256;
  RETURN jsonb_build_object('state', outcome, 'code', reason);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_finish_sql_source_card_refresh(text, text, text, text, jsonb, text) FROM PUBLIC;

-- The run's sources, as migration 071 defined them, with the newest card's version (`card_source_version`) and the
-- version the source's newest completed query read (`source_version`).
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_sql_sources(p_tenant text, p_run uuid, p_worker text,
  p_generation bigint)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  r enrichedlab_agent.broker_research_runs%ROWTYPE;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  RETURN coalesce((
    SELECT jsonb_agg(jsonb_build_object('profile', s.profile_key, 'source', s.source_key, 'title', s.title,
      'description', s.description, 'release', s.release, 'relations', s.relations, 'functions', to_jsonb(s.functions),
      'row_cap', s.row_cap, 'schema', s.schema_name,
        'reviewed_tools',coalesce((SELECT jsonb_agg(t.definition ORDER BY t.tool_key,t.version DESC)
          FROM enrichedlab_agent.broker_reviewed_queries t WHERE t.tenant_id=s.tenant_id AND t.source_key=s.source_key
            AND t.definition->>'state'='enabled'),'[]'::jsonb))
      || coalesce((SELECT jsonb_build_object('card', c.card, 'card_sha256', c.card_sha256)
           || CASE WHEN c.source_version IS NULL THEN '{}'::jsonb
                ELSE jsonb_build_object('card_source_version', c.source_version) END
        FROM enrichedlab_agent.broker_sql_source_cards c
        WHERE c.tenant_id = s.tenant_id AND c.source_key = s.source_key
          AND c.manifest_sha256 = enrichedlab_agent.broker_sql_source_card_key(s.snapshot)
        ORDER BY c.created_at DESC, c.card_sha256 LIMIT 1), '{}'::jsonb)
      || coalesce((SELECT jsonb_build_object('source_version', q.source_version)
        FROM enrichedlab_agent.broker_query_receipts q
        JOIN enrichedlab_agent.broker_task_queries t ON t.tenant_id = q.tenant_id AND t.task_id = q.task_id
        WHERE q.tenant_id = s.tenant_id AND t.source_key = s.source_key AND q.status = 'completed'
          AND q.source_version IS NOT NULL
        ORDER BY q.finished_at DESC, q.recorded_at DESC LIMIT 1), '{}'::jsonb)
      ORDER BY s.profile_key)
    FROM enrichedlab_agent.broker_sql_sources s
    WHERE s.tenant_id = p_tenant AND s.profile_key = ANY (r.allowed_profiles)), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_sql_sources(text, uuid, text, bigint) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (82) ON CONFLICT DO NOTHING;
COMMIT;
