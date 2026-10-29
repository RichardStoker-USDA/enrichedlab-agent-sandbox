\set ON_ERROR_STOP on

SET ROLE broker_api_qual;

DO $$
BEGIN
  BEGIN
    PERFORM enrichedlab_agent.broker_resolve_principal(
      'research-site',
      'https://example.cloudflareaccess.com',
      'example-subject-b',
      'reviewer@example.invalid',
      'Example Reviewer',
      'person'
    );
    RAISE EXCEPTION 'removed invitation still authenticated';
  EXCEPTION WHEN insufficient_privilege THEN
    IF SQLERRM <> 'broker:not_invited' AND SQLERRM <> 'broker:principal_disabled' THEN
      RAISE;
    END IF;
  END;
END;
$$;

SELECT enrichedlab_agent.broker_resolve_principal(
  'research-site',
  'https://example.cloudflareaccess.com',
  'example-subject-a',
  'researcher@example.invalid',
  'Example Researcher',
  'person'
) AS retained_principal \gset
SELECT set_config('qual.retained_principal', :'retained_principal', false);

DO $$
DECLARE
  v_groups integer;
BEGIN
  SELECT count(*) INTO v_groups
  FROM enrichedlab_agent.broker_get_groups(
    'research-site', current_setting('qual.retained_principal')::uuid
  );
  IF v_groups <> 1 THEN
    RAISE EXCEPTION 'retained invitation lost its membership';
  END IF;
END;
$$;

RESET ROLE;
