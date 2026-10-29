-- A principal's class is positive and comes from the authenticator that verified it.
-- Person-only rules read the class, so an identity from an unknown authenticator is refused instead of
-- being counted as a person because its issuer is not the ServiceAccount issuer.
BEGIN;

ALTER TABLE enrichedlab_agent.broker_principals ADD COLUMN IF NOT EXISTS principal_class text;
-- Every row before this migration was created by one of the two authenticators the broker has had: the
-- TokenReview mapping (issuer kubernetes://serviceaccount) or the Access assertion that identifies a person.
UPDATE enrichedlab_agent.broker_principals
SET principal_class = CASE WHEN issuer = 'kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END
WHERE principal_class IS NULL;
ALTER TABLE enrichedlab_agent.broker_principals ALTER COLUMN principal_class SET NOT NULL;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'broker_principals_class_known'
      AND conrelid = 'enrichedlab_agent.broker_principals'::regclass) THEN
    ALTER TABLE enrichedlab_agent.broker_principals ADD CONSTRAINT broker_principals_class_known
      CHECK (principal_class IN ('person', 'service_account'));
  END IF;
END $$;

-- The authenticator names the class. A new identity without one is refused; an existing identity keeps its
-- class for life. The invitation, membership and consumption rules are unchanged from migration 012.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_resolve_principal(
  p_tenant text,
  p_issuer text,
  p_subject text,
  p_email text,
  p_display_name text,
  p_class text
)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_principal uuid;
  v_class text;
  v_email text := lower(p_email);
BEGIN
  IF p_class IS NOT NULL AND p_class NOT IN ('person', 'service_account') THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:principal_class_unknown';
  END IF;
  -- Lock the identity before looking for its row: on first login no row exists
  -- for FOR UPDATE to lock. Separate identities still enroll independently.
  PERFORM pg_advisory_xact_lock(hashtextextended(
    json_build_array(p_tenant, p_issuer, p_subject)::text, 0));

  SELECT principal_id, principal_class INTO v_principal, v_class
  FROM enrichedlab_agent.broker_principals
  WHERE tenant_id = p_tenant AND issuer = p_issuer AND subject = p_subject
  FOR UPDATE;

  IF v_principal IS NULL THEN
    IF p_class IS NULL THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:principal_class_required';
    END IF;
    IF NOT EXISTS (
      SELECT 1
      FROM enrichedlab_agent.broker_principal_invitations
      WHERE tenant_id = p_tenant
        AND email = v_email
        AND enabled
        AND consumed_at IS NULL
    ) THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:not_invited';
    END IF;
    INSERT INTO enrichedlab_agent.broker_principals (
      tenant_id, issuer, subject, email, display_name, principal_class
    ) VALUES (
      p_tenant, p_issuer, p_subject, v_email, nullif(p_display_name, ''), p_class
    )
    RETURNING principal_id INTO v_principal;
  ELSE
    IF p_class IS NOT NULL AND p_class <> v_class THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:principal_class_mismatch';
    END IF;
    IF NOT EXISTS (
      SELECT 1
      FROM enrichedlab_agent.broker_principal_invitations
      WHERE tenant_id = p_tenant
        AND email = v_email
        AND enabled
    ) THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:not_invited';
    END IF;
    UPDATE enrichedlab_agent.broker_principals
    SET email = v_email,
        display_name = nullif(p_display_name, ''),
        last_seen_at = clock_timestamp(),
        updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND principal_id = v_principal AND enabled;
    IF NOT FOUND THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:principal_disabled';
    END IF;
  END IF;

  DELETE FROM enrichedlab_agent.broker_group_memberships AS memberships
  WHERE memberships.tenant_id = p_tenant
    AND memberships.principal_id = v_principal
    AND NOT EXISTS (
      SELECT 1
      FROM enrichedlab_agent.broker_principal_invitations AS invitations
      WHERE invitations.tenant_id = memberships.tenant_id
        AND invitations.group_key = memberships.group_key
        AND invitations.email = v_email
        AND invitations.enabled
    );

  INSERT INTO enrichedlab_agent.broker_group_memberships (
    tenant_id, group_key, principal_id, role
  )
  SELECT tenant_id, group_key, v_principal, role
  FROM enrichedlab_agent.broker_principal_invitations
  WHERE tenant_id = p_tenant
    AND email = v_email
    AND enabled
  ON CONFLICT (tenant_id, group_key, principal_id)
  DO UPDATE SET role = EXCLUDED.role, updated_at = clock_timestamp();

  UPDATE enrichedlab_agent.broker_principal_invitations
  SET consumed_at = clock_timestamp(), consumed_by = v_principal, updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant
    AND email = v_email
    AND enabled
    AND consumed_at IS NULL;

  RETURN v_principal;
END;
$$;

-- The earlier signature resolves existing identities only, so an API from before this migration keeps
-- working during a rollout but can never create a principal without a class.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_resolve_principal(
  p_tenant text,
  p_issuer text,
  p_subject text,
  p_email text,
  p_display_name text
)
RETURNS uuid
LANGUAGE sql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT enrichedlab_agent.broker_resolve_principal(p_tenant, p_issuer, p_subject, p_email, p_display_name, NULL)
$$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_resolve_principal(text,text,text,text,text,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_resolve_principal(text,text,text,text,text) FROM PUBLIC;

-- Every person rule moves from "issuer is not the ServiceAccount issuer" to "class is person", and every
-- service-account rule to "class is not person". The rewrite is exact: each function that still names the
-- ServiceAccount issuer must match one of the two forms, and the set rewritten must be the set reviewed.
-- A second application finds nothing left to rewrite.
DO $$
DECLARE
  f record;
  src text;
  rewritten text;
  changed text[] := '{}';
  expected text[] := ARRAY[
    'broker_admin_project_directory(p_tenant text, p_actor uuid, p_limit integer)',
    'broker_inspect_project(p_tenant text, p_actor uuid, p_id uuid, p_reason text)',
    'broker_is_person(p_tenant text, p_actor uuid)',
    'broker_is_project_administrator(p_tenant text, p_actor uuid)',
    'broker_lab_curator(p_tenant text, p_actor uuid, p_group text)',
    'broker_lab_note_json(p_tenant text, n enrichedlab_agent.broker_lab_notes)',
    'broker_lab_note_state(p_tenant text, n enrichedlab_agent.broker_lab_notes)',
    'broker_library_labels(p_tenant text, p_paper uuid)',
    'broker_library_paper_json(p_tenant text, p_actor uuid, p_role text, x enrichedlab_agent.broker_library_papers)',
    'broker_list_projects(p_tenant text, p_actor uuid, p_limit integer)',
    'broker_project_members(p_tenant text, p_actor uuid, p_project uuid)',
    'broker_record_task_review(p_tenant text, p_actor uuid, p_task uuid, p_decision text, p_note text, p_request uuid)',
    'broker_research_run_results(p_tenant text, p_run uuid, p_worker text, p_generation bigint)',
    'broker_share_lab_note(p_tenant text, p_actor uuid, p_project uuid, p_revision bigint, p_statement text, p_evidence_refs jsonb, p_reason text, p_review_days integer, p_request uuid)',
    'broker_task_lineage(p_tenant text, p_actor uuid, p_task uuid)',
    'broker_task_review_json(p_tenant text, r enrichedlab_agent.broker_task_reviews)'];
BEGIN
  FOR f IN
    SELECT p.oid, p.proname, pg_get_function_identity_arguments(p.oid) AS args
    FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'enrichedlab_agent' AND p.prosrc LIKE '%kubernetes://serviceaccount%'
    ORDER BY p.proname, 3
  LOOP
    src := pg_get_functiondef(f.oid);
    rewritten := regexp_replace(src, '\m([a-z_][a-z0-9_]*)\.issuer\s*<>\s*''kubernetes://serviceaccount''',
      '\1.principal_class = ''person''', 'g');
    rewritten := regexp_replace(rewritten, '\m([a-z_][a-z0-9_]*)\.issuer\s*=\s*''kubernetes://serviceaccount''',
      '\1.principal_class <> ''person''', 'g');
    IF rewritten = src OR strpos(rewritten, 'kubernetes://serviceaccount') > 0 THEN
      RAISE EXCEPTION 'principal class rewrite is incomplete for %(%)', f.proname, f.args;
    END IF;
    EXECUTE rewritten;
    changed := changed || (f.proname || '(' || f.args || ')');
  END LOOP;
  IF cardinality(changed) > 0 AND
     (SELECT array_agg(x ORDER BY x) FROM unnest(changed) x) IS DISTINCT FROM
     (SELECT array_agg(x ORDER BY x) FROM unnest(expected) x) THEN
    RAISE EXCEPTION 'principal class rewrite touched an unreviewed set: %', changed;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname = 'enrichedlab_agent' AND p.prosrc LIKE '%kubernetes://serviceaccount%') THEN
    RAISE EXCEPTION 'a function still classifies principals by issuer';
  END IF;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (30) ON CONFLICT DO NOTHING;
COMMIT;
