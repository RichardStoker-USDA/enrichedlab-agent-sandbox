-- Serialize first enrollment while retaining invitation and revocation checks.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_resolve_principal(
  p_tenant text,
  p_issuer text,
  p_subject text,
  p_email text,
  p_display_name text
)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  v_principal uuid;
  v_email text := lower(p_email);
BEGIN
  -- Lock the identity before looking for its row: on first login no row exists
  -- for FOR UPDATE to lock. Separate identities still enroll independently.
  PERFORM pg_advisory_xact_lock(hashtextextended(
    json_build_array(p_tenant, p_issuer, p_subject)::text, 0));

  SELECT principal_id INTO v_principal
  FROM enrichedlab_agent.broker_principals
  WHERE tenant_id = p_tenant AND issuer = p_issuer AND subject = p_subject
  FOR UPDATE;

  IF v_principal IS NULL THEN
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
      tenant_id, issuer, subject, email, display_name
    ) VALUES (
      p_tenant, p_issuer, p_subject, v_email, nullif(p_display_name, '')
    )
    RETURNING principal_id INTO v_principal;
  ELSE
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

COMMIT;
