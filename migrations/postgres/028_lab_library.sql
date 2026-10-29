-- Lab library. A lab keeps its papers in its own corpus of the document service; the broker keeps the
-- library: which papers the lab holds (the service's document id and the source SHA-256), the catalog fields read back
-- from the parse, who added each paper and when, removals with a reason, and typed labels. Every read and write first
-- requires membership of the lab, so a non-member learns nothing. Rows are immutable; removing and restoring a paper
-- and adding and removing a label are appended events. A label that only service accounts added reads as proposed
-- until a person adds it too. Additive: the running API, coordinator and worker keep working.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_library_papers (
  tenant_id text NOT NULL,
  paper_id uuid NOT NULL DEFAULT gen_random_uuid(),
  group_key text NOT NULL,
  -- The document service's identifier for this paper in the lab's corpus.
  document_id uuid NOT NULL,
  source_sha256 text NOT NULL,
  byte_size bigint NOT NULL,
  title text,
  authors jsonb NOT NULL,
  year integer,
  doi text,
  page_count integer,
  -- Counts and states the parse reported; never text from the paper.
  parse jsonb NOT NULL,
  added_by uuid NOT NULL,
  request_id uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, paper_id),
  UNIQUE (tenant_id, group_key, source_sha256),
  UNIQUE (tenant_id, group_key, document_id),
  UNIQUE (tenant_id, added_by, request_id),
  FOREIGN KEY (tenant_id, group_key) REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, added_by) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (source_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (byte_size BETWEEN 5 AND 104857600),
  CHECK (title IS NULL OR char_length(title) BETWEEN 1 AND 2048),
  CHECK (jsonb_typeof(authors) = 'array' AND jsonb_array_length(authors) <= 200 AND octet_length(authors::text) <= 32768),
  CHECK (year IS NULL OR year BETWEEN 1500 AND 2200),
  CHECK (doi IS NULL OR (char_length(doi) BETWEEN 4 AND 256 AND doi ~ '^10\.')),
  CHECK (page_count IS NULL OR page_count BETWEEN 1 AND 100000),
  CHECK (jsonb_typeof(parse) = 'object' AND octet_length(parse::text) <= 4096)
);
CREATE INDEX IF NOT EXISTS broker_library_papers_group ON enrichedlab_agent.broker_library_papers (tenant_id, group_key, created_at);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_library_paper_events (
  tenant_id text NOT NULL,
  event_id uuid NOT NULL DEFAULT gen_random_uuid(),
  paper_id uuid NOT NULL,
  kind text NOT NULL,
  actor_id uuid NOT NULL,
  request_id uuid NOT NULL,
  reason text,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, event_id),
  UNIQUE (tenant_id, actor_id, request_id),
  FOREIGN KEY (tenant_id, paper_id) REFERENCES enrichedlab_agent.broker_library_papers (tenant_id, paper_id),
  FOREIGN KEY (tenant_id, actor_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK ((kind = 'removed' AND reason IS NOT NULL AND char_length(reason) BETWEEN 1 AND 600)
    OR (kind = 'restored' AND reason IS NULL))
);
CREATE INDEX IF NOT EXISTS broker_library_paper_events_paper ON enrichedlab_agent.broker_library_paper_events (tenant_id, paper_id, created_at);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_library_label_events (
  tenant_id text NOT NULL,
  event_id uuid NOT NULL DEFAULT gen_random_uuid(),
  paper_id uuid NOT NULL,
  kind text NOT NULL,
  namespace text NOT NULL,
  value text NOT NULL,
  actor_id uuid NOT NULL,
  request_id uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, event_id),
  UNIQUE (tenant_id, actor_id, request_id),
  FOREIGN KEY (tenant_id, paper_id) REFERENCES enrichedlab_agent.broker_library_papers (tenant_id, paper_id),
  FOREIGN KEY (tenant_id, actor_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (kind IN ('added', 'removed')),
  CHECK (namespace IN ('topic', 'organism', 'food', 'compound', 'method', 'design', 'population')),
  CHECK (char_length(value) BETWEEN 1 AND 80 AND value = btrim(value) AND value !~ '[[:cntrl:]]')
);
CREATE INDEX IF NOT EXISTS broker_library_label_events_paper ON enrichedlab_agent.broker_library_label_events (tenant_id, paper_id, created_at);

DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['broker_library_papers', 'broker_library_paper_events', 'broker_library_label_events'] LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON enrichedlab_agent.%I FROM PUBLIC', t);
    EXECUTE format('DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.%I FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation()', t);
  END LOOP;
END $$;

-- The caller's role in an enabled lab of an enabled tenant, or lab_not_found.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_member(p_tenant text, p_actor uuid, p_group text)
RETURNS text LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r text;
BEGIN
  SELECT m.role INTO r FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = m.tenant_id AND a.principal_id = m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_tenants t ON t.tenant_id = m.tenant_id AND t.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND g.enabled
    WHERE m.tenant_id = p_tenant AND m.group_key = p_group AND m.principal_id = p_actor;
  IF r IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:lab_not_found'; END IF;
  RETURN r;
END $$;

-- A label is present when its newest event is an addition. It reads as set when a person added it since the last
-- removal, and as proposed when only service accounts did. Ordered by namespace and value.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_labels(p_tenant text, p_paper uuid)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  WITH ev AS (
    SELECT e.namespace, e.value, e.kind, e.created_at, a.issuer <> 'kubernetes://serviceaccount' AS by_person
    FROM enrichedlab_agent.broker_library_label_events e
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = e.tenant_id AND a.principal_id = e.actor_id
    WHERE e.tenant_id = p_tenant AND e.paper_id = p_paper
  ), cleared AS (
    SELECT namespace, value, max(created_at) AS at FROM ev WHERE kind = 'removed' GROUP BY namespace, value
  ), present AS (
    SELECT ev.namespace, ev.value, min(ev.created_at) AS since, bool_or(ev.by_person) AS by_person
    FROM ev LEFT JOIN cleared c ON c.namespace = ev.namespace AND c.value = ev.value
    WHERE ev.kind = 'added' AND (c.at IS NULL OR ev.created_at > c.at)
    GROUP BY ev.namespace, ev.value
  )
  SELECT coalesce(jsonb_agg(jsonb_build_object('namespace', namespace, 'value', value,
      'state', CASE WHEN by_person THEN 'set' ELSE 'proposed' END, 'since', since) ORDER BY namespace, value), '[]'::jsonb)
  FROM present
$$;

-- The newest removal when it is the paper's newest event; null while the paper is in the library.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_removal(p_tenant text, p_paper uuid)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT CASE WHEN x.kind = 'removed' THEN jsonb_build_object('removed_at', x.created_at, 'reason', x.reason,
      'by', a.display_name) END
  FROM (SELECT * FROM enrichedlab_agent.broker_library_paper_events e
        WHERE e.tenant_id = p_tenant AND e.paper_id = p_paper
        ORDER BY e.created_at DESC, e.event_id DESC LIMIT 1) x
  JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = x.tenant_id AND a.principal_id = x.actor_id
$$;

-- One paper as a lab member sees it. A lab lead or the person who added it may remove it.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_paper_json(p_tenant text, p_actor uuid, p_role text,
  x enrichedlab_agent.broker_library_papers)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT jsonb_build_object('paper_id', x.paper_id, 'lab', x.group_key, 'title', x.title, 'authors', x.authors,
    'year', x.year, 'doi', x.doi, 'page_count', x.page_count, 'byte_size', x.byte_size,
    'source_sha256', x.source_sha256, 'parse', x.parse,
    'added_by', jsonb_build_object('name', p.display_name,
      'kind', CASE WHEN p.issuer = 'kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END,
      'you', x.added_by = p_actor),
    'added_at', x.created_at,
    'labels', enrichedlab_agent.broker_library_labels(p_tenant, x.paper_id),
    'removed', enrichedlab_agent.broker_library_removal(p_tenant, x.paper_id),
    'can_remove', p_role = 'lead' OR x.added_by = p_actor)
  FROM enrichedlab_agent.broker_principals p WHERE p.tenant_id = p_tenant AND p.principal_id = x.added_by
$$;

-- The lab's papers in the library, newest first, and how many were removed.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library(p_tenant text, p_actor uuid, p_group text)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r text;
BEGIN
  r := enrichedlab_agent.broker_library_member(p_tenant, p_actor, p_group);
  RETURN jsonb_build_object('lab', p_group, 'role', r,
    'papers', (SELECT coalesce(jsonb_agg(enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x)
        ORDER BY x.created_at DESC, x.paper_id), '[]'::jsonb)
      FROM enrichedlab_agent.broker_library_papers x
      WHERE x.tenant_id = p_tenant AND x.group_key = p_group
        AND enrichedlab_agent.broker_library_removal(p_tenant, x.paper_id) IS NULL),
    'removed', (SELECT count(*) FROM enrichedlab_agent.broker_library_papers x
      WHERE x.tenant_id = p_tenant AND x.group_key = p_group
        AND enrichedlab_agent.broker_library_removal(p_tenant, x.paper_id) IS NOT NULL));
END $$;

-- One paper of the lab, removed or not, with its document identifier for the API's own use.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_paper(p_tenant text, p_actor uuid, p_group text, p_paper uuid)
RETURNS TABLE(paper jsonb, document_id uuid) LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r text; x enrichedlab_agent.broker_library_papers%ROWTYPE;
BEGIN
  r := enrichedlab_agent.broker_library_member(p_tenant, p_actor, p_group);
  SELECT * INTO x FROM enrichedlab_agent.broker_library_papers
    WHERE tenant_id = p_tenant AND group_key = p_group AND paper_id = p_paper;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:paper_not_found'; END IF;
  RETURN QUERY SELECT enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x), x.document_id;
END $$;

-- The lab's paper with these bytes, removed or not, or null: the API skips a second upload.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_find(p_tenant text, p_actor uuid, p_group text, p_sha256 text)
RETURNS TABLE(paper jsonb, document_id uuid) LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r text; x enrichedlab_agent.broker_library_papers%ROWTYPE;
BEGIN
  r := enrichedlab_agent.broker_library_member(p_tenant, p_actor, p_group);
  SELECT * INTO x FROM enrichedlab_agent.broker_library_papers
    WHERE tenant_id = p_tenant AND group_key = p_group AND source_sha256 = p_sha256;
  IF FOUND THEN
    RETURN QUERY SELECT enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x), x.document_id;
  END IF;
END $$;

-- Records a paper the API just placed in the lab's corpus, or returns the lab's paper with the same bytes (restoring
-- it when it had been removed). Idempotent per caller and request.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_record(p_tenant text, p_actor uuid, p_group text,
  p_document uuid, p_sha256 text, p_bytes bigint, p_title text, p_authors jsonb, p_year integer, p_doi text,
  p_pages integer, p_parse jsonb, p_request uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r text; x enrichedlab_agent.broker_library_papers%ROWTYPE; created boolean := false; restored boolean := false;
  prior enrichedlab_agent.broker_library_paper_events%ROWTYPE;
BEGIN
  r := enrichedlab_agent.broker_library_member(p_tenant, p_actor, p_group);
  IF p_request IS NULL OR p_document IS NULL OR p_sha256 IS NULL OR p_sha256 !~ '^[0-9a-f]{64}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:library_invalid'; END IF;
  SELECT * INTO x FROM enrichedlab_agent.broker_library_papers
    WHERE tenant_id = p_tenant AND added_by = p_actor AND request_id = p_request;
  IF FOUND THEN
    IF x.group_key <> p_group OR x.source_sha256 <> p_sha256 THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict'; END IF;
    RETURN jsonb_build_object('paper', enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x),
      'created', false, 'restored', false);
  END IF;
  -- The same request that restored a removed paper, repeated.
  SELECT * INTO prior FROM enrichedlab_agent.broker_library_paper_events
    WHERE tenant_id = p_tenant AND actor_id = p_actor AND request_id = p_request;
  IF FOUND THEN
    SELECT * INTO x FROM enrichedlab_agent.broker_library_papers
      WHERE tenant_id = p_tenant AND paper_id = prior.paper_id;
    IF prior.kind <> 'restored' OR x.group_key <> p_group OR x.source_sha256 <> p_sha256 THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict'; END IF;
    RETURN jsonb_build_object('paper', enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x),
      'created', false, 'restored', true);
  END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-library:' || p_tenant || ':' || p_group || ':' || p_sha256, 0));
  SELECT * INTO x FROM enrichedlab_agent.broker_library_papers
    WHERE tenant_id = p_tenant AND group_key = p_group AND source_sha256 = p_sha256;
  IF FOUND THEN
    IF x.document_id <> p_document THEN
      RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:library_conflict'; END IF;
    IF enrichedlab_agent.broker_library_removal(p_tenant, x.paper_id) IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_library_paper_events (tenant_id, paper_id, kind, actor_id, request_id)
        VALUES (p_tenant, x.paper_id, 'restored', p_actor, p_request);
      restored := true;
    END IF;
  ELSE
    BEGIN
      INSERT INTO enrichedlab_agent.broker_library_papers (tenant_id, group_key, document_id, source_sha256, byte_size,
          title, authors, year, doi, page_count, parse, added_by, request_id)
        VALUES (p_tenant, p_group, p_document, p_sha256, p_bytes, nullif(btrim(p_title), ''), coalesce(p_authors, '[]'::jsonb),
          p_year, nullif(btrim(p_doi), ''), p_pages, coalesce(p_parse, '{}'::jsonb), p_actor, p_request)
        RETURNING * INTO x;
    EXCEPTION WHEN check_violation OR not_null_violation THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:library_invalid';
    WHEN unique_violation THEN
      -- The same document under other bytes, which the service does not allow for one identifier.
      RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:library_conflict';
    END;
    created := true;
  END IF;
  RETURN jsonb_build_object('paper', enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x),
    'created', created, 'restored', restored);
END $$;

-- Removes a paper from the lab's library with a reason: a lab lead or whoever added it. Idempotent per request.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_remove(p_tenant text, p_actor uuid, p_group text,
  p_paper uuid, p_reason text, p_request uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r text; x enrichedlab_agent.broker_library_papers%ROWTYPE; clean text;
  prior enrichedlab_agent.broker_library_paper_events%ROWTYPE;
BEGIN
  r := enrichedlab_agent.broker_library_member(p_tenant, p_actor, p_group);
  SELECT * INTO x FROM enrichedlab_agent.broker_library_papers
    WHERE tenant_id = p_tenant AND group_key = p_group AND paper_id = p_paper;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:paper_not_found'; END IF;
  clean := nullif(btrim(coalesce(p_reason, ''), E' \t\r\n'), '');
  SELECT * INTO prior FROM enrichedlab_agent.broker_library_paper_events
    WHERE tenant_id = p_tenant AND actor_id = p_actor AND request_id = p_request;
  IF FOUND THEN
    IF prior.paper_id <> p_paper OR prior.kind <> 'removed' OR prior.reason IS DISTINCT FROM clean THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict'; END IF;
    RETURN enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x);
  END IF;
  IF NOT (r = 'lead' OR x.added_by = p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:library_forbidden'; END IF;
  IF p_request IS NULL OR clean IS NULL OR char_length(clean) > 600 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:library_invalid'; END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-library-paper:' || p_tenant || ':' || p_paper::text, 0));
  IF enrichedlab_agent.broker_library_removal(p_tenant, p_paper) IS NOT NULL THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:paper_removed'; END IF;
  INSERT INTO enrichedlab_agent.broker_library_paper_events (tenant_id, paper_id, kind, actor_id, request_id, reason)
    VALUES (p_tenant, p_paper, 'removed', p_actor, p_request, clean);
  RETURN enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x);
END $$;

-- Adds or removes a typed label on a paper in the library. Any member may add one; only a person may remove one.
-- Nothing is appended when the label already reads that way. Idempotent per request.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_label(p_tenant text, p_actor uuid, p_group text,
  p_paper uuid, p_kind text, p_namespace text, p_value text, p_request uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r text; x enrichedlab_agent.broker_library_papers%ROWTYPE; clean text; current jsonb; person boolean;
  prior enrichedlab_agent.broker_library_label_events%ROWTYPE;
BEGIN
  r := enrichedlab_agent.broker_library_member(p_tenant, p_actor, p_group);
  SELECT * INTO x FROM enrichedlab_agent.broker_library_papers
    WHERE tenant_id = p_tenant AND group_key = p_group AND paper_id = p_paper;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:paper_not_found'; END IF;
  clean := btrim(coalesce(p_value, ''), E' \t\r\n');
  SELECT * INTO prior FROM enrichedlab_agent.broker_library_label_events
    WHERE tenant_id = p_tenant AND actor_id = p_actor AND request_id = p_request;
  IF FOUND THEN
    IF prior.paper_id <> p_paper OR prior.kind <> p_kind OR prior.namespace <> p_namespace OR prior.value <> clean THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict'; END IF;
    RETURN enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x);
  END IF;
  IF p_request IS NULL OR p_kind NOT IN ('added', 'removed')
    OR p_namespace NOT IN ('topic', 'organism', 'food', 'compound', 'method', 'design', 'population')
    OR char_length(clean) NOT BETWEEN 1 AND 80 OR clean ~ '[[:cntrl:]]' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:label_invalid'; END IF;
  person := enrichedlab_agent.broker_is_person(p_tenant, p_actor);
  IF p_kind = 'removed' AND NOT person THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:label_person_only'; END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('broker-library-paper:' || p_tenant || ':' || p_paper::text, 0));
  IF enrichedlab_agent.broker_library_removal(p_tenant, p_paper) IS NOT NULL THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:paper_removed'; END IF;
  SELECT l INTO current FROM jsonb_array_elements(enrichedlab_agent.broker_library_labels(p_tenant, p_paper)) l
    WHERE l->>'namespace' = p_namespace AND l->>'value' = clean;
  IF (p_kind = 'added' AND current IS NOT NULL AND (current->>'state' = 'set' OR NOT person))
    OR (p_kind = 'removed' AND current IS NULL) THEN
    RETURN enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x);
  END IF;
  INSERT INTO enrichedlab_agent.broker_library_label_events (tenant_id, paper_id, kind, namespace, value, actor_id, request_id)
    VALUES (p_tenant, p_paper, p_kind, p_namespace, clean, p_actor, p_request);
  RETURN enrichedlab_agent.broker_library_paper_json(p_tenant, p_actor, r, x);
END $$;

-- For search: the lab's papers still in the library among these documents, with the fields a result shows.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_library_resolve(p_tenant text, p_actor uuid, p_group text,
  p_documents uuid[])
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  PERFORM enrichedlab_agent.broker_library_member(p_tenant, p_actor, p_group);
  IF cardinality(p_documents) > 100 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:library_invalid'; END IF;
  RETURN (SELECT coalesce(jsonb_agg(jsonb_build_object('document_id', x.document_id, 'paper_id', x.paper_id,
      'title', x.title, 'year', x.year, 'authors', x.authors) ORDER BY x.paper_id), '[]'::jsonb)
    FROM enrichedlab_agent.broker_library_papers x
    WHERE x.tenant_id = p_tenant AND x.group_key = p_group AND x.document_id = ANY(p_documents)
      AND enrichedlab_agent.broker_library_removal(p_tenant, x.paper_id) IS NULL);
END $$;

DO $$ DECLARE f record; BEGIN
  FOR f IN SELECT p.oid::regprocedure AS signature FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'enrichedlab_agent' AND p.proname IN ('broker_library_member', 'broker_library_labels',
      'broker_library_removal', 'broker_library_paper_json', 'broker_library', 'broker_library_paper',
      'broker_library_find', 'broker_library_record', 'broker_library_remove', 'broker_library_label',
      'broker_library_resolve')
  LOOP EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', f.signature); END LOOP;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (28) ON CONFLICT DO NOTHING;
COMMIT;
