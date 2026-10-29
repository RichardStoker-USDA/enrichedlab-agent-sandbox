-- Durable autonomous research campaigns. A campaign may plan and start only ordinary
-- research runs. It owns no cluster credential and every database writer below rechecks its tenant, lease,
-- lab methods, scope and budget.
BEGIN;

-- Campaign principals are records, not authenticators. The positive class keeps every person-only rule closed.
ALTER TABLE enrichedlab_agent.broker_principals
  DROP CONSTRAINT IF EXISTS broker_principals_class_known;
ALTER TABLE enrichedlab_agent.broker_principals
  ADD CONSTRAINT broker_principals_class_known
  CHECK (principal_class IN ('person', 'service_account', 'campaign'));

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_principal_invariant()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF NEW.issuer = 'urn:enrichedlab:campaign'
     OR NEW.principal_class = 'campaign'
     OR lower(NEW.email) LIKE '%@campaign.invalid' THEN
    IF NEW.issuer <> 'urn:enrichedlab:campaign'
       OR NEW.principal_class <> 'campaign'
       OR NEW.subject !~ '^campaign:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR NEW.email !~ '^campaign\+[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}@campaign\.invalid$' THEN
      RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:campaign_authentication_denied';
    END IF;
  END IF;
  RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_principal_invariant() FROM PUBLIC;
DROP TRIGGER IF EXISTS broker_campaign_principal_invariant ON enrichedlab_agent.broker_principals;
CREATE TRIGGER broker_campaign_principal_invariant
  BEFORE INSERT OR UPDATE ON enrichedlab_agent.broker_principals
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_campaign_principal_invariant();

DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'broker_invitations_no_campaign'
      AND conrelid = 'enrichedlab_agent.broker_principal_invitations'::regclass) THEN
    ALTER TABLE enrichedlab_agent.broker_principal_invitations
      ADD CONSTRAINT broker_invitations_no_campaign
      CHECK (lower(email) NOT LIKE '%@campaign.invalid');
  END IF;
END $$;

-- Site-owned catalog grants. Revocation is retained instead of deleting the row.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_capability_grants (
  tenant_id text NOT NULL,
  email text NOT NULL,
  group_key text NOT NULL,
  capability text NOT NULL,
  enabled boolean NOT NULL DEFAULT true,
  revoked_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, email, group_key, capability),
  FOREIGN KEY (tenant_id, group_key)
    REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  CHECK (email = lower(email) AND length(email) BETWEEN 3 AND 320),
  CHECK (email NOT LIKE '%@campaign.invalid'),
  CHECK (capability = 'research.discovery.autonomous'),
  CHECK (enabled = (revoked_at IS NULL))
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaigns (
  tenant_id text NOT NULL,
  campaign_id uuid NOT NULL,
  group_key text NOT NULL,
  started_by uuid NOT NULL,
  campaign_principal uuid NOT NULL,
  goal text NOT NULL,
  constraints text NOT NULL DEFAULT '',
  text_origin text NOT NULL DEFAULT 'person',
  scope jsonb NOT NULL,
  budgets jsonb NOT NULL,
  stop_rules jsonb NOT NULL,
  state text NOT NULL DEFAULT 'planned',
  state_reason text,
  generation bigint NOT NULL DEFAULT 0,
  lease_owner text,
  lease_until timestamptz,
  available_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  run_count integer NOT NULL DEFAULT 0,
  active_runs integer NOT NULL DEFAULT 0,
  finished_steps integer NOT NULL DEFAULT 0,
  evidence_steps integer NOT NULL DEFAULT 0,
  consecutive_without_evidence integer NOT NULL DEFAULT 0,
  planner_refused_cycles integer NOT NULL DEFAULT 0,
  event_sequence bigint NOT NULL DEFAULT 0,
  record_sequence bigint NOT NULL DEFAULT 0,
  started_at timestamptz,
  finished_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, campaign_id),
  UNIQUE (tenant_id, campaign_principal),
  FOREIGN KEY (tenant_id, group_key)
    REFERENCES enrichedlab_agent.broker_research_groups (tenant_id, group_key),
  FOREIGN KEY (tenant_id, started_by)
    REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  FOREIGN KEY (tenant_id, campaign_principal)
    REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (char_length(btrim(goal)) BETWEEN 1 AND 4000 AND goal !~ '[[:cntrl:]]'),
  CHECK (char_length(constraints) <= 4000 AND constraints !~ '[[:cntrl:]]'),
  CHECK (text_origin = 'person'),
  CHECK (jsonb_typeof(scope) = 'object' AND octet_length(scope::text) <= 65536),
  CHECK (jsonb_typeof(budgets) = 'object' AND octet_length(budgets::text) <= 1024),
  CHECK (jsonb_typeof(stop_rules) = 'object' AND octet_length(stop_rules::text) <= 1024),
  CHECK (state IN ('planned','running','pausing','paused','stopping','stopped','finished','failed')),
  CHECK (state_reason IS NULL OR state_reason ~ '^[a-z][a-z0-9_]{1,63}$'),
  CHECK ((lease_owner IS NULL) = (lease_until IS NULL)),
  CHECK (lease_owner IS NULL OR length(lease_owner) BETWEEN 1 AND 100),
  CHECK (run_count BETWEEN 0 AND 500 AND active_runs BETWEEN 0 AND 4
    AND active_runs <= run_count),
  CHECK (finished_steps BETWEEN 0 AND run_count AND evidence_steps BETWEEN 0 AND finished_steps),
  CHECK (consecutive_without_evidence BETWEEN 0 AND finished_steps - evidence_steps
    AND planner_refused_cycles >= 0),
  CHECK (event_sequence >= 0 AND record_sequence >= 0),
  CHECK (started_at IS NOT NULL OR state IN ('planned','stopping','stopped')),
  CHECK (finished_at IS NULL OR state IN ('stopped','finished','failed'))
);

CREATE INDEX IF NOT EXISTS broker_campaigns_claim
  ON enrichedlab_agent.broker_campaigns (tenant_id, state, available_at, created_at)
  WHERE state IN ('running','pausing','stopping');
CREATE INDEX IF NOT EXISTS broker_campaigns_lab
  ON enrichedlab_agent.broker_campaigns (tenant_id, group_key, updated_at DESC, campaign_id);

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_membership_invariant()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_principals p
      WHERE p.tenant_id = NEW.tenant_id AND p.principal_id = NEW.principal_id
        AND p.principal_class = 'campaign')
     AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaigns c
      WHERE c.tenant_id = NEW.tenant_id AND c.campaign_principal = NEW.principal_id
        AND c.group_key = NEW.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:campaign_scope_forbidden';
  END IF;
  RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_membership_invariant() FROM PUBLIC;
DROP TRIGGER IF EXISTS broker_campaign_membership_invariant
  ON enrichedlab_agent.broker_group_memberships;
CREATE TRIGGER broker_campaign_membership_invariant
  BEFORE INSERT OR UPDATE ON enrichedlab_agent.broker_group_memberships
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_campaign_membership_invariant();

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_branches (
  tenant_id text NOT NULL,
  campaign_id uuid NOT NULL,
  branch_id uuid NOT NULL,
  parent_id uuid,
  direction text NOT NULL,
  rationale text NOT NULL,
  text_origin text NOT NULL DEFAULT 'planner',
  state text NOT NULL DEFAULT 'open',
  reason_class text,
  reason text,
  step_count integer NOT NULL DEFAULT 0,
  evidence_count integer NOT NULL DEFAULT 0,
  no_answer_count integer NOT NULL DEFAULT 0,
  needs_person_count integer NOT NULL DEFAULT 0,
  stalled_count integer NOT NULL DEFAULT 0,
  failed_count integer NOT NULL DEFAULT 0,
  cancelled_count integer NOT NULL DEFAULT 0,
  consecutive_without_evidence integer NOT NULL DEFAULT 0,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, campaign_id, branch_id),
  FOREIGN KEY (tenant_id, campaign_id)
    REFERENCES enrichedlab_agent.broker_campaigns (tenant_id, campaign_id),
  FOREIGN KEY (tenant_id, campaign_id, parent_id)
    REFERENCES enrichedlab_agent.broker_campaign_branches (tenant_id, campaign_id, branch_id),
  CHECK (char_length(btrim(direction)) BETWEEN 1 AND 2000 AND direction !~ '[[:cntrl:]]'),
  CHECK (char_length(btrim(rationale)) BETWEEN 1 AND 2000 AND rationale !~ '[[:cntrl:]]'),
  CHECK (text_origin = 'planner'),
  CHECK (state IN ('open','waiting','abandoned','concluded')),
  CHECK (reason_class IS NULL OR reason_class ~ '^[a-z][a-z0-9_]{1,63}$'),
  CHECK (reason IS NULL OR (char_length(btrim(reason)) BETWEEN 1 AND 512 AND reason !~ '[[:cntrl:]]')),
  CHECK ((state = 'open' AND reason_class IS NULL AND reason IS NULL)
    OR (state <> 'open' AND reason_class IS NOT NULL AND reason IS NOT NULL)),
  CHECK (step_count >= 0 AND evidence_count >= 0 AND no_answer_count >= 0
    AND needs_person_count >= 0 AND stalled_count >= 0 AND failed_count >= 0
    AND cancelled_count >= 0 AND consecutive_without_evidence >= 0),
  CHECK (evidence_count + no_answer_count + needs_person_count + stalled_count
      + failed_count + cancelled_count <= step_count),
  CHECK (consecutive_without_evidence <= no_answer_count + needs_person_count
      + stalled_count + failed_count + cancelled_count)
);

CREATE INDEX IF NOT EXISTS broker_campaign_branches_parent
  ON enrichedlab_agent.broker_campaign_branches (tenant_id, campaign_id, parent_id, created_at);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_steps (
  tenant_id text NOT NULL,
  campaign_id uuid NOT NULL,
  step_id uuid NOT NULL,
  branch_id uuid NOT NULL,
  purpose text NOT NULL,
  question text NOT NULL,
  text_origin text NOT NULL DEFAULT 'planner',
  normalized_question text NOT NULL,
  profiles text[] NOT NULL,
  project_id uuid,
  run_id uuid,
  checkpoint_revision bigint,
  state text NOT NULL DEFAULT 'planned',
  outcome_class text,
  outcome jsonb,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  started_at timestamptz,
  finished_at timestamptz,
  PRIMARY KEY (tenant_id, campaign_id, step_id),
  UNIQUE (tenant_id, campaign_id, normalized_question),
  UNIQUE (tenant_id, project_id),
  UNIQUE (tenant_id, run_id),
  FOREIGN KEY (tenant_id, campaign_id, branch_id)
    REFERENCES enrichedlab_agent.broker_campaign_branches (tenant_id, campaign_id, branch_id),
  FOREIGN KEY (tenant_id, project_id)
    REFERENCES enrichedlab_agent.broker_projects (tenant_id, project_id),
  FOREIGN KEY (tenant_id, run_id)
    REFERENCES enrichedlab_agent.broker_research_runs (tenant_id, run_id),
  CONSTRAINT broker_campaign_steps_checkpoint_revision_fkey
    FOREIGN KEY (tenant_id, project_id, checkpoint_revision)
    REFERENCES enrichedlab_agent.broker_project_checkpoints (tenant_id, project_id, revision),
  CHECK (purpose IN ('explore','test','replicate','verify','literature','synthesize')),
  CHECK (char_length(btrim(question)) BETWEEN 1 AND 2000 AND question !~ '[[:cntrl:]]'),
  CHECK (text_origin = 'planner'),
  CHECK (char_length(normalized_question) BETWEEN 1 AND 2000),
  CHECK (cardinality(profiles) BETWEEN 1 AND 16),
  CHECK (state IN ('planned','running','finished')),
  CHECK (outcome_class IS NULL OR outcome_class IN
    ('evidence','no_answer','needs_person','stalled','failed','cancelled')),
  CHECK (outcome IS NULL OR (jsonb_typeof(outcome) = 'object' AND octet_length(outcome::text) <= 2048)),
  CONSTRAINT broker_campaign_steps_state_shape CHECK (
    (state = 'planned' AND project_id IS NULL AND run_id IS NULL
           AND checkpoint_revision IS NULL AND started_at IS NULL
           AND outcome_class IS NULL AND outcome IS NULL AND finished_at IS NULL)
    OR (state = 'running' AND project_id IS NOT NULL AND run_id IS NOT NULL
           AND checkpoint_revision IS NOT NULL AND started_at IS NOT NULL
           AND outcome_class IS NULL AND outcome IS NULL AND finished_at IS NULL)
    OR (state = 'finished' AND project_id IS NOT NULL AND run_id IS NOT NULL
           AND checkpoint_revision IS NOT NULL AND started_at IS NOT NULL
           AND outcome_class IS NOT NULL AND outcome IS NOT NULL AND finished_at IS NOT NULL))
);

-- Upgrade a database that ran the first committed 066 draft. That draft had no
-- step pin and gave the starter project-level steer authority. A pin is accepted
-- only when the run's append-only records and every checkpoint through it prove
-- the campaign principal and immutable scope. An active step whose current
-- project revision is not provable is fenced instead of guessing: its run is
-- paused, its campaign stops, and a provable campaign revision is retained.
-- Lock the source and writer relations so a checkpoint or run transition cannot
-- cross the proof-and-backfill window before the new guard and reduced grant commit.
LOCK TABLE enrichedlab_agent.broker_campaigns IN SHARE ROW EXCLUSIVE MODE;
LOCK TABLE enrichedlab_agent.broker_research_runs IN SHARE ROW EXCLUSIVE MODE;
LOCK TABLE enrichedlab_agent.broker_projects IN EXCLUSIVE MODE;
LOCK TABLE enrichedlab_agent.broker_project_grants IN SHARE ROW EXCLUSIVE MODE;
LOCK TABLE enrichedlab_agent.broker_project_checkpoints IN SHARE ROW EXCLUSIVE MODE;
ALTER TABLE enrichedlab_agent.broker_campaign_steps
  ADD COLUMN IF NOT EXISTS checkpoint_revision bigint;

CREATE TEMPORARY TABLE broker_campaign_step_pin_upgrade ON COMMIT DROP AS
WITH candidates AS (
  SELECT step_row.tenant_id, step_row.campaign_id, step_row.step_id,
    step_row.project_id, step_row.run_id, step_row.state AS step_state, step_row.question,
    run_row.initial_revision, run_row.actor_id AS run_actor_id,
    run_row.project_id AS run_project_id, accepted.candidate_revision
  FROM enrichedlab_agent.broker_campaign_steps step_row
  JOIN enrichedlab_agent.broker_research_runs run_row
    ON run_row.tenant_id = step_row.tenant_id AND run_row.run_id = step_row.run_id
  JOIN LATERAL (
    SELECT max(revision) AS candidate_revision
    FROM (
      SELECT run_row.initial_revision AS revision
      UNION ALL
      SELECT turn_row.checkpoint_revision
      FROM enrichedlab_agent.broker_research_turns turn_row
      WHERE turn_row.tenant_id = step_row.tenant_id AND turn_row.run_id = step_row.run_id
      UNION ALL
      SELECT plan_row.revision
      FROM enrichedlab_agent.broker_plan_revisions plan_row
      WHERE plan_row.tenant_id = step_row.tenant_id AND plan_row.project_id = step_row.project_id
        AND plan_row.run_id = step_row.run_id
    ) accepted_revision
  ) accepted ON true
  WHERE step_row.state <> 'planned' AND step_row.checkpoint_revision IS NULL
), snapshots AS (
  SELECT candidate.*, campaign_row.campaign_principal, campaign_row.constraints,
    campaign_row.scope, project_row.owner_id, project_row.revision AS project_revision,
    initial_checkpoint.author_id AS initial_author_id,
    initial_checkpoint.request_id AS initial_request_id,
    initial_checkpoint.state AS initial_state,
    candidate_checkpoint.author_id AS candidate_author_id,
    candidate_checkpoint.state AS candidate_state,
    expected_constraints.value AS expected_constraints
  FROM candidates candidate
  JOIN enrichedlab_agent.broker_campaigns campaign_row
    ON campaign_row.tenant_id = candidate.tenant_id
   AND campaign_row.campaign_id = candidate.campaign_id
  JOIN enrichedlab_agent.broker_projects project_row
    ON project_row.tenant_id = candidate.tenant_id
   AND project_row.project_id = candidate.project_id
  LEFT JOIN enrichedlab_agent.broker_project_checkpoints initial_checkpoint
    ON initial_checkpoint.tenant_id = candidate.tenant_id
   AND initial_checkpoint.project_id = candidate.project_id
   AND initial_checkpoint.revision = candidate.initial_revision
  LEFT JOIN enrichedlab_agent.broker_project_checkpoints candidate_checkpoint
    ON candidate_checkpoint.tenant_id = candidate.tenant_id
   AND candidate_checkpoint.project_id = candidate.project_id
   AND candidate_checkpoint.revision = candidate.candidate_revision
  JOIN LATERAL (
    SELECT coalesce(jsonb_agg(substring(campaign_row.constraints FROM offset_value FOR 256)
      ORDER BY offset_value), '[]'::jsonb) AS value
    FROM generate_series(1, char_length(campaign_row.constraints), 256) offset_value
    WHERE char_length(campaign_row.constraints) > 0
  ) expected_constraints ON true
), initial_checked AS (
  SELECT snapshot.*,
    snapshot.initial_author_id = snapshot.campaign_principal
      AND snapshot.initial_request_id = snapshot.step_id
      AND snapshot.run_actor_id = snapshot.campaign_principal
      AND snapshot.run_project_id = snapshot.project_id
      AND snapshot.owner_id = snapshot.campaign_principal
      AND snapshot.initial_state->>'goal' = snapshot.question
      AND snapshot.initial_state->'constraints' IS NOT DISTINCT FROM snapshot.expected_constraints
      AND snapshot.initial_state->'workspace' IS NOT DISTINCT FROM snapshot.scope->'workspace'
      AS initial_safe
  FROM snapshots snapshot
), checked AS (
  SELECT initial_checked.*,
    initial_checked.initial_safe
      AND initial_checked.candidate_revision >= initial_checked.initial_revision
      AND initial_checked.candidate_revision <= initial_checked.project_revision
      AND initial_checked.candidate_author_id = initial_checked.campaign_principal
      AND initial_checked.candidate_state->'goal' IS NOT DISTINCT FROM initial_checked.initial_state->'goal'
      AND initial_checked.candidate_state->'constraints' IS NOT DISTINCT FROM initial_checked.initial_state->'constraints'
      AND initial_checked.candidate_state->'workspace' IS NOT DISTINCT FROM initial_checked.initial_state->'workspace'
      AND (SELECT count(*)
           FROM enrichedlab_agent.broker_project_checkpoints history
           WHERE history.tenant_id = initial_checked.tenant_id
             AND history.project_id = initial_checked.project_id
             AND history.revision BETWEEN initial_checked.initial_revision
                                      AND initial_checked.candidate_revision)
          = initial_checked.candidate_revision - initial_checked.initial_revision + 1
      AND NOT EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_project_checkpoints history
        WHERE history.tenant_id = initial_checked.tenant_id
          AND history.project_id = initial_checked.project_id
          AND history.revision BETWEEN initial_checked.initial_revision
                                   AND initial_checked.candidate_revision
          AND (history.author_id IS DISTINCT FROM initial_checked.campaign_principal
            OR history.state->'goal' IS DISTINCT FROM initial_checked.initial_state->'goal'
            OR history.state->'constraints' IS DISTINCT FROM initial_checked.initial_state->'constraints'
            OR history.state->'workspace' IS DISTINCT FROM initial_checked.initial_state->'workspace'))
      AS candidate_safe
  FROM initial_checked
)
SELECT checked.*,
  checked.candidate_safe AND checked.candidate_revision = checked.project_revision AS active_safe
FROM checked;

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_temp.broker_campaign_step_pin_upgrade
             WHERE initial_safe IS NOT TRUE) THEN
    RAISE EXCEPTION USING ERRCODE = '55000',
      MESSAGE = 'broker:campaign_checkpoint_upgrade_unsafe';
  END IF;
END;
$$;

UPDATE enrichedlab_agent.broker_campaign_steps step_row
SET checkpoint_revision = CASE WHEN upgrade.candidate_safe IS TRUE
                               THEN upgrade.candidate_revision
                               ELSE upgrade.initial_revision END
FROM pg_temp.broker_campaign_step_pin_upgrade upgrade
WHERE step_row.tenant_id = upgrade.tenant_id
  AND step_row.campaign_id = upgrade.campaign_id
  AND step_row.step_id = upgrade.step_id
  AND step_row.checkpoint_revision IS NULL;

UPDATE enrichedlab_agent.broker_research_runs run_row
SET status = 'paused', code = 'campaign_checkpoint_unpinned', generation = generation + 1,
    lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
FROM pg_temp.broker_campaign_step_pin_upgrade upgrade
WHERE upgrade.step_state = 'running' AND upgrade.active_safe IS NOT TRUE
  AND run_row.tenant_id = upgrade.tenant_id AND run_row.run_id = upgrade.run_id
  AND run_row.status IN ('queued','running','waiting','paused');

UPDATE enrichedlab_agent.broker_campaigns campaign_row
SET state = 'stopping', state_reason = 'checkpoint_unpinned', generation = generation + 1,
    lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
FROM (SELECT DISTINCT tenant_id, campaign_id
      FROM pg_temp.broker_campaign_step_pin_upgrade
      WHERE step_state = 'running' AND active_safe IS NOT TRUE) unsafe
WHERE campaign_row.tenant_id = unsafe.tenant_id
  AND campaign_row.campaign_id = unsafe.campaign_id
  AND campaign_row.state IN ('planned','running','pausing','paused');

DO $$
DECLARE legacy_constraint name;
BEGIN
  FOR legacy_constraint IN
    SELECT constraint_row.conname
    FROM pg_catalog.pg_constraint constraint_row
    WHERE constraint_row.conrelid = 'enrichedlab_agent.broker_campaign_steps'::regclass
      AND constraint_row.contype = 'c'
      AND constraint_row.conname <> 'broker_campaign_steps_state_shape'
      AND ((SELECT attribute.attnum
            FROM pg_catalog.pg_attribute attribute
            WHERE attribute.attrelid = constraint_row.conrelid
              AND attribute.attname = 'project_id') = ANY (constraint_row.conkey)
        OR pg_get_constraintdef(constraint_row.oid) LIKE '%project_id IS NULL%')
      AND ((SELECT attribute.attnum
            FROM pg_catalog.pg_attribute attribute
            WHERE attribute.attrelid = constraint_row.conrelid
              AND attribute.attname = 'outcome_class') = ANY (constraint_row.conkey)
        OR pg_get_constraintdef(constraint_row.oid) LIKE '%outcome_class IS NULL%')
  LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.broker_campaign_steps DROP CONSTRAINT %I',
      legacy_constraint);
  END LOOP;
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_constraint
    WHERE conrelid = 'enrichedlab_agent.broker_campaign_steps'::regclass
      AND conname = 'broker_campaign_steps_state_shape'
  ) THEN
    ALTER TABLE enrichedlab_agent.broker_campaign_steps
      ADD CONSTRAINT broker_campaign_steps_state_shape CHECK (
        (state = 'planned' AND project_id IS NULL AND run_id IS NULL
          AND checkpoint_revision IS NULL AND started_at IS NULL
          AND outcome_class IS NULL AND outcome IS NULL AND finished_at IS NULL)
        OR (state = 'running' AND project_id IS NOT NULL AND run_id IS NOT NULL
          AND checkpoint_revision IS NOT NULL AND started_at IS NOT NULL
          AND outcome_class IS NULL AND outcome IS NULL AND finished_at IS NULL)
        OR (state = 'finished' AND project_id IS NOT NULL AND run_id IS NOT NULL
          AND checkpoint_revision IS NOT NULL AND started_at IS NOT NULL
          AND outcome_class IS NOT NULL AND outcome IS NOT NULL AND finished_at IS NOT NULL));
  END IF;

  FOR legacy_constraint IN
    SELECT constraint_row.conname
    FROM pg_catalog.pg_constraint constraint_row
    WHERE constraint_row.conrelid = 'enrichedlab_agent.broker_campaign_steps'::regclass
      AND constraint_row.contype = 'f'
      AND constraint_row.conname <> 'broker_campaign_steps_checkpoint_revision_fkey'
      AND (SELECT attribute.attnum
           FROM pg_catalog.pg_attribute attribute
           WHERE attribute.attrelid = constraint_row.conrelid
             AND attribute.attname = 'checkpoint_revision') = ANY (constraint_row.conkey)
  LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.broker_campaign_steps DROP CONSTRAINT %I',
      legacy_constraint);
  END LOOP;
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_constraint
    WHERE conrelid = 'enrichedlab_agent.broker_campaign_steps'::regclass
      AND conname = 'broker_campaign_steps_checkpoint_revision_fkey'
  ) THEN
    ALTER TABLE enrichedlab_agent.broker_campaign_steps
      ADD CONSTRAINT broker_campaign_steps_checkpoint_revision_fkey
      FOREIGN KEY (tenant_id, project_id, checkpoint_revision)
      REFERENCES enrichedlab_agent.broker_project_checkpoints (tenant_id, project_id, revision);
  END IF;
END;
$$;

INSERT INTO enrichedlab_agent.broker_project_grants AS campaign_grant
  (tenant_id, project_id, principal_id, capabilities)
SELECT project_row.tenant_id, project_row.project_id, campaign_row.started_by,
  ARRAY['read']::text[]
FROM enrichedlab_agent.broker_projects project_row
JOIN enrichedlab_agent.broker_campaigns campaign_row
  ON campaign_row.tenant_id = project_row.tenant_id
 AND campaign_row.campaign_principal = project_row.owner_id
ON CONFLICT (tenant_id, project_id, principal_id) DO UPDATE
SET capabilities = EXCLUDED.capabilities
WHERE campaign_grant.capabilities IS DISTINCT FROM EXCLUDED.capabilities;

CREATE INDEX IF NOT EXISTS broker_campaign_steps_branch
  ON enrichedlab_agent.broker_campaign_steps (tenant_id, campaign_id, branch_id, created_at);
CREATE INDEX IF NOT EXISTS broker_campaign_steps_running
  ON enrichedlab_agent.broker_campaign_steps (tenant_id, campaign_id, state, started_at)
  WHERE state = 'running';

-- A campaign project has one immutable person-set scope. Only its unresolvable campaign
-- principal may author later working checkpoints, and those checkpoints may change the
-- plan and interpretation but never the question, constraints, or workspace snapshot.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_checkpoint_guard()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_principal uuid;
DECLARE initial_state jsonb;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', NEW.tenant_id, true);
  SELECT campaign_row.campaign_principal INTO campaign_principal
  FROM enrichedlab_agent.broker_projects project_row
  JOIN enrichedlab_agent.broker_campaigns campaign_row
    ON campaign_row.tenant_id = project_row.tenant_id
   AND campaign_row.campaign_principal = project_row.owner_id
  WHERE project_row.tenant_id = NEW.tenant_id AND project_row.project_id = NEW.project_id;
  IF NOT FOUND THEN RETURN NEW; END IF;
  IF NEW.author_id IS DISTINCT FROM campaign_principal THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:campaign_checkpoint_forbidden';
  END IF;
  SELECT checkpoint.state INTO initial_state
  FROM enrichedlab_agent.broker_project_checkpoints checkpoint
  WHERE checkpoint.tenant_id = NEW.tenant_id AND checkpoint.project_id = NEW.project_id
    AND checkpoint.revision = 1;
  IF NOT FOUND THEN
    IF NEW.revision <> 1 THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_checkpoint_scope_changed';
    END IF;
  ELSIF NEW.state->'goal' IS DISTINCT FROM initial_state->'goal'
     OR NEW.state->'constraints' IS DISTINCT FROM initial_state->'constraints'
     OR NEW.state->'workspace' IS DISTINCT FROM initial_state->'workspace' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_checkpoint_scope_changed';
  END IF;
  RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_checkpoint_guard() FROM PUBLIC;
DROP TRIGGER IF EXISTS campaign_checkpoint_guard
  ON enrichedlab_agent.broker_project_checkpoints;
CREATE TRIGGER campaign_checkpoint_guard
  BEFORE INSERT ON enrichedlab_agent.broker_project_checkpoints
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_campaign_checkpoint_guard();

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_pin_checkpoint()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', NEW.tenant_id, true);
  UPDATE enrichedlab_agent.broker_campaign_steps
  SET checkpoint_revision = NEW.revision
  WHERE tenant_id = NEW.tenant_id AND project_id = NEW.project_id;
  RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_pin_checkpoint() FROM PUBLIC;
DROP TRIGGER IF EXISTS campaign_pin_checkpoint
  ON enrichedlab_agent.broker_project_checkpoints;
CREATE TRIGGER campaign_pin_checkpoint
  AFTER INSERT ON enrichedlab_agent.broker_project_checkpoints
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_campaign_pin_checkpoint();

-- The API checkpoint route is never an internal campaign writer, even if a caller
-- supplies the campaign principal's identifier. This is the database backstop for
-- API authorization and remains true after the starter loses the capability.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_checkpoint_project(
  p_tenant text, p_actor uuid, p_id uuid, p_expected bigint, p_request uuid, p_state jsonb
)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE principal_class text;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_projects project_row
    JOIN enrichedlab_agent.broker_campaigns campaign_row
      ON campaign_row.tenant_id = project_row.tenant_id
     AND campaign_row.campaign_principal = project_row.owner_id
    WHERE project_row.tenant_id = p_tenant AND project_row.project_id = p_id
  ) THEN
    IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_id, 'read') THEN
      RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
    END IF;
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:campaign_checkpoint_forbidden';
  END IF;
  SELECT principal.principal_class INTO principal_class
  FROM enrichedlab_agent.broker_principals principal
  WHERE principal.tenant_id = p_tenant AND principal.principal_id = p_actor;
  RETURN enrichedlab_agent.broker_write_checkpoint(
    p_tenant, p_actor, p_id, p_expected, p_request, p_state,
    CASE WHEN principal_class = 'person' THEN 'person' ELSE 'service_account' END,
    NULL, NULL, p_actor);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_checkpoint_project(text,uuid,uuid,bigint,uuid,jsonb) FROM PUBLIC;

-- Coordinator claims call the ordinary project reader as the campaign principal.
-- For those reads, use the revision pinned by the campaign step rather than an
-- independently selected latest checkpoint. Other project readers keep the usual view.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_project(
  p_tenant text, p_actor uuid, p_id uuid
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE result jsonb;
DECLARE visible_revision bigint;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_id, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  SELECT step_row.checkpoint_revision INTO visible_revision
  FROM enrichedlab_agent.broker_campaign_steps step_row
  JOIN enrichedlab_agent.broker_campaigns campaign_row
    ON campaign_row.tenant_id = step_row.tenant_id
   AND campaign_row.campaign_id = step_row.campaign_id
  WHERE step_row.tenant_id = p_tenant AND step_row.project_id = p_id
    AND campaign_row.campaign_principal = p_actor;
  IF visible_revision IS NULL THEN
    SELECT project_row.revision INTO visible_revision
    FROM enrichedlab_agent.broker_projects project_row
    WHERE project_row.tenant_id = p_tenant AND project_row.project_id = p_id;
  END IF;
  SELECT to_jsonb(project_row) - 'tenant_id' INTO result
  FROM enrichedlab_agent.broker_projects project_row
  WHERE project_row.tenant_id = p_tenant AND project_row.project_id = p_id;
  RETURN result || jsonb_build_object(
    'capabilities', (SELECT coalesce(jsonb_agg(capability), '[]'::jsonb)
      FROM unnest(ARRAY['read','review','contribute','steer','cancel','manage']) capability
      WHERE enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_id, capability)),
    'checkpoint', (SELECT jsonb_build_object(
        'revision', checkpoint.revision, 'author_id', checkpoint.author_id,
        'created_at', checkpoint.created_at, 'state', checkpoint.state)
      FROM enrichedlab_agent.broker_project_checkpoints checkpoint
      WHERE checkpoint.tenant_id = p_tenant AND checkpoint.project_id = p_id
        AND checkpoint.revision = visible_revision),
    'tasks', (SELECT coalesce(jsonb_agg(to_jsonb(task_view)
        ORDER BY task_view.submitted_at DESC, task_view.task_id DESC), '[]'::jsonb)
      FROM (
        SELECT task.task_id, project_task.action_key, project_task.parent_task_id,
          task.profile_key AS profile, task.status, task.attempt, task.updated_at, task.submitted_at,
          CASE WHEN figure.task_id IS NULL THEN NULL ELSE jsonb_build_object(
            'title', task.objective, 'feature', figure.feature, 'source_task', figure.source_task_id,
            'table_path', figure.table_path, 'previous', figure.previous_task_id) END AS figure
        FROM enrichedlab_agent.broker_project_tasks project_task
        JOIN enrichedlab_agent.broker_tasks task USING (tenant_id, task_id)
        LEFT JOIN enrichedlab_agent.broker_figure_requests figure
          ON figure.tenant_id = project_task.tenant_id AND figure.task_id = project_task.task_id
        WHERE project_task.tenant_id = p_tenant AND project_task.project_id = p_id
        ORDER BY task.submitted_at DESC, task.task_id DESC LIMIT 200
      ) task_view),
    'task_count', (SELECT count(*) FROM enrichedlab_agent.broker_project_tasks
      WHERE tenant_id = p_tenant AND project_id = p_id),
    'inspection_expires_at', (SELECT inspection.expires_at
      FROM enrichedlab_agent.broker_project_inspections inspection
      WHERE inspection.tenant_id = p_tenant AND inspection.project_id = p_id
        AND inspection.principal_id = p_actor AND inspection.expires_at > clock_timestamp())
  );
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_project(text,uuid,uuid) FROM PUBLIC;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_records (
  tenant_id text NOT NULL,
  campaign_id uuid NOT NULL,
  record_id uuid NOT NULL,
  sequence bigint NOT NULL,
  kind text NOT NULL,
  text text NOT NULL,
  origin text NOT NULL,
  status text NOT NULL,
  refs jsonb NOT NULL DEFAULT '[]'::jsonb,
  superseded_by uuid,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, campaign_id, record_id),
  UNIQUE (tenant_id, campaign_id, sequence),
  FOREIGN KEY (tenant_id, campaign_id)
    REFERENCES enrichedlab_agent.broker_campaigns (tenant_id, campaign_id),
  FOREIGN KEY (tenant_id, campaign_id, superseded_by)
    REFERENCES enrichedlab_agent.broker_campaign_records (tenant_id, campaign_id, record_id),
  CHECK (kind IN ('question','hypothesis','attempt','finding','prospect','lesson','request')),
  CHECK (char_length(btrim(text)) BETWEEN 1 AND 2000 AND text !~ '[[:cntrl:]]'),
  CHECK (origin IN ('director','planner')),
  CHECK (status IN ('open','supported','contradicted','abandoned','promoted','withdrawn')),
  CHECK (sequence > 0),
  CHECK (jsonb_typeof(refs) = 'array' AND jsonb_array_length(refs) <= 64
    AND octet_length(refs::text) <= 8192),
  CHECK (superseded_by IS NULL OR superseded_by <> record_id)
);

CREATE INDEX IF NOT EXISTS broker_campaign_records_page
  ON enrichedlab_agent.broker_campaign_records (tenant_id, campaign_id, sequence DESC);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_relations (
  tenant_id text NOT NULL,
  campaign_id uuid NOT NULL,
  relation_id uuid NOT NULL,
  kind text NOT NULL,
  source_record uuid NOT NULL,
  target_record uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, campaign_id, relation_id),
  UNIQUE (tenant_id, campaign_id, kind, source_record, target_record),
  FOREIGN KEY (tenant_id, campaign_id, source_record)
    REFERENCES enrichedlab_agent.broker_campaign_records (tenant_id, campaign_id, record_id),
  FOREIGN KEY (tenant_id, campaign_id, target_record)
    REFERENCES enrichedlab_agent.broker_campaign_records (tenant_id, campaign_id, record_id),
  CHECK (kind IN ('supports','contradicts','tested_by','retry_of','derived_from','cites_evidence','answers')),
  CHECK (source_record <> target_record)
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_campaign_events (
  tenant_id text NOT NULL,
  campaign_id uuid NOT NULL,
  sequence bigint NOT NULL,
  event_id uuid NOT NULL DEFAULT gen_random_uuid(),
  kind text NOT NULL,
  detail jsonb NOT NULL DEFAULT '{}'::jsonb,
  occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, campaign_id, sequence),
  UNIQUE (tenant_id, event_id),
  FOREIGN KEY (tenant_id, campaign_id)
    REFERENCES enrichedlab_agent.broker_campaigns (tenant_id, campaign_id),
  CHECK (kind IN ('created','started','paused','resumed','stop_requested','stopped','finished',
    'budget_exhausted','rule_fired','step_started','step_finished','branch_opened','branch_closed',
    'proposal_refused','stop_all')),
  CHECK (sequence > 0),
  CHECK (jsonb_typeof(detail) = 'object' AND octet_length(detail::text) <= 2048)
);

CREATE INDEX IF NOT EXISTS broker_campaign_events_page
  ON enrichedlab_agent.broker_campaign_events (tenant_id, campaign_id, sequence);

-- These tables are function-only and also enforce the connection's tenant setting for the migration owner.
DO $$
DECLARE table_name text;
BEGIN
  FOREACH table_name IN ARRAY ARRAY[
    'broker_capability_grants','broker_campaigns','broker_campaign_branches','broker_campaign_steps',
    'broker_campaign_records','broker_campaign_relations','broker_campaign_events'
  ] LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I ENABLE ROW LEVEL SECURITY', table_name);
    EXECUTE format('ALTER TABLE enrichedlab_agent.%I FORCE ROW LEVEL SECURITY', table_name);
    EXECUTE format('REVOKE ALL ON enrichedlab_agent.%I FROM PUBLIC', table_name);
    EXECUTE format('DROP POLICY IF EXISTS campaign_tenant ON enrichedlab_agent.%I', table_name);
    EXECUTE format('CREATE POLICY campaign_tenant ON enrichedlab_agent.%I USING '
      '(tenant_id = current_setting(''enrichedlab.tenant_id'', true)) WITH CHECK '
      '(tenant_id = current_setting(''enrichedlab.tenant_id'', true))', table_name);
  END LOOP;
END;
$$;

DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_campaign_events;
CREATE TRIGGER immutable_record
  BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_campaign_events
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_normalize_question(p_question text)
RETURNS text
LANGUAGE sql
IMMUTABLE
SET search_path = pg_catalog
AS $$
  SELECT btrim(regexp_replace(lower(p_question), '[^[:alnum:]]+', ' ', 'g'))
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_normalize_question(text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_event_detail_valid(p_detail jsonb)
RETURNS boolean
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
BEGIN
  IF jsonb_typeof(p_detail) IS DISTINCT FROM 'object' THEN RETURN false; END IF;
  RETURN octet_length(p_detail::text) <= 2048
    AND (SELECT count(*) <= 16 FROM jsonb_object_keys(p_detail))
    AND NOT EXISTS (
      SELECT 1 FROM jsonb_each(p_detail) AS item(key, value)
      WHERE key NOT IN ('rule','outcome_class','reason_code','actor_id','branch_id','step_id','run_id','project_id',
                        'record_id','count','threshold','remaining','state','purpose','lab','idempotent')
         OR jsonb_typeof(value) NOT IN ('string','number','boolean','null')
         OR (jsonb_typeof(value) = 'string' AND (char_length(value #>> '{}') > 128
              OR value #>> '{}' !~ '^[a-zA-Z0-9_.:/-]+$'))
         OR (jsonb_typeof(value) = 'number' AND value::text !~ '^-?[0-9]{1,12}$')
    );
EXCEPTION WHEN invalid_parameter_value OR invalid_text_representation OR numeric_value_out_of_range THEN
  RETURN false;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_event_detail_valid(jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_event(
  p_tenant text, p_campaign uuid, p_kind text, p_detail jsonb
)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE next_sequence bigint;
BEGIN
  IF p_kind IS NULL OR p_kind NOT IN ('created','started','paused','resumed','stop_requested','stopped','finished',
      'budget_exhausted','rule_fired','step_started','step_finished','branch_opened','branch_closed',
      'proposal_refused','stop_all')
     OR NOT enrichedlab_agent.broker_campaign_event_detail_valid(p_detail) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  UPDATE enrichedlab_agent.broker_campaigns
  SET event_sequence = event_sequence + 1, updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign
  RETURNING event_sequence INTO next_sequence;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  INSERT INTO enrichedlab_agent.broker_campaign_events
    (tenant_id, campaign_id, sequence, kind, detail)
  VALUES (p_tenant, p_campaign, next_sequence, p_kind, p_detail);
  RETURN next_sequence;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_event(text,uuid,text,jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_has_capability(
  p_tenant text, p_actor uuid, p_group text
)
RETURNS boolean
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT EXISTS (
    SELECT 1
    FROM enrichedlab_agent.broker_principals principal
    JOIN enrichedlab_agent.broker_group_memberships membership
      ON membership.tenant_id = principal.tenant_id
     AND membership.principal_id = principal.principal_id
     AND membership.group_key = p_group
    JOIN enrichedlab_agent.broker_research_groups lab
      ON lab.tenant_id = membership.tenant_id AND lab.group_key = membership.group_key AND lab.enabled
    JOIN enrichedlab_agent.broker_tenants tenant
      ON tenant.tenant_id = principal.tenant_id AND tenant.enabled
    JOIN enrichedlab_agent.broker_capability_grants grant_row
      ON grant_row.tenant_id = principal.tenant_id
     AND grant_row.email = principal.email
     AND grant_row.group_key = membership.group_key
     AND grant_row.capability = 'research.discovery.autonomous'
     AND grant_row.enabled
    WHERE principal.tenant_id = p_tenant
      AND principal.principal_id = p_actor
      AND principal.enabled
      AND principal.principal_class = 'person'
  )
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_has_capability(text,uuid,text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_scope_valid(
  p_tenant text, p_group text, p_scope jsonb
)
RETURNS boolean
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  IF jsonb_typeof(p_scope) IS DISTINCT FROM 'object' THEN RETURN false; END IF;
  RETURN coalesce((NOT EXISTS (SELECT 1 FROM jsonb_object_keys(p_scope) key
                    WHERE key NOT IN ('starting_points','profiles','workspace'))
    AND EXISTS (
      SELECT 1 FROM enrichedlab_agent.broker_research_groups lab
      JOIN enrichedlab_agent.broker_tenants tenant
        ON tenant.tenant_id = lab.tenant_id AND tenant.enabled
      WHERE lab.tenant_id = p_tenant AND lab.group_key = p_group AND lab.enabled
    )
    AND jsonb_typeof(p_scope->'starting_points') = 'array'
    AND jsonb_array_length(p_scope->'starting_points') BETWEEN 1 AND 16
    AND jsonb_typeof(p_scope->'profiles') = 'array'
    AND jsonb_array_length(p_scope->'profiles') BETWEEN 1 AND 16
    AND (SELECT count(*) = count(DISTINCT value)
         FROM jsonb_array_elements_text(p_scope->'starting_points'))
    AND (SELECT count(*) = count(DISTINCT value)
         FROM jsonb_array_elements_text(p_scope->'profiles'))
    AND NOT EXISTS (SELECT 1 FROM jsonb_array_elements(p_scope->'starting_points') value
                    WHERE jsonb_typeof(value) <> 'string'
                       OR value #>> '{}' !~ '^[a-zA-Z0-9_-]{1,100}$')
    AND NOT EXISTS (SELECT 1 FROM jsonb_array_elements(p_scope->'profiles') value
                    WHERE jsonb_typeof(value) <> 'string'
                       OR value #>> '{}' !~ '^[a-z][a-z0-9_-]{1,63}$')
    AND NOT EXISTS (
      SELECT 1 FROM jsonb_array_elements_text(p_scope->'profiles') AS scoped_profile(value)
      WHERE NOT EXISTS (
        SELECT 1
        FROM enrichedlab_agent.broker_group_profile_grants grant_row
        JOIN enrichedlab_agent.broker_agent_profiles profile
          ON profile.tenant_id = grant_row.tenant_id AND profile.profile_key = grant_row.profile_key
        WHERE grant_row.tenant_id = p_tenant AND grant_row.group_key = p_group
          AND grant_row.profile_key = scoped_profile.value AND profile.enabled
          AND NOT enrichedlab_agent.broker_run_excluded_route(profile.route)
      )
    )
    AND jsonb_typeof(p_scope->'workspace') = 'object'
    AND NOT EXISTS (SELECT 1 FROM jsonb_object_keys(p_scope->'workspace') key
                    WHERE key NOT IN ('schema_version','profile','image_digest','source_digest',
                                      'input_refs','tool_refs','preparation_receipt','starting_points'))
    AND (p_scope->'workspace'->>'schema_version') = '1'
    AND (p_scope->'workspace'->>'profile') ~ '^[a-z][a-z0-9_-]{1,127}$'
    AND (p_scope->'workspace'->>'image_digest') ~ '^sha256:[0-9a-f]{64}$'
    AND (p_scope->'workspace'->>'source_digest') ~ '^sha256:[0-9a-f]{64}$'
    AND jsonb_typeof(p_scope->'workspace'->'input_refs') = 'array'
    AND jsonb_array_length(p_scope->'workspace'->'input_refs') BETWEEN 1 AND 64
    AND jsonb_typeof(p_scope->'workspace'->'tool_refs') = 'array'
    AND jsonb_array_length(p_scope->'workspace'->'tool_refs') BETWEEN 1 AND 64
    AND NOT EXISTS (
      SELECT 1 FROM (
        SELECT value FROM jsonb_array_elements(p_scope->'workspace'->'input_refs')
        UNION ALL
        SELECT value FROM jsonb_array_elements(p_scope->'workspace'->'tool_refs')
      ) workspace_ref
      WHERE jsonb_typeof(workspace_ref.value) <> 'string'
         OR char_length(workspace_ref.value #>> '{}') NOT BETWEEN 1 AND 512
         OR (workspace_ref.value #>> '{}') ~ '[[:space:]?#@]'
         OR (workspace_ref.value #>> '{}') !~
            '^(artifact|input|receipt|tool|profile|source)://'
    )
    AND p_scope->'workspace' ? 'preparation_receipt'
    AND p_scope->'workspace'->'preparation_receipt' = 'null'::jsonb
    AND (NOT (p_scope->'workspace' ? 'starting_points') OR (
      jsonb_typeof(p_scope->'workspace'->'starting_points') = 'array'
      AND jsonb_array_length(p_scope->'workspace'->'starting_points') BETWEEN 2 AND 16
    ))), false);
EXCEPTION WHEN invalid_parameter_value OR invalid_text_representation OR numeric_value_out_of_range THEN
  RETURN false;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_scope_valid(text,text,jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_budgets_valid(p_budgets jsonb)
RETURNS boolean
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
BEGIN
  IF jsonb_typeof(p_budgets) IS DISTINCT FROM 'object' THEN RETURN false; END IF;
  RETURN (SELECT count(*) = 4 FROM jsonb_object_keys(p_budgets))
    AND NOT EXISTS (SELECT 1 FROM jsonb_object_keys(p_budgets) key
                    WHERE key NOT IN ('wall_seconds','max_runs','max_concurrent_runs','max_turns_per_run'))
    AND NOT EXISTS (SELECT 1 FROM jsonb_each(p_budgets) item
                    WHERE jsonb_typeof(item.value) <> 'number' OR item.value::text !~ '^[0-9]{1,10}$')
    AND (p_budgets->>'wall_seconds')::numeric BETWEEN 1 AND 1209600
    AND (p_budgets->>'max_runs')::numeric BETWEEN 1 AND 500
    AND (p_budgets->>'max_concurrent_runs')::numeric BETWEEN 1 AND 4
    AND (p_budgets->>'max_turns_per_run')::numeric BETWEEN 1 AND 64;
EXCEPTION WHEN invalid_parameter_value OR invalid_text_representation OR numeric_value_out_of_range THEN
  RETURN false;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_budgets_valid(jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_stop_rules_valid(p_rules jsonb)
RETURNS boolean
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog
AS $$
BEGIN
  IF jsonb_typeof(p_rules) IS DISTINCT FROM 'object' THEN RETURN false; END IF;
  RETURN (SELECT count(*) = 4 FROM jsonb_object_keys(p_rules))
    AND NOT EXISTS (SELECT 1 FROM jsonb_object_keys(p_rules) key
                    WHERE key NOT IN ('change_approach_repeats','branch_window','campaign_window','planner_refused_cycles'))
    AND NOT EXISTS (SELECT 1 FROM jsonb_each(p_rules) item
                    WHERE jsonb_typeof(item.value) <> 'number' OR item.value::text !~ '^[0-9]{1,3}$')
    AND (p_rules->>'change_approach_repeats')::numeric BETWEEN 2 AND 8
    AND (p_rules->>'branch_window')::numeric BETWEEN 2 AND 64
    AND (p_rules->>'campaign_window')::numeric BETWEEN 2 AND 128
    AND (p_rules->>'planner_refused_cycles')::numeric BETWEEN 1 AND 8;
EXCEPTION WHEN invalid_parameter_value OR invalid_text_representation OR numeric_value_out_of_range THEN
  RETURN false;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_stop_rules_valid(jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_create_campaign(
  p_tenant text,
  p_actor uuid,
  p_campaign uuid,
  p_group text,
  p_goal text,
  p_constraints text,
  p_scope jsonb,
  p_budgets jsonb,
  p_stop_rules jsonb
)
RETURNS uuid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  existing enrichedlab_agent.broker_campaigns%ROWTYPE;
  principal uuid := gen_random_uuid();
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF p_actor IS NULL OR p_campaign IS NULL OR p_group IS NULL OR p_goal IS NULL
     OR p_scope IS NULL OR p_budgets IS NULL OR p_stop_rules IS NULL THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_invalid';
  END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended(
    json_build_array(p_tenant, 'campaign', p_campaign)::text, 0));
  IF NOT enrichedlab_agent.broker_campaign_has_capability(p_tenant, p_actor, p_group) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:campaign_capability_required';
  END IF;
  IF char_length(btrim(p_goal)) NOT BETWEEN 1 AND 4000 OR p_goal ~ '[[:cntrl:]]'
     OR char_length(coalesce(p_constraints, '')) > 4000 OR coalesce(p_constraints, '') ~ '[[:cntrl:]]'
     OR NOT enrichedlab_agent.broker_campaign_scope_valid(p_tenant, p_group, p_scope)
     OR NOT enrichedlab_agent.broker_campaign_budgets_valid(p_budgets)
     OR NOT enrichedlab_agent.broker_campaign_stop_rules_valid(p_stop_rules) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_invalid';
  END IF;
  SELECT * INTO existing
  FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  IF FOUND THEN
    IF existing.group_key <> p_group OR existing.started_by <> p_actor OR existing.goal <> p_goal
       OR existing.constraints <> coalesce(p_constraints, '') OR existing.scope <> p_scope
       OR existing.budgets <> p_budgets OR existing.stop_rules <> p_stop_rules THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
    RETURN p_campaign;
  END IF;

  INSERT INTO enrichedlab_agent.broker_principals
    (tenant_id, principal_id, issuer, subject, email, display_name, principal_class, enabled)
  VALUES
    (p_tenant, principal, 'urn:enrichedlab:campaign', 'campaign:' || p_campaign::text,
     'campaign+' || p_campaign::text || '@campaign.invalid', 'Research campaign', 'campaign', true);
  INSERT INTO enrichedlab_agent.broker_campaigns
    (tenant_id, campaign_id, group_key, started_by, campaign_principal, goal, constraints,
     scope, budgets, stop_rules)
  VALUES
    (p_tenant, p_campaign, p_group, p_actor, principal, p_goal, coalesce(p_constraints, ''),
     p_scope, p_budgets, p_stop_rules);
  -- Researcher is the smallest lab role. Project ownership supplies contribute and steer only for its own projects.
  INSERT INTO enrichedlab_agent.broker_group_memberships
    (tenant_id, group_key, principal_id, role)
  VALUES (p_tenant, p_group, principal, 'researcher');
  PERFORM enrichedlab_agent.broker_campaign_event(
    p_tenant, p_campaign, 'created',
    jsonb_build_object('actor_id', p_actor, 'lab', p_group, 'state', 'planned'));
  RETURN p_campaign;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_create_campaign(text,uuid,uuid,text,text,text,jsonb,jsonb,jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_snapshot(
  p_tenant text, p_campaign uuid, p_record_limit integer
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE result jsonb;
BEGIN
  IF p_record_limit IS NULL OR p_record_limit NOT BETWEEN 0 AND 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:list_limit_invalid';
  END IF;
  SELECT (to_jsonb(c) - 'tenant_id' - 'lease_owner' - 'lease_until') || jsonb_build_object(
      'budgets_left', jsonb_build_object(
        'wall_seconds', greatest(0, (c.budgets->>'wall_seconds')::integer - CASE
          WHEN c.started_at IS NULL THEN 0
          ELSE floor(extract(epoch FROM statement_timestamp() - c.started_at))::integer END),
        'runs', greatest(0, (c.budgets->>'max_runs')::integer - c.run_count),
        'concurrent_runs', greatest(0,
          (c.budgets->>'max_concurrent_runs')::integer - c.active_runs),
        'max_turns_per_run', (c.budgets->>'max_turns_per_run')::integer),
      'branches', (SELECT coalesce(jsonb_agg(to_jsonb(branch_row) - 'tenant_id'
          ORDER BY branch_row.created_at, branch_row.branch_id), '[]'::jsonb)
        FROM (SELECT * FROM enrichedlab_agent.broker_campaign_branches b
              WHERE b.tenant_id = c.tenant_id AND b.campaign_id = c.campaign_id
              ORDER BY b.created_at, b.branch_id LIMIT 500) branch_row),
      'steps', (SELECT coalesce(jsonb_agg(to_jsonb(step_row) - 'tenant_id'
          ORDER BY step_row.created_at, step_row.step_id), '[]'::jsonb)
        FROM (SELECT s.*,
                (SELECT coalesce(jsonb_agg(bounded_task.task_id ORDER BY bounded_task.task_id), '[]'::jsonb)
                 FROM (SELECT project_task.task_id
                       FROM enrichedlab_agent.broker_project_tasks project_task
                       WHERE project_task.tenant_id = s.tenant_id
                         AND project_task.project_id = s.project_id
                       ORDER BY project_task.task_id LIMIT 256) bounded_task) AS task_ids,
                (SELECT coalesce(jsonb_agg(bounded_artifact.ref ORDER BY bounded_artifact.ref), '[]'::jsonb)
                 FROM (SELECT 'artifact://tasks/' || artifact.task_id::text || '/' ||
                              artifact.artifact_id::text AS ref
                       FROM enrichedlab_agent.broker_project_tasks project_task
                       JOIN enrichedlab_agent.broker_artifacts artifact
                         ON artifact.tenant_id = project_task.tenant_id
                        AND artifact.task_id = project_task.task_id
                       WHERE project_task.tenant_id = s.tenant_id
                         AND project_task.project_id = s.project_id
                         AND artifact.visibility = 'researcher'
                       ORDER BY artifact.task_id, artifact.artifact_id LIMIT 256) bounded_artifact) AS artifact_refs
              FROM enrichedlab_agent.broker_campaign_steps s
              WHERE s.tenant_id = c.tenant_id AND s.campaign_id = c.campaign_id
              ORDER BY s.created_at, s.step_id LIMIT 500) step_row),
      'records', (SELECT coalesce(jsonb_agg(to_jsonb(record_row) - 'tenant_id'
          ORDER BY record_row.sequence DESC), '[]'::jsonb)
        FROM (SELECT * FROM enrichedlab_agent.broker_campaign_records r
              WHERE r.tenant_id = c.tenant_id AND r.campaign_id = c.campaign_id
              ORDER BY r.sequence DESC LIMIT p_record_limit) record_row),
      'open_requests', (SELECT coalesce(jsonb_agg(to_jsonb(request_row) - 'tenant_id'
          ORDER BY request_row.sequence DESC), '[]'::jsonb)
        FROM (SELECT * FROM enrichedlab_agent.broker_campaign_records r
              WHERE r.tenant_id = c.tenant_id AND r.campaign_id = c.campaign_id
                AND r.kind = 'request' AND r.status = 'open'
              ORDER BY r.sequence DESC LIMIT 100) request_row),
      'relations', (SELECT coalesce(jsonb_agg(to_jsonb(relation_row) - 'tenant_id'
          ORDER BY relation_row.created_at, relation_row.relation_id), '[]'::jsonb)
        FROM (SELECT * FROM enrichedlab_agent.broker_campaign_relations rel
              WHERE rel.tenant_id = c.tenant_id AND rel.campaign_id = c.campaign_id
              ORDER BY rel.created_at DESC, rel.relation_id LIMIT 1000) relation_row),
      'rule_notices', (SELECT coalesce(jsonb_agg(notice.detail ORDER BY notice.sequence DESC),
                                         '[]'::jsonb)
        FROM (SELECT event_row.sequence, event_row.detail
              FROM enrichedlab_agent.broker_campaign_events event_row
              WHERE event_row.tenant_id = c.tenant_id AND event_row.campaign_id = c.campaign_id
                AND event_row.kind = 'rule_fired'
              ORDER BY event_row.sequence DESC LIMIT 50) notice))
    INTO result
  FROM enrichedlab_agent.broker_campaigns c
  WHERE c.tenant_id = p_tenant AND c.campaign_id = p_campaign;
  RETURN result;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_snapshot(text,uuid,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_campaign(
  p_tenant text, p_actor uuid, p_campaign uuid
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO campaign_row FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_campaign_has_capability(
      p_tenant, p_actor, campaign_row.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  RETURN enrichedlab_agent.broker_campaign_snapshot(p_tenant, p_campaign, 100);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_campaign(text,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_campaigns(
  p_tenant text, p_actor uuid, p_group text, p_limit integer
)
RETURNS SETOF jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF p_limit IS NULL OR p_limit NOT BETWEEN 1 AND 100 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:list_limit_invalid';
  END IF;
  IF NOT enrichedlab_agent.broker_campaign_has_capability(p_tenant, p_actor, p_group) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:lab_not_found';
  END IF;
  RETURN QUERY
  SELECT to_jsonb(summary_row)
  FROM (
    SELECT c.campaign_id, c.group_key, c.started_by, c.goal, c.state, c.state_reason,
           c.run_count, c.active_runs, c.finished_steps, c.evidence_steps,
           c.started_at, c.finished_at, c.created_at, c.updated_at
    FROM enrichedlab_agent.broker_campaigns c
    WHERE c.tenant_id = p_tenant AND c.group_key = p_group
    ORDER BY c.updated_at DESC, c.campaign_id
    LIMIT p_limit
  ) summary_row;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_campaigns(text,uuid,text,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_events_page(
  p_tenant text, p_actor uuid, p_campaign uuid, p_after bigint, p_limit integer
)
RETURNS SETOF jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE group_key text;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT c.group_key INTO group_key FROM enrichedlab_agent.broker_campaigns c
  WHERE c.tenant_id = p_tenant AND c.campaign_id = p_campaign;
  IF group_key IS NULL OR NOT enrichedlab_agent.broker_campaign_has_capability(
      p_tenant, p_actor, group_key) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  IF p_after IS NULL OR p_after < 0 OR p_limit IS NULL OR p_limit NOT BETWEEN 1 AND 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:event_cursor_invalid';
  END IF;
  RETURN QUERY SELECT to_jsonb(event_row) - 'tenant_id'
    FROM enrichedlab_agent.broker_campaign_events event_row
    WHERE event_row.tenant_id = p_tenant AND event_row.campaign_id = p_campaign
      AND event_row.sequence > p_after
    ORDER BY event_row.sequence LIMIT p_limit;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_events_page(text,uuid,uuid,bigint,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_records_page(
  p_tenant text, p_actor uuid, p_campaign uuid, p_after bigint, p_limit integer
)
RETURNS SETOF jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE group_key text;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT c.group_key INTO group_key FROM enrichedlab_agent.broker_campaigns c
  WHERE c.tenant_id = p_tenant AND c.campaign_id = p_campaign;
  IF group_key IS NULL OR NOT enrichedlab_agent.broker_campaign_has_capability(
      p_tenant, p_actor, group_key) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  IF p_after IS NULL OR p_after < 0 OR p_limit IS NULL OR p_limit NOT BETWEEN 1 AND 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:event_cursor_invalid';
  END IF;
  RETURN QUERY SELECT to_jsonb(record_row) - 'tenant_id'
    FROM enrichedlab_agent.broker_campaign_records record_row
    WHERE record_row.tenant_id = p_tenant AND record_row.campaign_id = p_campaign
      AND record_row.sequence > p_after
    ORDER BY record_row.sequence LIMIT p_limit;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_records_page(text,uuid,uuid,bigint,integer) FROM PUBLIC;

-- Research pause fences the coordinator but deliberately leaves submitted tasks alone. Campaign reconciliation and
-- lifecycle finalization therefore share this definition of work that is still in flight.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_project_has_active_tasks(
  p_tenant text, p_project uuid
)
RETURNS boolean
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_project_tasks project_task
    JOIN enrichedlab_agent.broker_tasks task USING (tenant_id, task_id)
    WHERE project_task.tenant_id = p_tenant AND project_task.project_id = p_project
      AND task.status IN ('queued','leased','starting','running','cancelling')
  )
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_project_has_active_tasks(text,uuid) FROM PUBLIC;

-- Stop and pause drain existing research runs and their submitted tasks. No new step can start outside `running`;
-- once every running step is reconciled and every task has settled, this helper finalizes the lifecycle.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_finalize_lifecycle(
  p_tenant text, p_campaign uuid
)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
DECLARE running_count integer;
DECLARE active_task_projects integer;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO campaign_row FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign FOR UPDATE;
  IF NOT FOUND THEN RETURN NULL; END IF;
  SELECT count(*) INTO running_count
  FROM enrichedlab_agent.broker_campaign_steps
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND state = 'running';
  SELECT count(*) INTO active_task_projects
  FROM enrichedlab_agent.broker_campaign_steps step_row
  WHERE step_row.tenant_id = p_tenant AND step_row.campaign_id = p_campaign
    AND step_row.project_id IS NOT NULL
    AND enrichedlab_agent.broker_campaign_project_has_active_tasks(
      step_row.tenant_id, step_row.project_id);
  UPDATE enrichedlab_agent.broker_campaigns SET active_runs = running_count
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  IF running_count = 0 AND active_task_projects = 0 AND campaign_row.state = 'pausing' THEN
    UPDATE enrichedlab_agent.broker_campaigns
    SET state = 'paused', generation = generation + 1, lease_owner = NULL, lease_until = NULL,
        available_at = clock_timestamp(), updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    RETURN 'paused';
  ELSIF running_count = 0 AND active_task_projects = 0 AND campaign_row.state = 'stopping' THEN
    UPDATE enrichedlab_agent.broker_campaigns
    SET state = 'stopped', generation = generation + 1, lease_owner = NULL, lease_until = NULL,
        finished_at = coalesce(finished_at, clock_timestamp()), updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    UPDATE enrichedlab_agent.broker_principals SET enabled = false, updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND principal_id = campaign_row.campaign_principal;
    DELETE FROM enrichedlab_agent.broker_group_memberships
    WHERE tenant_id = p_tenant AND principal_id = campaign_row.campaign_principal;
    PERFORM enrichedlab_agent.broker_campaign_event(
      p_tenant, p_campaign, 'stopped', jsonb_build_object('reason_code', campaign_row.state_reason));
    RETURN 'stopped';
  END IF;
  RETURN campaign_row.state;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_finalize_lifecycle(text,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_start_campaign(
  p_tenant text, p_actor uuid, p_campaign uuid
)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO campaign_row FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_campaign_has_capability(
      p_tenant, p_actor, campaign_row.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  IF campaign_row.state <> 'planned' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_state_conflict';
  END IF;
  IF NOT enrichedlab_agent.broker_campaign_scope_valid(p_tenant, campaign_row.group_key, campaign_row.scope) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:campaign_scope_forbidden';
  END IF;
  UPDATE enrichedlab_agent.broker_campaigns
  SET state = 'running', state_reason = NULL, started_at = clock_timestamp(),
      available_at = clock_timestamp(), updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  PERFORM enrichedlab_agent.broker_campaign_event(
    p_tenant, p_campaign, 'started', jsonb_build_object('actor_id', p_actor, 'state', 'running'));
  RETURN 'running';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_start_campaign(text,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_pause_campaign(
  p_tenant text, p_actor uuid, p_campaign uuid
)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
DECLARE result text;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO campaign_row FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_campaign_has_capability(
      p_tenant, p_actor, campaign_row.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  IF campaign_row.state IN ('paused','pausing') THEN RETURN campaign_row.state; END IF;
  IF campaign_row.state <> 'running' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_state_conflict';
  END IF;
  UPDATE enrichedlab_agent.broker_campaigns
  SET state = 'pausing', state_reason = 'user_paused', generation = generation + 1,
      lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  PERFORM enrichedlab_agent.broker_campaign_event(
    p_tenant, p_campaign, 'paused',
    jsonb_build_object('actor_id', p_actor, 'state', 'pausing', 'reason_code', 'user_paused'));
  result := enrichedlab_agent.broker_campaign_finalize_lifecycle(p_tenant, p_campaign);
  RETURN result;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_pause_campaign(text,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_resume_campaign(
  p_tenant text, p_actor uuid, p_campaign uuid
)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO campaign_row FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_campaign_has_capability(
      p_tenant, p_actor, campaign_row.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  IF campaign_row.state <> 'paused' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_state_conflict';
  END IF;
  IF NOT enrichedlab_agent.broker_campaign_scope_valid(p_tenant, campaign_row.group_key, campaign_row.scope) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:campaign_scope_forbidden';
  END IF;
  UPDATE enrichedlab_agent.broker_campaigns
  SET state = 'running', state_reason = NULL, available_at = clock_timestamp(), updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  PERFORM enrichedlab_agent.broker_campaign_event(
    p_tenant, p_campaign, 'resumed', jsonb_build_object('actor_id', p_actor, 'state', 'running'));
  RETURN 'running';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_resume_campaign(text,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_stop_campaign(
  p_tenant text, p_actor uuid, p_campaign uuid
)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
DECLARE result text;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO campaign_row FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_campaign_has_capability(
      p_tenant, p_actor, campaign_row.group_key) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:campaign_not_found';
  END IF;
  IF campaign_row.state IN ('stopped','finished','failed') THEN RETURN campaign_row.state; END IF;
  IF campaign_row.state = 'stopping' THEN
    RETURN enrichedlab_agent.broker_campaign_finalize_lifecycle(p_tenant, p_campaign);
  END IF;
  UPDATE enrichedlab_agent.broker_campaigns
  SET state = 'stopping', state_reason = 'user_stopped', generation = generation + 1,
      lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  PERFORM enrichedlab_agent.broker_campaign_event(
    p_tenant, p_campaign, 'stop_requested',
    jsonb_build_object('actor_id', p_actor, 'reason_code', 'user_stopped'));
  result := enrichedlab_agent.broker_campaign_finalize_lifecycle(p_tenant, p_campaign);
  RETURN result;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_stop_campaign(text,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_refs_valid(
  p_tenant text, p_campaign uuid, p_refs jsonb
)
RETURNS boolean
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE reference text;
DECLARE identifier uuid;
BEGIN
  IF jsonb_typeof(p_refs) IS DISTINCT FROM 'array' OR jsonb_array_length(p_refs) > 64
     OR octet_length(p_refs::text) > 8192 THEN RETURN false; END IF;
  FOR reference IN SELECT value FROM jsonb_array_elements_text(p_refs) LOOP
    IF reference ~ '^step:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
      identifier := substring(reference FROM 6)::uuid;
      IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_steps
          WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND step_id = identifier) THEN RETURN false; END IF;
    ELSIF reference ~ '^run:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
      identifier := substring(reference FROM 5)::uuid;
      IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_steps
          WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND run_id = identifier) THEN RETURN false; END IF;
    ELSIF reference ~ '^project:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
      identifier := substring(reference FROM 9)::uuid;
      IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_steps
          WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND project_id = identifier) THEN RETURN false; END IF;
    ELSIF reference ~ '^task:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
      identifier := substring(reference FROM 6)::uuid;
      IF NOT EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_campaign_steps step_row
        JOIN enrichedlab_agent.broker_project_tasks project_task
          ON project_task.tenant_id = step_row.tenant_id AND project_task.project_id = step_row.project_id
        WHERE step_row.tenant_id = p_tenant AND step_row.campaign_id = p_campaign
          AND project_task.task_id = identifier
      ) THEN RETURN false; END IF;
    ELSIF reference ~ '^record:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
      identifier := substring(reference FROM 8)::uuid;
      IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_records
          WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND record_id = identifier) THEN RETURN false; END IF;
    ELSIF reference ~ '^artifact://tasks/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
      IF NOT EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_artifacts artifact
        JOIN enrichedlab_agent.broker_project_tasks project_task
          ON project_task.tenant_id = artifact.tenant_id AND project_task.task_id = artifact.task_id
        JOIN enrichedlab_agent.broker_campaign_steps step_row
          ON step_row.tenant_id = project_task.tenant_id AND step_row.project_id = project_task.project_id
        WHERE artifact.tenant_id = p_tenant AND step_row.campaign_id = p_campaign
          AND ('artifact://tasks/' || artifact.task_id::text || '/' || artifact.artifact_id::text) = reference
          AND artifact.visibility = 'researcher'
      ) THEN RETURN false; END IF;
    ELSE
      RETURN false;
    END IF;
  END LOOP;
  RETURN true;
EXCEPTION WHEN invalid_text_representation THEN
  RETURN false;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_refs_valid(text,uuid,jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_constraint_array(p_constraints text)
RETURNS jsonb
LANGUAGE sql
IMMUTABLE
SET search_path = pg_catalog
AS $$
  -- ResearchState permits 1,024 UTF-8 bytes per constraint. PostgreSQL text positions
  -- count characters, so 256 characters also fit when every character uses four bytes.
  SELECT coalesce(jsonb_agg(substring(p_constraints FROM offset_value FOR 256) ORDER BY offset_value), '[]'::jsonb)
  FROM generate_series(1, char_length(p_constraints), 256) offset_value
  WHERE char_length(p_constraints) > 0
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_constraint_array(text) FROM PUBLIC;

-- Counts and answer states only. Research text and values never cross the director role.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_run_outcome(
  p_tenant text, p_campaign uuid, p_step uuid
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE step_row enrichedlab_agent.broker_campaign_steps%ROWTYPE;
DECLARE run_row enrichedlab_agent.broker_research_runs%ROWTYPE;
DECLARE newest jsonb;
DECLARE finding_count integer := 0;
DECLARE refused_count integer := 0;
DECLARE task_count integer := 0;
DECLARE cancelled_count integer := 0;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO step_row FROM enrichedlab_agent.broker_campaign_steps
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND step_id = p_step;
  IF NOT FOUND OR step_row.state <> 'running' THEN RETURN NULL; END IF;
  SELECT * INTO run_row FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = step_row.run_id;
  IF NOT FOUND OR run_row.status NOT IN ('paused','review_required','failed') THEN RETURN NULL; END IF;
  IF run_row.status = 'paused'
     AND enrichedlab_agent.broker_campaign_project_has_active_tasks(p_tenant, step_row.project_id) THEN
    RETURN NULL;
  END IF;
  SELECT turn_record.record->'proposal' INTO newest
  FROM enrichedlab_agent.broker_research_turns turn_record
  WHERE turn_record.tenant_id = p_tenant AND turn_record.run_id = step_row.run_id
    AND jsonb_typeof(turn_record.record->'proposal') = 'object'
  ORDER BY turn_record.turn DESC LIMIT 1;
  IF jsonb_typeof(newest->'findings') = 'array' THEN
    finding_count := jsonb_array_length(newest->'findings');
  END IF;
  SELECT count(*) INTO refused_count
  FROM enrichedlab_agent.broker_research_run_events event_row
  WHERE event_row.tenant_id = p_tenant AND event_row.run_id = step_row.run_id
    AND event_row.code = 'proposal_invalid';
  SELECT count(*), count(*) FILTER (WHERE task.status = 'cancelled')
    INTO task_count, cancelled_count
  FROM enrichedlab_agent.broker_project_tasks project_task
  JOIN enrichedlab_agent.broker_tasks task USING (tenant_id, task_id)
  WHERE project_task.tenant_id = p_tenant AND project_task.project_id = step_row.project_id;
  RETURN jsonb_build_object(
    'answer', newest->>'answer',
    'action', newest->>'action',
    'run_state', run_row.status,
    'findings', finding_count,
    'refused_replies', refused_count,
    'turns', run_row.turn,
    'max_turns', run_row.max_turns,
    'cancelled', task_count > 0 AND task_count = cancelled_count);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_run_outcome(text,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_claim_campaign(
  p_tenant text, p_worker text, p_seconds integer
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE candidate record;
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF p_seconds IS NULL OR p_seconds NOT BETWEEN 30 AND 600 OR p_worker IS NULL
     OR p_worker !~ '^[a-zA-Z0-9_.-]{1,100}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_lease_invalid';
  END IF;
  -- Wall expiry changes state before any campaign can be returned for more planning.
  FOR candidate IN
    SELECT campaign_id FROM enrichedlab_agent.broker_campaigns
    WHERE tenant_id = p_tenant AND state = 'running'
      AND started_at + make_interval(secs => (budgets->>'wall_seconds')::integer) <= clock_timestamp()
    FOR UPDATE SKIP LOCKED
  LOOP
    UPDATE enrichedlab_agent.broker_campaigns
    SET state = 'stopping', state_reason = 'budget_exhausted', generation = generation + 1,
        lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = candidate.campaign_id;
    PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, candidate.campaign_id,
      'budget_exhausted', jsonb_build_object('reason_code', 'wall_seconds'));
  END LOOP;
  -- Settle drained pause/stop requests even when there is no campaign left to claim.
  FOR candidate IN
    SELECT campaign_id FROM enrichedlab_agent.broker_campaigns
    WHERE tenant_id = p_tenant AND state IN ('pausing','stopping')
    ORDER BY updated_at FOR UPDATE SKIP LOCKED
  LOOP
    PERFORM enrichedlab_agent.broker_campaign_finalize_lifecycle(p_tenant, candidate.campaign_id);
  END LOOP;
  SELECT * INTO campaign_row
  FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND state IN ('running','pausing','stopping')
    AND available_at <= clock_timestamp() AND (lease_until IS NULL OR lease_until <= clock_timestamp())
  ORDER BY available_at, created_at
  FOR UPDATE SKIP LOCKED LIMIT 1;
  IF NOT FOUND THEN RETURN NULL; END IF;
  UPDATE enrichedlab_agent.broker_campaigns
  SET generation = generation + 1, lease_owner = p_worker,
      lease_until = clock_timestamp() + make_interval(secs => p_seconds), updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND campaign_id = campaign_row.campaign_id
  RETURNING * INTO campaign_row;
  RETURN enrichedlab_agent.broker_campaign_snapshot(p_tenant, campaign_row.campaign_id, 100);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_claim_campaign(text,text,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_renew_campaign(
  p_tenant text, p_campaign uuid, p_worker text, p_generation bigint, p_seconds integer
)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF p_seconds IS NULL OR p_seconds NOT BETWEEN 30 AND 600 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_lease_invalid';
  END IF;
  SELECT * INTO campaign_row FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign FOR UPDATE;
  IF NOT FOUND OR p_generation IS NULL OR campaign_row.lease_owner IS DISTINCT FROM p_worker
     OR campaign_row.generation <> p_generation OR campaign_row.lease_until <= clock_timestamp()
     OR campaign_row.state NOT IN ('running','pausing','stopping') THEN RETURN false; END IF;
  IF campaign_row.state = 'running'
     AND campaign_row.started_at + make_interval(secs => (campaign_row.budgets->>'wall_seconds')::integer)
         <= clock_timestamp() THEN
    UPDATE enrichedlab_agent.broker_campaigns
    SET state = 'stopping', state_reason = 'budget_exhausted', generation = generation + 1,
        lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign,
      'budget_exhausted', jsonb_build_object('reason_code', 'wall_seconds'));
    RETURN false;
  END IF;
  UPDATE enrichedlab_agent.broker_campaigns
  SET lease_until = clock_timestamp() + make_interval(secs => p_seconds)
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  RETURN true;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_renew_campaign(text,uuid,text,bigint,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_yield_campaign(
  p_tenant text, p_campaign uuid, p_worker text, p_generation bigint
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  UPDATE enrichedlab_agent.broker_campaigns
  SET lease_owner = NULL, lease_until = NULL, available_at = clock_timestamp() + interval '2 seconds',
      updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign
    AND state IN ('running','pausing','stopping') AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_lease_fence_lost';
  END IF;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_yield_campaign(text,uuid,text,bigint) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_director_view(
  p_tenant text, p_campaign uuid, p_worker text, p_generation bigint
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaigns
      WHERE tenant_id = p_tenant AND campaign_id = p_campaign
        AND state IN ('running','pausing','stopping') AND lease_owner = p_worker
        AND generation = p_generation AND lease_until > clock_timestamp()) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_lease_fence_lost';
  END IF;
  RETURN enrichedlab_agent.broker_campaign_snapshot(p_tenant, p_campaign, 100);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_director_view(text,uuid,text,bigint) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_finished_steps(
  p_tenant text, p_campaign uuid, p_worker text, p_generation bigint
)
RETURNS SETOF jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaigns
      WHERE tenant_id = p_tenant AND campaign_id = p_campaign
        AND state IN ('running','pausing','stopping') AND lease_owner = p_worker
        AND generation = p_generation AND lease_until > clock_timestamp()) THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_lease_fence_lost';
  END IF;
  RETURN QUERY
  SELECT jsonb_build_object('step_id', step_row.step_id, 'branch_id', step_row.branch_id,
           'project_id', step_row.project_id, 'run_id', step_row.run_id,
           'outcome', enrichedlab_agent.broker_campaign_run_outcome(p_tenant, p_campaign, step_row.step_id))
  FROM enrichedlab_agent.broker_campaign_steps step_row
  WHERE step_row.tenant_id = p_tenant AND step_row.campaign_id = p_campaign
    AND step_row.state = 'running'
    AND enrichedlab_agent.broker_campaign_run_outcome(p_tenant, p_campaign, step_row.step_id) IS NOT NULL
  ORDER BY step_row.started_at, step_row.step_id;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_finished_steps(text,uuid,text,bigint) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_apply(
  p_tenant text, p_campaign uuid, p_worker text, p_generation bigint, p_change jsonb
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
DECLARE terminal_branch enrichedlab_agent.broker_campaign_branches%ROWTYPE;
DECLARE item jsonb;
DECLARE expected_outcome jsonb;
DECLARE next_record bigint;
DECLARE target_state text;
DECLARE target_reason text;
DECLARE lifecycle text;
DECLARE record_reference record;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO campaign_row FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign FOR UPDATE;
  IF NOT FOUND OR p_generation IS NULL OR campaign_row.state NOT IN ('running','pausing','stopping')
     OR campaign_row.lease_owner IS DISTINCT FROM p_worker
     OR campaign_row.generation <> p_generation OR campaign_row.lease_until <= clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_lease_fence_lost';
  END IF;
  IF jsonb_typeof(p_change) IS DISTINCT FROM 'object' OR octet_length(p_change::text) > 262144
     OR EXISTS (SELECT 1 FROM jsonb_object_keys(p_change) key WHERE key NOT IN
       ('branches','branch_updates','records','record_updates','relations','events','finished_steps',
        'planner_cycle','campaign_state')) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  IF EXISTS (SELECT 1 FROM (VALUES ('branches'),('branch_updates'),('records'),('record_updates'),
      ('relations'),('events'),('finished_steps')) fields(name)
      WHERE p_change ? fields.name AND jsonb_typeof(p_change->fields.name) IS DISTINCT FROM 'array') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  IF campaign_row.state = 'running'
     AND campaign_row.started_at + make_interval(secs => (campaign_row.budgets->>'wall_seconds')::integer)
         <= clock_timestamp() THEN
    UPDATE enrichedlab_agent.broker_campaigns
    SET state = 'stopping', state_reason = 'budget_exhausted', generation = generation + 1,
        lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign,
      'budget_exhausted', jsonb_build_object('reason_code', 'wall_seconds'));
    PERFORM enrichedlab_agent.broker_campaign_finalize_lifecycle(p_tenant, p_campaign);
    RETURN enrichedlab_agent.broker_campaign_snapshot(p_tenant, p_campaign, 100);
  END IF;

  IF jsonb_array_length(coalesce(p_change->'branches','[]'::jsonb)) > 32
     OR (SELECT count(*) FROM enrichedlab_agent.broker_campaign_branches
         WHERE tenant_id = p_tenant AND campaign_id = p_campaign)
        + jsonb_array_length(coalesce(p_change->'branches','[]'::jsonb)) > 500 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_change->'branches','[]'::jsonb)) LOOP
    IF jsonb_typeof(item) IS DISTINCT FROM 'object'
       OR EXISTS (SELECT 1 FROM jsonb_object_keys(item) key
                  WHERE key NOT IN ('branch_id','parent_id','direction','rationale'))
       OR coalesce(item->>'branch_id','') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR (item->>'parent_id' IS NOT NULL AND item->>'parent_id' !~
          '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')
       OR char_length(btrim(coalesce(item->>'direction',''))) NOT BETWEEN 1 AND 2000
       OR char_length(btrim(coalesce(item->>'rationale',''))) NOT BETWEEN 1 AND 2000
       OR item->>'direction' ~ '[[:cntrl:]]' OR item->>'rationale' ~ '[[:cntrl:]]' THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
    IF item->>'parent_id' IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM enrichedlab_agent.broker_campaign_branches
      WHERE tenant_id = p_tenant AND campaign_id = p_campaign
        AND branch_id = (item->>'parent_id')::uuid
    ) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_ref_invalid';
    END IF;
    INSERT INTO enrichedlab_agent.broker_campaign_branches
      (tenant_id, campaign_id, branch_id, parent_id, direction, rationale)
    VALUES (p_tenant, p_campaign, (item->>'branch_id')::uuid, (item->>'parent_id')::uuid,
            item->>'direction', item->>'rationale');
    PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign, 'branch_opened',
      jsonb_build_object('branch_id', item->>'branch_id'));
  END LOOP;

  IF jsonb_array_length(coalesce(p_change->'branch_updates','[]'::jsonb)) > 64 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_change->'branch_updates','[]'::jsonb)) LOOP
    IF jsonb_typeof(item) IS DISTINCT FROM 'object'
       OR EXISTS (SELECT 1 FROM jsonb_object_keys(item) key
                  WHERE key NOT IN ('branch_id','state','reason_class','reason'))
       OR coalesce(item->>'branch_id','') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR item->>'state' NOT IN ('waiting','abandoned','concluded')
       OR coalesce(item->>'reason_class','') !~ '^[a-z][a-z0-9_]{1,63}$'
       OR char_length(btrim(coalesce(item->>'reason',''))) NOT BETWEEN 1 AND 512
       OR item->>'reason' ~ '[[:cntrl:]]' THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
    SELECT * INTO terminal_branch FROM enrichedlab_agent.broker_campaign_branches
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign
      AND branch_id = (item->>'branch_id')::uuid FOR UPDATE;
    IF NOT FOUND THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_ref_invalid';
    END IF;
    IF terminal_branch.state IN ('abandoned','concluded') THEN
      PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign, 'branch_closed',
        jsonb_build_object('branch_id', item->>'branch_id', 'reason_code', terminal_branch.reason_class,
                           'idempotent', true));
      CONTINUE;
    END IF;
    UPDATE enrichedlab_agent.broker_campaign_branches
    SET state = item->>'state', reason_class = item->>'reason_class', reason = item->>'reason',
        updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign
      AND branch_id = (item->>'branch_id')::uuid;
    IF item->>'state' IN ('abandoned','concluded') THEN
      PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign, 'branch_closed',
        jsonb_build_object('branch_id', item->>'branch_id', 'reason_code', item->>'reason_class'));
    END IF;
  END LOOP;

  IF jsonb_array_length(coalesce(p_change->'records','[]'::jsonb)) > 128 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_change->'records','[]'::jsonb)) LOOP
    IF jsonb_typeof(item) IS DISTINCT FROM 'object'
       OR EXISTS (SELECT 1 FROM jsonb_object_keys(item) key
                  WHERE key NOT IN ('record_id','kind','text','origin','status','refs','superseded_by'))
       OR coalesce(item->>'record_id','') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR (item->>'superseded_by' IS NOT NULL AND item->>'superseded_by' !~
           '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')
       OR item->>'kind' NOT IN ('question','hypothesis','attempt','finding','prospect','lesson','request')
       OR item->>'origin' NOT IN ('director','planner')
       OR item->>'status' NOT IN ('open','supported','contradicted','abandoned','withdrawn')
       OR char_length(btrim(coalesce(item->>'text',''))) NOT BETWEEN 1 AND 2000
       OR item->>'text' ~ '[[:cntrl:]]'
       OR jsonb_typeof(item->'refs') IS DISTINCT FROM 'array'
       OR jsonb_array_length(item->'refs') > 64 OR octet_length((item->'refs')::text) > 8192 THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
    UPDATE enrichedlab_agent.broker_campaigns SET record_sequence = record_sequence + 1
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign
    RETURNING record_sequence INTO next_record;
    INSERT INTO enrichedlab_agent.broker_campaign_records
      (tenant_id, campaign_id, record_id, sequence, kind, text, origin, status, refs)
    VALUES (p_tenant, p_campaign, (item->>'record_id')::uuid, next_record, item->>'kind',
            item->>'text', item->>'origin', item->>'status', item->'refs');
  END LOOP;
  -- Validate after all proposed records exist, so records in one atomic change may refer to each other.
  FOR record_reference IN
    SELECT record_id, refs FROM enrichedlab_agent.broker_campaign_records
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign
      AND record_id IN (SELECT (value->>'record_id')::uuid
                        FROM jsonb_array_elements(coalesce(p_change->'records','[]'::jsonb)))
  LOOP
    IF NOT enrichedlab_agent.broker_campaign_refs_valid(p_tenant, p_campaign, record_reference.refs) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_ref_invalid';
    END IF;
  END LOOP;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_change->'records','[]'::jsonb)) LOOP
    IF item->>'superseded_by' IS NOT NULL THEN
      UPDATE enrichedlab_agent.broker_campaign_records
      SET superseded_by = (item->>'superseded_by')::uuid, status = 'withdrawn', updated_at = clock_timestamp()
      WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND record_id = (item->>'record_id')::uuid
        AND EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_records target
                    WHERE target.tenant_id = p_tenant AND target.campaign_id = p_campaign
                      AND target.record_id = (item->>'superseded_by')::uuid);
      IF NOT FOUND THEN
        RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_ref_invalid';
      END IF;
    END IF;
  END LOOP;

  IF jsonb_array_length(coalesce(p_change->'record_updates','[]'::jsonb)) > 128 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_change->'record_updates','[]'::jsonb)) LOOP
    IF jsonb_typeof(item) IS DISTINCT FROM 'object'
       OR EXISTS (SELECT 1 FROM jsonb_object_keys(item) key
                  WHERE key NOT IN ('record_id','status','superseded_by'))
       OR coalesce(item->>'record_id','') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR item->>'status' NOT IN ('open','supported','contradicted','abandoned','withdrawn')
       OR (item->>'superseded_by' IS NOT NULL AND item->>'superseded_by' !~
           '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$') THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
    UPDATE enrichedlab_agent.broker_campaign_records
    SET status = item->>'status', superseded_by = (item->>'superseded_by')::uuid,
        updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND record_id = (item->>'record_id')::uuid
      AND (item->>'superseded_by' IS NULL OR EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_campaign_records target
        WHERE target.tenant_id = p_tenant AND target.campaign_id = p_campaign
          AND target.record_id = (item->>'superseded_by')::uuid));
    IF NOT FOUND THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_ref_invalid';
    END IF;
  END LOOP;

  IF jsonb_array_length(coalesce(p_change->'relations','[]'::jsonb)) > 256 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_change->'relations','[]'::jsonb)) LOOP
    IF jsonb_typeof(item) IS DISTINCT FROM 'object'
       OR EXISTS (SELECT 1 FROM jsonb_object_keys(item) key
                  WHERE key NOT IN ('relation_id','kind','source_record','target_record'))
       OR coalesce(item->>'relation_id','') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR coalesce(item->>'source_record','') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR coalesce(item->>'target_record','') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR item->>'kind' NOT IN ('supports','contradicts','tested_by','retry_of','derived_from',
                                'cites_evidence','answers')
       OR item->>'source_record' = item->>'target_record' THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
    INSERT INTO enrichedlab_agent.broker_campaign_relations
      (tenant_id, campaign_id, relation_id, kind, source_record, target_record)
    VALUES (p_tenant, p_campaign, (item->>'relation_id')::uuid, item->>'kind',
            (item->>'source_record')::uuid, (item->>'target_record')::uuid);
  END LOOP;

  IF jsonb_array_length(coalesce(p_change->'finished_steps','[]'::jsonb)) > 32 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_change->'finished_steps','[]'::jsonb)) LOOP
    IF jsonb_typeof(item) IS DISTINCT FROM 'object'
       OR EXISTS (SELECT 1 FROM jsonb_object_keys(item) key
                  WHERE key NOT IN ('step_id','outcome_class','outcome'))
       OR coalesce(item->>'step_id','') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       OR item->>'outcome_class' NOT IN ('evidence','no_answer','needs_person','stalled','failed','cancelled') THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
    expected_outcome := enrichedlab_agent.broker_campaign_run_outcome(
      p_tenant, p_campaign, (item->>'step_id')::uuid);
    IF expected_outcome IS NULL OR expected_outcome <> item->'outcome' THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_outcome_invalid';
    END IF;
    UPDATE enrichedlab_agent.broker_campaign_steps
    SET state = 'finished', outcome_class = item->>'outcome_class', outcome = item->'outcome',
        finished_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign
      AND step_id = (item->>'step_id')::uuid AND state = 'running';
    IF NOT FOUND THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_outcome_invalid';
    END IF;
    UPDATE enrichedlab_agent.broker_campaign_branches branch_row
    SET evidence_count = evidence_count + CASE WHEN item->>'outcome_class' = 'evidence' THEN 1 ELSE 0 END,
        no_answer_count = no_answer_count + CASE WHEN item->>'outcome_class' = 'no_answer' THEN 1 ELSE 0 END,
        needs_person_count = needs_person_count + CASE WHEN item->>'outcome_class' = 'needs_person' THEN 1 ELSE 0 END,
        stalled_count = stalled_count + CASE WHEN item->>'outcome_class' = 'stalled' THEN 1 ELSE 0 END,
        failed_count = failed_count + CASE WHEN item->>'outcome_class' = 'failed' THEN 1 ELSE 0 END,
        cancelled_count = cancelled_count + CASE WHEN item->>'outcome_class' = 'cancelled' THEN 1 ELSE 0 END,
        consecutive_without_evidence = CASE WHEN item->>'outcome_class' = 'evidence'
          THEN 0 ELSE consecutive_without_evidence + 1 END,
        updated_at = clock_timestamp()
    FROM enrichedlab_agent.broker_campaign_steps step_row
    WHERE step_row.tenant_id = p_tenant AND step_row.campaign_id = p_campaign
      AND step_row.step_id = (item->>'step_id')::uuid
      AND branch_row.tenant_id = step_row.tenant_id AND branch_row.campaign_id = step_row.campaign_id
      AND branch_row.branch_id = step_row.branch_id;
    UPDATE enrichedlab_agent.broker_campaigns
    SET active_runs = greatest(active_runs - 1, 0), finished_steps = finished_steps + 1,
        evidence_steps = evidence_steps + CASE WHEN item->>'outcome_class' = 'evidence' THEN 1 ELSE 0 END,
        consecutive_without_evidence = CASE WHEN item->>'outcome_class' = 'evidence'
          THEN 0 ELSE consecutive_without_evidence + 1 END,
        updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign, 'step_finished',
      jsonb_build_object('step_id', item->>'step_id', 'outcome_class', item->>'outcome_class'));
  END LOOP;

  IF p_change ? 'planner_cycle' THEN
    IF p_change->>'planner_cycle' = 'refused' THEN
      UPDATE enrichedlab_agent.broker_campaigns
      SET planner_refused_cycles = planner_refused_cycles + 1
      WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    ELSIF p_change->>'planner_cycle' = 'accepted' THEN
      UPDATE enrichedlab_agent.broker_campaigns SET planner_refused_cycles = 0
      WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    ELSE
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
  END IF;

  IF jsonb_array_length(coalesce(p_change->'events','[]'::jsonb)) > 128 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(p_change->'events','[]'::jsonb)) LOOP
    IF jsonb_typeof(item) IS DISTINCT FROM 'object'
       OR EXISTS (SELECT 1 FROM jsonb_object_keys(item) key WHERE key NOT IN ('kind','detail'))
       OR item->>'kind' NOT IN ('rule_fired','proposal_refused') THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
    PERFORM enrichedlab_agent.broker_campaign_event(
      p_tenant, p_campaign, item->>'kind', coalesce(item->'detail','{}'::jsonb));
  END LOOP;

  IF p_change ? 'campaign_state' THEN
    IF jsonb_typeof(p_change->'campaign_state') IS DISTINCT FROM 'object'
       OR EXISTS (SELECT 1 FROM jsonb_object_keys(p_change->'campaign_state') key
                  WHERE key NOT IN ('state','reason')) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
    END IF;
    target_state := p_change->'campaign_state'->>'state';
    target_reason := p_change->'campaign_state'->>'reason';
    IF target_state NOT IN ('pausing','stopping','finished','failed')
       OR coalesce(target_reason, '') !~ '^[a-z][a-z0-9_]{1,63}$'
       OR (target_state = 'pausing' AND campaign_row.state <> 'running')
       OR (target_state = 'stopping' AND campaign_row.state NOT IN ('running','pausing','stopping'))
       OR (target_state IN ('finished','failed') AND campaign_row.state <> 'running') THEN
      RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_state_conflict';
    END IF;
    IF target_state IN ('finished','failed') AND (
      EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_steps
        WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND state = 'running')
      OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_steps step_row
        WHERE step_row.tenant_id = p_tenant AND step_row.campaign_id = p_campaign
          AND step_row.project_id IS NOT NULL
          AND enrichedlab_agent.broker_campaign_project_has_active_tasks(
            step_row.tenant_id, step_row.project_id))
    ) THEN
      RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_state_conflict';
    END IF;
    IF target_state = 'finished' AND (
      NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_branches
                     WHERE tenant_id = p_tenant AND campaign_id = p_campaign)
      OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_branches
                 WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND state IN ('open','waiting'))
    ) THEN
      RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_state_conflict';
    END IF;
    UPDATE enrichedlab_agent.broker_campaigns
    SET state = target_state, state_reason = target_reason,
        generation = CASE WHEN target_state IN ('pausing','stopping','finished','failed')
                          THEN generation + 1 ELSE generation END,
        lease_owner = NULL, lease_until = NULL,
        finished_at = CASE WHEN target_state IN ('finished','failed') THEN clock_timestamp() ELSE finished_at END,
        updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    IF target_state = 'finished' THEN
      PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign, 'finished',
        jsonb_build_object('reason_code', target_reason));
    ELSIF target_state = 'pausing' THEN
      PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign, 'paused',
        jsonb_build_object('state', 'pausing', 'reason_code', target_reason));
    ELSIF target_state = 'stopping' AND target_reason = 'budget_exhausted' THEN
      PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign, 'budget_exhausted',
        jsonb_build_object('reason_code', 'max_runs'));
    END IF;
    IF target_state IN ('finished','failed') THEN
      UPDATE enrichedlab_agent.broker_principals SET enabled = false, updated_at = clock_timestamp()
      WHERE tenant_id = p_tenant AND principal_id = campaign_row.campaign_principal;
      DELETE FROM enrichedlab_agent.broker_group_memberships
      WHERE tenant_id = p_tenant AND principal_id = campaign_row.campaign_principal;
    ELSIF target_state IN ('pausing','stopping') THEN
      lifecycle := enrichedlab_agent.broker_campaign_finalize_lifecycle(p_tenant, p_campaign);
    END IF;
  END IF;
  RETURN enrichedlab_agent.broker_campaign_snapshot(p_tenant, p_campaign, 100);
EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range OR check_violation
    OR not_null_violation OR foreign_key_violation OR unique_violation THEN
  RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_change_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_apply(text,uuid,text,bigint,jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_start_campaign_step(
  p_tenant text,
  p_campaign uuid,
  p_worker text,
  p_generation bigint,
  p_step uuid,
  p_branch uuid,
  p_purpose text,
  p_question text,
  p_profiles text[],
  p_project uuid,
  p_run uuid
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row enrichedlab_agent.broker_campaigns%ROWTYPE;
DECLARE branch_row enrichedlab_agent.broker_campaign_branches%ROWTYPE;
DECLARE previous_step enrichedlab_agent.broker_campaign_steps%ROWTYPE;
DECLARE normalized text;
DECLARE max_runs integer;
DECLARE max_concurrent integer;
DECLARE max_turns integer;
DECLARE checkpoint jsonb;
DECLARE sorted_profiles text[];
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  SELECT * INTO campaign_row FROM enrichedlab_agent.broker_campaigns
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign FOR UPDATE;
  IF NOT FOUND OR p_generation IS NULL OR campaign_row.lease_owner IS DISTINCT FROM p_worker
     OR campaign_row.generation <> p_generation OR campaign_row.lease_until <= clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_lease_fence_lost';
  END IF;
  IF campaign_row.state <> 'running' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:campaign_not_running';
  END IF;
  IF campaign_row.started_at + make_interval(secs => (campaign_row.budgets->>'wall_seconds')::integer)
       <= clock_timestamp() THEN
    UPDATE enrichedlab_agent.broker_campaigns
    SET state = 'stopping', state_reason = 'budget_exhausted', generation = generation + 1,
        lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign,
      'budget_exhausted', jsonb_build_object('reason_code', 'wall_seconds'));
    RETURN jsonb_build_object('started', false, 'code', 'campaign_budget_exhausted', 'state', 'stopping');
  END IF;
  max_runs := (campaign_row.budgets->>'max_runs')::integer;
  max_concurrent := (campaign_row.budgets->>'max_concurrent_runs')::integer;
  max_turns := (campaign_row.budgets->>'max_turns_per_run')::integer;
  -- A spent total budget is permanent, so it takes precedence when both limits apply.
  -- Capacity refusals are normal decisions: callers receive started=false instead of an exception.
  IF campaign_row.run_count >= max_runs THEN
    UPDATE enrichedlab_agent.broker_campaigns
    SET state = 'stopping', state_reason = 'budget_exhausted', generation = generation + 1,
        lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
    PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign,
      'budget_exhausted', jsonb_build_object('reason_code', 'max_runs'));
    RETURN jsonb_build_object('started', false, 'code', 'campaign_budget_exhausted', 'state', 'stopping');
  END IF;
  IF campaign_row.active_runs >= max_concurrent THEN
    RETURN jsonb_build_object('started', false, 'code', 'campaign_concurrency_exhausted', 'state', 'running');
  END IF;
  IF p_step IS NULL OR p_branch IS NULL OR p_project IS NULL OR p_run IS NULL
     OR p_purpose IS NULL OR p_question IS NULL OR p_profiles IS NULL
     OR p_purpose NOT IN ('explore','test','replicate','verify','literature','synthesize')
     OR char_length(btrim(p_question)) NOT BETWEEN 1 AND 2000 OR p_question ~ '[[:cntrl:]]'
     OR cardinality(p_profiles) NOT BETWEEN 1 AND 16
     OR EXISTS (SELECT 1 FROM unnest(p_profiles) AS proposed_profile(value)
                WHERE proposed_profile.value IS NULL
                   OR proposed_profile.value !~ '^[a-z][a-z0-9_-]{1,63}$')
     OR (SELECT count(*) <> count(DISTINCT proposed_profile.value)
         FROM unnest(p_profiles) AS proposed_profile(value)) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_step_invalid';
  END IF;
  IF NOT enrichedlab_agent.broker_campaign_scope_valid(p_tenant, campaign_row.group_key, campaign_row.scope)
     OR EXISTS (
       SELECT 1 FROM unnest(p_profiles) AS proposed_profile(value)
       WHERE NOT (campaign_row.scope->'profiles') ? proposed_profile.value
          OR NOT EXISTS (
            SELECT 1 FROM enrichedlab_agent.broker_group_profile_grants grant_row
            JOIN enrichedlab_agent.broker_agent_profiles profile
              ON profile.tenant_id = grant_row.tenant_id AND profile.profile_key = grant_row.profile_key
            WHERE grant_row.tenant_id = p_tenant AND grant_row.group_key = campaign_row.group_key
              AND grant_row.profile_key = proposed_profile.value AND profile.enabled
              AND NOT enrichedlab_agent.broker_run_excluded_route(profile.route)
          )
     ) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:campaign_scope_forbidden';
  END IF;
  SELECT * INTO branch_row FROM enrichedlab_agent.broker_campaign_branches
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND branch_id = p_branch FOR UPDATE;
  IF NOT FOUND OR branch_row.state NOT IN ('open','waiting') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_ref_invalid';
  END IF;
  normalized := enrichedlab_agent.broker_campaign_normalize_question(p_question);
  IF normalized = '' OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_steps
      WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND normalized_question = normalized) THEN
    RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:campaign_question_duplicate';
  END IF;
  SELECT array_agg(proposed_profile.value ORDER BY proposed_profile.value) INTO sorted_profiles
  FROM unnest(p_profiles) AS proposed_profile(value);
  IF branch_row.state = 'waiting' THEN
    SELECT * INTO previous_step FROM enrichedlab_agent.broker_campaign_steps
    WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND branch_id = p_branch
      AND outcome_class IS NOT NULL AND outcome_class <> 'evidence'
    ORDER BY finished_at DESC, step_id DESC LIMIT 1;
    IF FOUND AND (previous_step.normalized_question = normalized
       OR (SELECT array_agg(previous_profile.value ORDER BY previous_profile.value)
           FROM unnest(previous_step.profiles) AS previous_profile(value))
          = sorted_profiles) THEN
      RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_approach_repeated';
    END IF;
  END IF;
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_campaign_steps
      WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND step_id = p_step)
     OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_projects
                WHERE tenant_id = p_tenant AND project_id = p_project)
     OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_research_runs
                WHERE tenant_id = p_tenant AND run_id = p_run) THEN
    RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
  END IF;

  INSERT INTO enrichedlab_agent.broker_projects
    (tenant_id, project_id, group_key, owner_id, title, visibility)
  VALUES (p_tenant, p_project, campaign_row.group_key, campaign_row.campaign_principal,
          substring(btrim(p_question) FROM 1 FOR 160), 'private');
  INSERT INTO enrichedlab_agent.broker_project_grants
    (tenant_id, project_id, principal_id, capabilities)
  VALUES (p_tenant, p_project, campaign_row.started_by, ARRAY['read']::text[]);
  checkpoint := jsonb_build_object(
    'schema_version', 1,
    'goal', p_question,
    'constraints', enrichedlab_agent.broker_campaign_constraint_array(campaign_row.constraints),
    'plan', '[]'::jsonb,
    'lessons', '[]'::jsonb,
    'next_action', 'Plan the requested analysis using the selected data and approved methods.',
    'workspace', campaign_row.scope->'workspace');
  INSERT INTO enrichedlab_agent.broker_project_checkpoints
    (tenant_id, project_id, revision, author_id, request_id, state)
  VALUES (p_tenant, p_project, 1, campaign_row.campaign_principal, p_step, checkpoint);
  UPDATE enrichedlab_agent.broker_projects SET revision = 1, updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND project_id = p_project;
  PERFORM enrichedlab_agent.broker_start_research_run(
    p_tenant, campaign_row.campaign_principal, p_project, p_run, 1,
    p_profiles, max_turns, max_concurrent);
  INSERT INTO enrichedlab_agent.broker_campaign_steps
    (tenant_id, campaign_id, step_id, branch_id, purpose, question, normalized_question,
     profiles, project_id, run_id, checkpoint_revision, state, started_at)
  VALUES (p_tenant, p_campaign, p_step, p_branch, p_purpose, p_question, normalized,
          p_profiles, p_project, p_run, 1, 'running', clock_timestamp());
  UPDATE enrichedlab_agent.broker_campaign_branches
  SET state = 'open', reason_class = NULL, reason = NULL, step_count = step_count + 1,
      updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign AND branch_id = p_branch;
  UPDATE enrichedlab_agent.broker_campaigns
  SET run_count = run_count + 1, active_runs = active_runs + 1, planner_refused_cycles = 0,
      updated_at = clock_timestamp()
  WHERE tenant_id = p_tenant AND campaign_id = p_campaign;
  PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, p_campaign, 'step_started',
    jsonb_build_object('step_id', p_step, 'branch_id', p_branch, 'project_id', p_project,
                       'run_id', p_run, 'purpose', p_purpose));
  RETURN jsonb_build_object('started', true, 'step_id', p_step, 'project_id', p_project, 'run_id', p_run);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_start_campaign_step(text,uuid,text,bigint,uuid,uuid,text,text,text[],uuid,uuid) FROM PUBLIC;

-- Operator kill switch. It is intentionally granted to no runtime role.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_stop_all_campaigns(p_tenant text, p_reason text)
RETURNS integer
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE campaign_row record;
DECLARE changed integer := 0;
BEGIN
  PERFORM set_config('enrichedlab.tenant_id', p_tenant, true);
  IF p_reason IS NULL OR p_reason !~ '^[a-z][a-z0-9_]{1,63}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:campaign_stop_reason_invalid';
  END IF;
  FOR campaign_row IN
    SELECT campaign_id FROM enrichedlab_agent.broker_campaigns
    WHERE tenant_id = p_tenant AND state IN ('running','pausing','paused')
    ORDER BY campaign_id FOR UPDATE
  LOOP
    UPDATE enrichedlab_agent.broker_campaigns
    SET state = 'stopping', state_reason = p_reason, generation = generation + 1,
        lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
    WHERE tenant_id = p_tenant AND campaign_id = campaign_row.campaign_id;
    PERFORM enrichedlab_agent.broker_campaign_event(p_tenant, campaign_row.campaign_id,
      'stop_all', jsonb_build_object('reason_code', p_reason));
    PERFORM enrichedlab_agent.broker_campaign_finalize_lifecycle(p_tenant, campaign_row.campaign_id);
    changed := changed + 1;
  END LOOP;
  RETURN changed;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_stop_all_campaigns(text,text) FROM PUBLIC;

-- A campaign director login is a function-only capability. This predicate is used by
-- both provisioning (for the named target role) and startup (for session_user).
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
    'enrichedlab_agent.broker_start_campaign_step(text,uuid,text,bigint,uuid,uuid,text,text,text[],uuid,uuid)'
  ]::text[]) entry(signature);
  IF cardinality(allowed_functions) <> 9 OR array_position(allowed_functions, NULL) IS NOT NULL OR EXISTS (
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
  -- No direct relation, sequence, or column ACL is ever part of the role contract.
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_class relation
      CROSS JOIN LATERAL aclexplode(relation.relacl) privilege
      WHERE privilege.grantee = role_oid)
     OR EXISTS (SELECT 1 FROM pg_catalog.pg_attribute attribute
      CROSS JOIN LATERAL aclexplode(attribute.attacl) privilege
      WHERE privilege.grantee = role_oid) THEN
    RETURN false;
  END IF;
  -- The one permitted direct schema ACL is non-grantable USAGE on the API schema.
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_namespace namespace
      CROSS JOIN LATERAL aclexplode(namespace.nspacl) privilege
      WHERE privilege.grantee = role_oid
        AND (namespace.nspname <> 'enrichedlab_agent' OR privilege.privilege_type <> 'USAGE'
          OR privilege.is_grantable)) THEN
    RETURN false;
  END IF;
  -- Direct and effective function authority is limited to the readiness and
  -- generation-fenced campaign functions listed above.
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
  -- Check effective privileges over every broker relation as well as direct ACLs;
  -- this also catches authority inherited through PUBLIC or a later role grant.
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

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_campaign_director_ready()
RETURNS boolean
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT enrichedlab_agent.broker_campaign_director_role_safe(session_user::name)
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_campaign_director_ready() FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = 2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (66) ON CONFLICT DO NOTHING;
COMMIT;
