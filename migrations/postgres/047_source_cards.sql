-- Schema cards for query sources. A card states, for one source's snapshot, the facts a research
-- lead would otherwise spend queries on: each relation's keys and missing values, how relations join with the cast a
-- join needs, the values of small columns and the entries of lookup tables with how many rows use each. The broker's
-- `source-card` command measures it read-only as the source's reader; the operator registers it once per snapshot.
-- Cards are immutable: a new snapshot or a new declaration makes a new card, and the run reads the newest card of the
-- snapshot its source names. The run's sources gain `card` and `card_sha256`; everything else in that function is
-- migration 038's, and a coordinator that does not read the two keys is unchanged.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_sql_source_cards (
  tenant_id text NOT NULL,
  source_key text NOT NULL,
  card_sha256 text NOT NULL CHECK (card_sha256 ~ '^sha256:[0-9a-f]{64}$'),
  manifest_sha256 text NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
  card jsonb NOT NULL CHECK (jsonb_typeof(card) = 'object' AND octet_length(card::text) <= 262144),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, source_key, card_sha256),
  FOREIGN KEY (tenant_id, source_key) REFERENCES enrichedlab_agent.broker_sql_sources (tenant_id, source_key)
);
ALTER TABLE enrichedlab_agent.broker_sql_source_cards ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_sql_source_cards FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_sql_source_cards;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_sql_source_cards
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- Operator: register a card measured for a registered source. The card must name the source and the snapshot the
-- source records, and only relations the source lists. Its digest is computed here from the stored form, so the same
-- card registered again returns the stored row.
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
  IF jsonb_typeof(p_card) IS DISTINCT FROM 'object'
    OR p_card->>'schema' IS DISTINCT FROM 'enrichedlab.sql-source-card.v1'
    OR jsonb_typeof(p_card->'relations') IS DISTINCT FROM 'array'
    OR jsonb_typeof(p_card->'joins') IS DISTINCT FROM 'array'
    OR jsonb_typeof(p_card->'values') IS DISTINCT FROM 'array'
    OR jsonb_typeof(p_card->'lookups') IS DISTINCT FROM 'array'
    OR octet_length(p_card::text) > 262144 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_invalid';
  END IF;
  SELECT * INTO s FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id = p_tenant AND source_key = p_card->>'source';
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_not_found';
  END IF;
  IF p_card->>'manifest_sha256' IS DISTINCT FROM s.snapshot->>'manifest_sha256' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:sql_source_card_snapshot_mismatch';
  END IF;
  -- The shape the coordinator reads, element by element (the arrays themselves were checked above): a card it could not
  -- read whole is refused here rather than left out of every run. Relations and lookups must be the source's own. A
  -- count is a whole number of at most 18 digits written without a fraction or exponent, and a join matches no more
  -- rows than it has, the rule the coordinator's view applies to the same stored form.
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
  digest := 'sha256:' || encode(sha256(convert_to(p_card::text, 'UTF8')), 'hex');
  SELECT * INTO v FROM enrichedlab_agent.broker_sql_source_cards
  WHERE tenant_id = p_tenant AND source_key = s.source_key AND card_sha256 = digest;
  IF NOT FOUND THEN
    INSERT INTO enrichedlab_agent.broker_sql_source_cards (tenant_id, source_key, card_sha256, manifest_sha256, card)
    VALUES (p_tenant, s.source_key, digest, p_card->>'manifest_sha256', p_card)
    RETURNING * INTO v;
  END IF;
  RETURN jsonb_build_object('source', v.source_key, 'card_sha256', v.card_sha256, 'created_at', v.created_at);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_register_sql_source_card(text, jsonb) FROM PUBLIC;

-- The run's sources, as migration 038 defined them, each with the newest card registered for its snapshot.
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
      'row_cap', s.row_cap, 'schema', s.schema_name)
      || coalesce((SELECT jsonb_build_object('card', c.card, 'card_sha256', c.card_sha256)
        FROM enrichedlab_agent.broker_sql_source_cards c
        WHERE c.tenant_id = s.tenant_id AND c.source_key = s.source_key
          AND c.manifest_sha256 = s.snapshot->>'manifest_sha256'
        ORDER BY c.created_at DESC, c.card_sha256 LIMIT 1), '{}'::jsonb)
      ORDER BY s.profile_key)
    FROM enrichedlab_agent.broker_sql_sources s
    WHERE s.tenant_id = p_tenant AND s.profile_key = ANY (r.allowed_profiles)), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_sql_sources(text, uuid, text, bigint) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (47) ON CONFLICT DO NOTHING;
COMMIT;
