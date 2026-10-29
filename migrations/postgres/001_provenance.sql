CREATE SCHEMA IF NOT EXISTS enrichedlab_agent;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.schema_migrations (
  version bigint PRIMARY KEY,
  applied_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.provenance_runs (
  tenant_id text NOT NULL,
  run_id text NOT NULL,
  subject text NOT NULL,
  status text NOT NULL DEFAULT 'active',
  created_at timestamptz NOT NULL,
  updated_at timestamptz NOT NULL,
  PRIMARY KEY (tenant_id, run_id)
);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.provenance_events (
  tenant_id text NOT NULL,
  run_id text NOT NULL,
  sequence bigint NOT NULL,
  event_id uuid NOT NULL,
  subject text NOT NULL,
  segment bigint NOT NULL,
  harness text NOT NULL,
  provider text,
  model text,
  engine text,
  parser text,
  session_id text,
  kind text NOT NULL,
  evidence_role text NOT NULL,
  visibility text NOT NULL,
  actor text NOT NULL,
  occurred_at text NOT NULL,
  source_event_id text,
  parent_source_event_id text,
  metadata jsonb NOT NULL,
  restricted_content text,
  content_sha256 text,
  previous_hash text NOT NULL,
  hash text NOT NULL,
  PRIMARY KEY (tenant_id, run_id, sequence),
  UNIQUE (tenant_id, event_id),
  UNIQUE (tenant_id, run_id, hash),
  FOREIGN KEY (tenant_id, run_id)
    REFERENCES enrichedlab_agent.provenance_runs (tenant_id, run_id)
);

CREATE INDEX IF NOT EXISTS provenance_events_run_time
  ON enrichedlab_agent.provenance_events (tenant_id, run_id, occurred_at, sequence);
CREATE INDEX IF NOT EXISTS provenance_events_kind
  ON enrichedlab_agent.provenance_events (tenant_id, kind, occurred_at DESC);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.evidence_leases (
  tenant_id text NOT NULL,
  lease_id uuid NOT NULL,
  run_id text NOT NULL,
  token_sha256 text NOT NULL,
  actor text NOT NULL,
  purpose text NOT NULL,
  issued_at timestamptz NOT NULL,
  expires_at timestamptz NOT NULL,
  use_count bigint NOT NULL DEFAULT 0,
  last_used_at timestamptz,
  PRIMARY KEY (tenant_id, lease_id),
  UNIQUE (tenant_id, token_sha256),
  FOREIGN KEY (tenant_id, run_id)
    REFERENCES enrichedlab_agent.provenance_runs (tenant_id, run_id)
);

ALTER TABLE enrichedlab_agent.provenance_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.provenance_runs FORCE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.provenance_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.provenance_events FORCE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.evidence_leases ENABLE ROW LEVEL SECURITY;
ALTER TABLE enrichedlab_agent.evidence_leases FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS provenance_runs_tenant ON enrichedlab_agent.provenance_runs;
CREATE POLICY provenance_runs_tenant ON enrichedlab_agent.provenance_runs
  USING (tenant_id = current_setting('enrichedlab.tenant_id', true))
  WITH CHECK (tenant_id = current_setting('enrichedlab.tenant_id', true));

DROP POLICY IF EXISTS provenance_events_tenant ON enrichedlab_agent.provenance_events;
CREATE POLICY provenance_events_tenant ON enrichedlab_agent.provenance_events
  USING (tenant_id = current_setting('enrichedlab.tenant_id', true))
  WITH CHECK (tenant_id = current_setting('enrichedlab.tenant_id', true));

DROP POLICY IF EXISTS evidence_leases_tenant ON enrichedlab_agent.evidence_leases;
CREATE POLICY evidence_leases_tenant ON enrichedlab_agent.evidence_leases
  USING (tenant_id = current_setting('enrichedlab.tenant_id', true))
  WITH CHECK (tenant_id = current_setting('enrichedlab.tenant_id', true));

CREATE OR REPLACE FUNCTION enrichedlab_agent.reject_provenance_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  RAISE EXCEPTION 'provenance events are append-only';
END;
$$;

DROP TRIGGER IF EXISTS provenance_events_no_update ON enrichedlab_agent.provenance_events;
CREATE TRIGGER provenance_events_no_update
  BEFORE UPDATE ON enrichedlab_agent.provenance_events
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_provenance_mutation();

DROP TRIGGER IF EXISTS provenance_events_no_delete ON enrichedlab_agent.provenance_events;
CREATE TRIGGER provenance_events_no_delete
  BEFORE DELETE ON enrichedlab_agent.provenance_events
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_provenance_mutation();

INSERT INTO enrichedlab_agent.schema_migrations (version)
VALUES (1)
ON CONFLICT (version) DO NOTHING;
