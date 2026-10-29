-- Content-addressed model-planner calls for research campaigns.
BEGIN;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_planner_texts (
  tenant_id text NOT NULL,
  sha256 text NOT NULL,
  content bytea NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, sha256),
  FOREIGN KEY (tenant_id) REFERENCES enrichedlab_agent.broker_tenants (tenant_id),
  CHECK (octet_length(content) BETWEEN 1 AND 1048576),
  CHECK (sha256 = 'sha256:' || encode(pg_catalog.sha256(content), 'hex'))
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_planner_calls (
  tenant_id text NOT NULL,
  campaign_id uuid NOT NULL,
  call_id uuid NOT NULL,
  sequence bigint NOT NULL,
  context_sha256 text NOT NULL,
  reply_sha256 text,
  model text,
  input_size_bytes integer NOT NULL,
  output_size_bytes integer NOT NULL,
  outcome_code text NOT NULL,
  called_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, campaign_id, call_id),
  UNIQUE (tenant_id, campaign_id, sequence),
  FOREIGN KEY (tenant_id, campaign_id)
    REFERENCES enrichedlab_agent.broker_campaigns (tenant_id, campaign_id),
  FOREIGN KEY (tenant_id, context_sha256)
    REFERENCES enrichedlab_agent.broker_campaign_planner_texts (tenant_id, sha256),
  FOREIGN KEY (tenant_id, reply_sha256)
    REFERENCES enrichedlab_agent.broker_campaign_planner_texts (tenant_id, sha256),
  CHECK (sequence > 0),
  CHECK (context_sha256 ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (reply_sha256 IS NULL OR reply_sha256 ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (model IS NULL OR (char_length(model) BETWEEN 1 AND 256 AND model !~ '[[:cntrl:]]')),
  CHECK (input_size_bytes BETWEEN 1 AND 65536),
  CHECK (output_size_bytes BETWEEN 0 AND 1048576),
  CHECK ((reply_sha256 IS NULL) = (output_size_bytes = 0)),
  CHECK ((reply_sha256 IS NULL) = (model IS NULL)),
  CHECK (outcome_code ~ '^[a-z][a-z0-9_]{1,63}$')
);

-- A start is durable before the model route is called. The terminal row above is
-- separate and may arrive after a pause or stop fenced the generation that began it.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_planner_call_starts (
  tenant_id text NOT NULL,
  campaign_id uuid NOT NULL,
  call_id uuid NOT NULL,
  sequence bigint NOT NULL,
  context_sha256 text NOT NULL,
  model text NOT NULL,
  input_size_bytes integer NOT NULL,
  lease_owner text NOT NULL,
  lease_generation bigint NOT NULL,
  started_at timestamptz DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, campaign_id, call_id),
  UNIQUE (tenant_id, campaign_id, sequence),
  FOREIGN KEY (tenant_id, campaign_id)
    REFERENCES enrichedlab_agent.broker_campaigns (tenant_id, campaign_id),
  FOREIGN KEY (tenant_id, context_sha256)
    REFERENCES enrichedlab_agent.broker_campaign_planner_texts (tenant_id, sha256),
  CHECK (sequence > 0),
  CHECK (context_sha256 ~ '^sha256:[0-9a-f]{64}$'),
  CHECK (char_length(model) BETWEEN 1 AND 256 AND model !~ '[[:cntrl:]]'),
  CHECK (input_size_bytes BETWEEN 1 AND 65536),
  CHECK (lease_owner ~ '^[a-zA-Z0-9_.-]{1,100}$'),
  CHECK (lease_generation >= 0),
  CHECK (lease_generation = 0 OR started_at IS NOT NULL)
);

-- Upgrade databases that ran the committed one-row planner draft. Its terminal row
-- carries the exact context, size, and model when known. The unprovable worker,
-- generation, and start time use the explicit legacy/zero/null markers.
-- The migration owner temporarily opens its own table inside this transaction so
-- forced tenant RLS cannot hide rows during the all-tenant schema backfill.
ALTER TABLE enrichedlab_agent.broker_campaign_planner_calls DISABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.broker_campaign_planner_call_starts DISABLE ROW LEVEL SECURITY;
INSERT INTO enrichedlab_agent.broker_campaign_planner_call_starts
  (tenant_id,campaign_id,call_id,sequence,context_sha256,model,input_size_bytes,
   lease_owner,lease_generation,started_at)
SELECT tenant_id,campaign_id,call_id,sequence,context_sha256,
       coalesce(model,'legacy_unknown'),input_size_bytes,'legacy',0,NULL
FROM enrichedlab_agent.broker_campaign_planner_calls
ON CONFLICT (tenant_id,campaign_id,call_id) DO NOTHING;

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint
      WHERE conrelid = 'enrichedlab_agent.broker_campaign_planner_calls'::regclass
        AND conname = 'broker_campaign_planner_calls_start_fkey') THEN
    ALTER TABLE enrichedlab_agent.broker_campaign_planner_calls
      ADD CONSTRAINT broker_campaign_planner_calls_start_fkey
      FOREIGN KEY (tenant_id,campaign_id,call_id)
      REFERENCES enrichedlab_agent.broker_campaign_planner_call_starts
        (tenant_id,campaign_id,call_id);
  END IF;
END;
$$;

CREATE INDEX IF NOT EXISTS broker_campaign_planner_calls_page
  ON enrichedlab_agent.broker_campaign_planner_calls
    (tenant_id, campaign_id, sequence);
CREATE INDEX IF NOT EXISTS broker_campaign_planner_call_starts_page
  ON enrichedlab_agent.broker_campaign_planner_call_starts
    (tenant_id, campaign_id, sequence);

DO $$
DECLARE table_name text;
BEGIN
  FOREACH table_name IN ARRAY ARRAY[
    'broker_campaign_planner_texts','broker_campaign_planner_calls',
    'broker_campaign_planner_call_starts'
  ] LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I ENABLE ROW LEVEL SECURITY', table_name);
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I FORCE ROW LEVEL SECURITY', table_name);
    EXECUTE format('REVOKE ALL ON enrichedlab_agent.%I FROM PUBLIC', table_name);
    EXECUTE format('DROP POLICY IF EXISTS campaign_planner_tenant ON enrichedlab_agent.%I', table_name);
    EXECUTE format('CREATE POLICY campaign_planner_tenant ON enrichedlab_agent.%I USING '
      '(tenant_id = current_setting(''enrichedlab.tenant_id'', true)) WITH CHECK '
      '(tenant_id = current_setting(''enrichedlab.tenant_id'', true))', table_name);
  END LOOP;
END;
$$;

DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_campaign_planner_texts;
CREATE TRIGGER immutable_record
  BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_campaign_planner_texts
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_campaign_planner_calls;
CREATE TRIGGER immutable_record
  BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_campaign_planner_calls
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_campaign_planner_call_starts;
CREATE TRIGGER immutable_record
  BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_campaign_planner_call_starts
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- Add the method labels and omission counts used by the bounded planner context.
-- The general API snapshot keeps a notice tail; the director replaces it with the
-- complete rule history because rule notices are an uncut planning input.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_director_view(
  p_tenant text, p_campaign uuid, p_worker text, p_generation bigint
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE result jsonb;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaigns
      WHERE tenant_id = p_tenant AND campaign_id = p_campaign
        AND state IN ('running','pausing','stopping') AND lease_owner = p_worker
        AND generation = p_generation AND lease_until > clock_timestamp()) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_lease_fence_lost';
  END IF;
  SELECT enrichedlab_agent.broker_campaign_snapshot(p_tenant, p_campaign, 100)
      || jsonb_build_object(
        'records_omitted', (SELECT greatest(count(*) - 100, 0)
          FROM enrichedlab_agent.broker_campaign_records record_row
          WHERE record_row.tenant_id = p_tenant
            AND record_row.campaign_id = p_campaign),
        'rule_notices_omitted', 0,
        'rule_notices', (SELECT coalesce(jsonb_agg(event_row.detail
                                                  ORDER BY event_row.sequence DESC), '[]'::jsonb)
          FROM enrichedlab_agent.broker_campaign_events event_row
          WHERE event_row.tenant_id = p_tenant
            AND event_row.campaign_id = p_campaign
            AND event_row.kind = 'rule_fired'),
        'methods', coalesce((
        SELECT jsonb_agg(jsonb_build_object(
            'key', profile.profile_key,
            'name', profile.name,
            'description', left(profile.description, 500),
            'data_sources', campaign_row.scope->'starting_points')
          ORDER BY scoped.ordinality)
        FROM enrichedlab_agent.broker_campaigns campaign_row
        CROSS JOIN LATERAL jsonb_array_elements_text(campaign_row.scope->'profiles')
          WITH ORDINALITY scoped(profile_key, ordinality)
        JOIN enrichedlab_agent.broker_agent_profiles profile
          ON profile.tenant_id = campaign_row.tenant_id
         AND profile.profile_key = scoped.profile_key
        WHERE campaign_row.tenant_id = p_tenant
          AND campaign_row.campaign_id = p_campaign), '[]'::jsonb))
    INTO result;
  RETURN result;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_director_view(text,uuid,text,bigint)
  FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_start_campaign_planner_call(
  p_tenant text, p_campaign uuid, p_worker text, p_generation bigint,
  p_call uuid, p_context bytea, p_model text
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE context_digest text;
DECLARE call_sequence bigint;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  PERFORM 1 FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND state = 'running'
    AND lease_owner = p_worker AND generation = p_generation
    AND lease_until > clock_timestamp()
  FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_lease_fence_lost';
  END IF;
  IF p_call IS NULL OR p_context IS NULL OR octet_length(p_context) NOT BETWEEN 1 AND 65536
     OR p_model IS NULL OR char_length(p_model) NOT BETWEEN 1 AND 256
     OR p_model ~ '[[:cntrl:]]' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  BEGIN
    PERFORM convert_from(p_context, 'UTF8');
  EXCEPTION WHEN character_not_in_repertoire THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END;
  context_digest := 'sha256:' || encode(pg_catalog.sha256(p_context), 'hex');
  INSERT INTO enrichedlab_agent.broker_campaign_planner_texts(tenant_id,sha256,content)
  VALUES (p_tenant,context_digest,p_context) ON CONFLICT (tenant_id,sha256) DO NOTHING;
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_planner_texts
      WHERE tenant_id = p_tenant AND sha256 = context_digest AND content = p_context) THEN
    RAISE EXCEPTION USING ERRCODE = '23514', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  SELECT coalesce(max(sequence),0) + 1 INTO call_sequence
  FROM enrichedlab_agent.broker_campaign_planner_call_starts
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  INSERT INTO enrichedlab_agent.broker_campaign_planner_call_starts
    (tenant_id,campaign_id,call_id,sequence,context_sha256,model,input_size_bytes,
     lease_owner,lease_generation)
  VALUES
    (p_tenant,p_campaign,p_call,call_sequence,context_digest,p_model,
     octet_length(p_context),p_worker,p_generation);
  RETURN jsonb_build_object('call_id',p_call,'sequence',call_sequence,
    'context_sha256',context_digest,'state','started');
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_start_campaign_planner_call(
  text,uuid,text,bigint,uuid,bytea,text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_finish_campaign_planner_call(
  p_tenant text, p_campaign uuid, p_worker text, p_generation bigint,
  p_call uuid, p_reply bytea, p_reported_model text, p_outcome text
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE start_row enrichedlab_agent.broker_campaign_planner_call_starts%ROWTYPE;
DECLARE reply_digest text;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO start_row
  FROM enrichedlab_agent.broker_campaign_planner_call_starts
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND call_id = p_call
    AND lease_owner = p_worker AND lease_generation = p_generation;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_lease_fence_lost';
  END IF;
  IF (p_reply IS NOT NULL AND octet_length(p_reply) NOT BETWEEN 1 AND 1048576)
     OR ((p_reply IS NULL) <> (p_reported_model IS NULL))
     OR (p_reported_model IS NOT NULL
         AND (char_length(p_reported_model) NOT BETWEEN 1 AND 256
              OR p_reported_model ~ '[[:cntrl:]]'))
     OR p_outcome IS NULL OR p_outcome !~ '^[a-z][a-z0-9_]{1,63}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  IF p_reply IS NOT NULL THEN
    BEGIN
      PERFORM convert_from(p_reply, 'UTF8');
    EXCEPTION WHEN character_not_in_repertoire THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END;
    reply_digest := 'sha256:' || encode(pg_catalog.sha256(p_reply), 'hex');
    INSERT INTO enrichedlab_agent.broker_campaign_planner_texts(tenant_id,sha256,content)
    VALUES (p_tenant,reply_digest,p_reply) ON CONFLICT (tenant_id,sha256) DO NOTHING;
    IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_planner_texts
        WHERE tenant_id = p_tenant AND sha256 = reply_digest AND content = p_reply) THEN
      RAISE EXCEPTION USING ERRCODE = '23514', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
  END IF;
  INSERT INTO enrichedlab_agent.broker_campaign_planner_calls
    (tenant_id,campaign_id,call_id,sequence,context_sha256,reply_sha256,model,
     input_size_bytes,output_size_bytes,outcome_code)
  VALUES
    (p_tenant,p_campaign,p_call,start_row.sequence,start_row.context_sha256,
     reply_digest,p_reported_model,start_row.input_size_bytes,
     coalesce(octet_length(p_reply),0),p_outcome);
  RETURN jsonb_build_object('call_id',p_call,'sequence',start_row.sequence,
    'context_sha256',start_row.context_sha256,'reply_sha256',reply_digest,
    'outcome_code',p_outcome,'state','finished');
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_finish_campaign_planner_call(
  text,uuid,text,bigint,uuid,bytea,text,text) FROM PUBLIC;

-- The committed planner draft wrote start and terminal data in one fenced call.
-- Removing it also removes any old runtime grant before the role is checked.
DROP FUNCTION IF EXISTS enrichedlab_agent.broker_record_campaign_planner_call(
  text,uuid,text,bigint,uuid,bytea,bytea,text,text);

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_planner_calls_page(
  p_tenant text, p_actor uuid, p_campaign uuid, p_after bigint, p_limit integer
)
RETURNS SETOF jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_group text;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT group_key INTO campaign_group
  FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_campaign_has_capability(
      p_tenant,p_actor,campaign_group) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  IF p_after IS NULL OR p_after < 0 OR p_limit IS NULL OR p_limit NOT BETWEEN 1 AND 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:list_limit_invalid';
  END IF;
  RETURN QUERY
  SELECT (to_jsonb(start_row) - 'tenant_id' - 'campaign_id' - 'lease_owner' - 'model')
      || jsonb_build_object(
        'configured_model', start_row.model,
        'model', terminal_row.model,
        'reply_sha256', terminal_row.reply_sha256,
        'reported_model', terminal_row.model,
        'output_size_bytes', terminal_row.output_size_bytes,
        'outcome_code', terminal_row.outcome_code,
        'called_at', terminal_row.called_at,
        'finished_at', terminal_row.called_at)
  FROM enrichedlab_agent.broker_campaign_planner_call_starts start_row
  LEFT JOIN enrichedlab_agent.broker_campaign_planner_calls terminal_row
    ON terminal_row.tenant_id = start_row.tenant_id
   AND terminal_row.campaign_id = start_row.campaign_id
   AND terminal_row.call_id = start_row.call_id
  WHERE start_row.tenant_id = p_tenant AND start_row.campaign_id = p_campaign
    AND start_row.sequence > p_after
  ORDER BY start_row.sequence
  LIMIT p_limit;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_planner_calls_page(
  text,uuid,uuid,bigint,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_planner_text(
  p_tenant text, p_actor uuid, p_campaign uuid, p_sha256 text
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_group text;
DECLARE body bytea;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT group_key INTO campaign_group
  FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_campaign_has_capability(
      p_tenant,p_actor,campaign_group) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  IF p_sha256 IS NULL OR p_sha256 !~ '^sha256:[0-9a-f]{64}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_planner_text_invalid';
  END IF;
  IF NOT EXISTS (SELECT 1
      FROM enrichedlab_agent.broker_campaign_planner_call_starts start_row
      LEFT JOIN enrichedlab_agent.broker_campaign_planner_calls terminal_row
        ON terminal_row.tenant_id = start_row.tenant_id
       AND terminal_row.campaign_id = start_row.campaign_id
       AND terminal_row.call_id = start_row.call_id
      WHERE start_row.tenant_id = p_tenant AND start_row.campaign_id = p_campaign
        AND p_sha256 IN (start_row.context_sha256,terminal_row.reply_sha256)) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  SELECT content INTO body
  FROM enrichedlab_agent.broker_campaign_planner_texts
  WHERE tenant_id = p_tenant AND sha256 = p_sha256;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  RETURN jsonb_build_object('sha256',p_sha256,'bytes',octet_length(body),
    'text',convert_from(body,'UTF8'));
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_planner_text(text,uuid,uuid,text)
  FROM PUBLIC;

-- A director stays function-only when the planner's call writers are added. Keep this list
-- in lockstep with provisioning and startup readiness.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_director_role_safe(p_role name)
RETURNS boolean
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE role_oid oid;
DECLARE allowed_functions oid[];
BEGIN
  SELECT role.oid INTO role_oid FROM pg_catalog.pg_roles role WHERE role.rolname = p_role;
  IF NOT FOUND THEN RETURN false; END IF;
  SELECT array_agg(to_regprocedure(entry.signature)::oid ORDER BY entry.signature)
    INTO allowed_functions
  FROM unnest(ARRAY[
    'enrichedlab_agent.broker_schema_ready(integer)',
    'enrichedlab_agent.broker_campaign_director_ready()',
    'enrichedlab_agent.broker_claim_campaign(text,text,integer)',
    'enrichedlab_agent.broker_renew_campaign(text,uuid,text,bigint,integer)',
    'enrichedlab_agent.broker_yield_campaign(text,uuid,text,bigint)',
    'enrichedlab_agent.broker_campaign_director_view(text,uuid,text,bigint)',
    'enrichedlab_agent.broker_campaign_finished_steps(text,uuid,text,bigint)',
    'enrichedlab_agent.broker_campaign_apply(text,uuid,text,bigint,jsonb)',
    'enrichedlab_agent.broker_start_campaign_step(text,uuid,text,bigint,uuid,uuid,text,text,text[],uuid,uuid)',
    'enrichedlab_agent.broker_start_campaign_planner_call(text,uuid,text,bigint,uuid,bytea,text)',
    'enrichedlab_agent.broker_finish_campaign_planner_call(text,uuid,text,bigint,uuid,bytea,text,text)'
  ]::text[]) entry(signature);
  IF cardinality(allowed_functions) <> 11 OR array_position(allowed_functions, NULL) IS NOT NULL OR EXISTS (
    SELECT 1 FROM pg_catalog.pg_roles role
    WHERE role.oid = role_oid AND (role.rolsuper OR role.rolcreatedb OR role.rolcreaterole
      OR role.rolreplication OR role.rolbypassrls)
  ) THEN RETURN false; END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership
      WHERE membership.roleid = role_oid OR membership.member = role_oid) THEN
    RETURN false;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_shdepend dependency
      WHERE dependency.refclassid = 'pg_catalog.pg_authid'::regclass
        AND dependency.refobjid = role_oid AND dependency.deptype = 'o') THEN
    RETURN false;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_class relation
      CROSS JOIN LATERAL aclexplode(relation.relacl) privilege
      WHERE privilege.grantee = role_oid)
     OR EXISTS (SELECT 1 FROM pg_catalog.pg_attribute attribute
      CROSS JOIN LATERAL aclexplode(attribute.attacl) privilege
      WHERE privilege.grantee = role_oid) THEN
    RETURN false;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_namespace namespace
      CROSS JOIN LATERAL aclexplode(namespace.nspacl) privilege
      WHERE privilege.grantee = role_oid
        AND (namespace.nspname <> 'enrichedlab_agent' OR privilege.privilege_type <> 'USAGE'
          OR privilege.is_grantable)) THEN
    RETURN false;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_proc function_row
      CROSS JOIN LATERAL aclexplode(function_row.proacl) privilege
      WHERE privilege.grantee = role_oid
        AND (NOT (function_row.oid = ANY(allowed_functions))
          OR privilege.privilege_type <> 'EXECUTE' OR privilege.is_grantable)) THEN
    RETURN false;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_proc function_row
      JOIN pg_catalog.pg_namespace namespace ON namespace.oid = function_row.pronamespace
      WHERE namespace.nspname = 'enrichedlab_agent'
        AND pg_catalog.has_function_privilege(p_role::text, function_row.oid, 'EXECUTE')
        AND NOT (function_row.oid = ANY(allowed_functions))) THEN
    RETURN false;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_class relation
      JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace
      WHERE namespace.nspname = 'enrichedlab_agent' AND relation.relkind IN ('r','p','v','m','f')
        AND (pg_catalog.has_table_privilege(p_role::text, relation.oid, 'SELECT')
          OR pg_catalog.has_table_privilege(p_role::text, relation.oid, 'INSERT')
          OR pg_catalog.has_table_privilege(p_role::text, relation.oid, 'UPDATE')
          OR pg_catalog.has_table_privilege(p_role::text, relation.oid, 'DELETE')
          OR pg_catalog.has_table_privilege(p_role::text, relation.oid, 'TRUNCATE')
          OR pg_catalog.has_table_privilege(p_role::text, relation.oid, 'REFERENCES')
          OR pg_catalog.has_table_privilege(p_role::text, relation.oid, 'TRIGGER')
          OR pg_catalog.has_any_column_privilege(p_role::text, relation.oid, 'SELECT')
          OR pg_catalog.has_any_column_privilege(p_role::text, relation.oid, 'INSERT')
          OR pg_catalog.has_any_column_privilege(p_role::text, relation.oid, 'UPDATE')
          OR pg_catalog.has_any_column_privilege(p_role::text, relation.oid, 'REFERENCES'))) THEN
    RETURN false;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_class sequence
      JOIN pg_catalog.pg_namespace namespace ON namespace.oid = sequence.relnamespace
      WHERE namespace.nspname = 'enrichedlab_agent' AND sequence.relkind = 'S'
        AND (pg_catalog.has_sequence_privilege(p_role::text, sequence.oid, 'USAGE')
          OR pg_catalog.has_sequence_privilege(p_role::text, sequence.oid, 'SELECT')
          OR pg_catalog.has_sequence_privilege(p_role::text, sequence.oid, 'UPDATE'))) THEN
    RETURN false;
  END IF;
  RETURN true;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_director_role_safe(name) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = 2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (67) ON CONFLICT DO NOTHING;
COMMIT;
