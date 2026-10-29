-- What changed, for live views, each lab's dataset signal, and kept reasoning.
--
-- Every write that a workspace view shows adds one row to `broker_changes` in the same transaction: the keys a reader
-- should read again (`projects`, `project:<id>`, `tasks`, `task:<id>`, `dataset:<id>`, `lab:<key>`, `inbox`), the ids
-- that decide who may see the row, and the writing transaction's id. No title, text or value is copied. The trigger
-- notifies channel `broker_changes` with the tenant; PostgreSQL sends one notification per transaction and tenant, at
-- commit. A reader resumes from a cursor (transaction id, row id) and reads only rows whose transaction is older than
-- every transaction still running (`pg_snapshot_xmin`), so a row that commits late is never skipped. Rows are pruned
-- after a day; a cursor behind the pruned horizon reads `behind` and the reader reads everything again. A write
-- transaction left open anywhere in the database cluster holds delivery back until it ends; it never loses a row.
--
-- `broker_get_group_lab_data` answers, for each lab of the caller, whether exactly one enabled `server-dataset` method
-- is granted to it: the rule `broker_dataset_method` (migration 059) applies when lab data is added.
--
-- Each model call of a research turn may keep its reasoning text beside its reply (`reasoning_sha256` in its call
-- record, and `lead_reasoning_sha256` on a refused reply's event), readable by the same people as the reply.
--
-- Rollback: an older API never reads the feed, and an older coordinator never names reasoning. The triggers only add
-- rows; dropping them and the table stops the feed.
BEGIN;
-- A busy table makes this migration fail, to be run again as it is, instead of holding live writes back: every lock
-- is taken here, before any change, and each is waited on for 2 s at most (the services wait 5 s). The column below
-- needs the strongest lock, so it is taken first.
SET LOCAL lock_timeout = '2s';
LOCK TABLE enrichedlab_agent.broker_research_run_events IN ACCESS EXCLUSIVE MODE;
LOCK TABLE enrichedlab_agent.broker_agent_profiles, enrichedlab_agent.broker_analysis_catalogs,
  enrichedlab_agent.broker_analysis_receipts, enrichedlab_agent.broker_analysis_requests,
  enrichedlab_agent.broker_artifact_digests, enrichedlab_agent.broker_artifact_kinds,
  enrichedlab_agent.broker_artifacts, enrichedlab_agent.broker_attempt_executions,
  enrichedlab_agent.broker_attempt_inputs, enrichedlab_agent.broker_attempt_outputs,
  enrichedlab_agent.broker_dataset_receipts, enrichedlab_agent.broker_dataset_requests,
  enrichedlab_agent.broker_datasets, enrichedlab_agent.broker_figure_manuscript_events,
  enrichedlab_agent.broker_figure_receipts, enrichedlab_agent.broker_figure_requests,
  enrichedlab_agent.broker_group_memberships, enrichedlab_agent.broker_group_profile_grants,
  enrichedlab_agent.broker_inbox_acknowledgements, enrichedlab_agent.broker_lab_note_events,
  enrichedlab_agent.broker_lab_notes, enrichedlab_agent.broker_library_label_events,
  enrichedlab_agent.broker_library_paper_events, enrichedlab_agent.broker_library_papers,
  enrichedlab_agent.broker_plan_events, enrichedlab_agent.broker_plan_pages, enrichedlab_agent.broker_plan_revisions,
  enrichedlab_agent.broker_principals, enrichedlab_agent.broker_project_administrators,
  enrichedlab_agent.broker_project_audit, enrichedlab_agent.broker_project_checkpoints,
  enrichedlab_agent.broker_project_grants, enrichedlab_agent.broker_project_inspections,
  enrichedlab_agent.broker_project_looks, enrichedlab_agent.broker_project_tasks, enrichedlab_agent.broker_projects,
  enrichedlab_agent.broker_query_receipts, enrichedlab_agent.broker_render_catalogs,
  enrichedlab_agent.broker_research_groups, enrichedlab_agent.broker_research_runs,
  enrichedlab_agent.broker_research_turns, enrichedlab_agent.broker_task_events,
  enrichedlab_agent.broker_task_output_closures, enrichedlab_agent.broker_task_platform_tool_jobs,
  enrichedlab_agent.broker_task_reviews, enrichedlab_agent.broker_task_scientific_evidence,
  enrichedlab_agent.broker_task_scientific_history, enrichedlab_agent.broker_task_working_copies,
  enrichedlab_agent.broker_tasks IN SHARE ROW EXCLUSIVE MODE;

-- Each task write asks which shared notes cite the task; this index answers without reading every note.
CREATE INDEX IF NOT EXISTS broker_lab_notes_evidence ON enrichedlab_agent.broker_lab_notes
  USING gin (evidence jsonb_path_ops);

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_changes (
  tenant_id text NOT NULL,
  change_id bigint GENERATED ALWAYS AS IDENTITY,
  txid xid8 NOT NULL DEFAULT pg_current_xact_id(),
  at timestamptz NOT NULL DEFAULT clock_timestamp(),
  kind text NOT NULL,
  project_id uuid,
  task_id uuid,
  dataset_id uuid,
  group_key text,
  principal_id uuid,
  keys text[] NOT NULL,
  PRIMARY KEY (change_id),
  CHECK (kind ~ '^[a-z][a-z_]{1,31}$'),
  CHECK (cardinality(keys) BETWEEN 1 AND 8)
);
CREATE INDEX IF NOT EXISTS broker_changes_cursor ON enrichedlab_agent.broker_changes (tenant_id, txid, change_id);
CREATE INDEX IF NOT EXISTS broker_changes_at ON enrichedlab_agent.broker_changes (at);
ALTER TABLE enrichedlab_agent.broker_changes ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_changes FROM PUBLIC;

-- The highest cursor pruned per tenant: a reader whose cursor is below it may have missed rows.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_change_horizons (
  tenant_id text PRIMARY KEY,
  txid xid8 NOT NULL,
  change_id bigint NOT NULL,
  pruned_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
ALTER TABLE enrichedlab_agent.broker_change_horizons ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_change_horizons FROM PUBLIC;

-- One row of the feed. Keys are built here so readers never need to know the tables.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_add_change(p_tenant text, p_kind text, p_project uuid, p_task uuid,
  p_dataset uuid, p_group text, p_principal uuid, p_inbox boolean, p_extra text[])
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  k text[] := '{}';
  grp text := p_group;
BEGIN
  IF p_project IS NOT NULL THEN
    k := k || ARRAY['projects', 'project:' || p_project::text];
    IF grp IS NULL THEN
      SELECT group_key INTO grp FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = p_project;
    END IF;
  END IF;
  IF p_task IS NOT NULL THEN
    k := k || ARRAY['tasks', 'task:' || p_task::text];
  END IF;
  IF p_dataset IS NOT NULL THEN
    k := k || ('dataset:' || p_dataset::text);
  END IF;
  IF p_project IS NULL AND p_task IS NULL AND grp IS NOT NULL THEN
    k := k || ('lab:' || grp);
  END IF;
  IF p_inbox THEN
    k := k || 'inbox'::text;
  END IF;
  k := k || coalesce(p_extra, '{}');
  SELECT coalesce(array_agg(DISTINCT x ORDER BY x), '{}') INTO k FROM unnest(k) AS x WHERE x IS NOT NULL;
  IF cardinality(k) = 0 THEN
    RETURN;
  END IF;
  k := k[1:8];
  -- A transaction that writes many rows of one kind (an analysis's files) adds one change, not one per row; its
  -- readers read after it commits, so a second identical row tells them nothing more.
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_changes c
             WHERE c.tenant_id = p_tenant AND c.txid = pg_current_xact_id() AND c.kind = p_kind
               AND c.project_id IS NOT DISTINCT FROM p_project AND c.task_id IS NOT DISTINCT FROM p_task
               AND c.dataset_id IS NOT DISTINCT FROM p_dataset AND c.group_key IS NOT DISTINCT FROM grp
               AND c.principal_id IS NOT DISTINCT FROM p_principal AND c.keys = k) THEN
    RETURN;
  END IF;
  INSERT INTO enrichedlab_agent.broker_changes (tenant_id, kind, project_id, task_id, dataset_id, group_key, principal_id, keys)
  VALUES (p_tenant, p_kind, p_project, p_task, p_dataset, grp, p_principal, k);
  PERFORM pg_notify('broker_changes', p_tenant);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_add_change(text, text, uuid, uuid, uuid, text, uuid, boolean, text[]) FROM PUBLIC;

-- The project a task belongs to, if any, and the analysis it was made from: a figure version or template analysis names
-- its source, so the source's page reads again.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_change_task_project(p_tenant text, p_task uuid)
RETURNS TABLE(project_id uuid, source_task_id uuid) LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT pt.project_id,
    coalesce(pt.parent_task_id,
      (SELECT f.source_task_id FROM enrichedlab_agent.broker_figure_requests f WHERE f.tenant_id = p_tenant AND f.task_id = p_task),
      (SELECT a.source_task_id FROM enrichedlab_agent.broker_analysis_requests a WHERE a.tenant_id = p_tenant AND a.task_id = p_task))
  FROM (SELECT 1) one
  LEFT JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = p_tenant AND pt.task_id = p_task
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_change_task_project(text, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_note_change()
RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  t text := TG_TABLE_NAME;
  tenant text;
  proj uuid;
  task uuid;
  source uuid;
  grp text;
  ds uuid;
  prev uuid;
  vis text;
  who uuid;
BEGIN
  IF TG_OP = 'DELETE' THEN
    tenant := OLD.tenant_id;
  ELSE
    tenant := NEW.tenant_id;
  END IF;
  CASE t
  WHEN 'broker_projects' THEN
    IF TG_OP = 'UPDATE' AND (OLD.visibility, OLD.owner_id, OLD.group_key)
                            IS DISTINCT FROM (NEW.visibility, NEW.owner_id, NEW.group_key) THEN
      -- Who may read the project changed: its lab reads everything again, and so do a lab it left and its old and new
      -- owners, who need not be in its lab.
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NEW.project_id, NULL, NULL, NEW.group_key, NULL, true, NULL);
      IF OLD.group_key IS DISTINCT FROM NEW.group_key THEN
        PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, OLD.group_key, NULL, true, NULL);
      END IF;
      IF OLD.owner_id IS DISTINCT FROM NEW.owner_id THEN
        PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, NULL, OLD.owner_id, true, NULL);
        PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, NULL, NEW.owner_id, true, NULL);
      END IF;
    ELSE
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'project', NEW.project_id, NULL, NULL, NEW.group_key, NULL, false, NULL);
    END IF;
  WHEN 'broker_project_checkpoints' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'checkpoint', NEW.project_id, NULL, NULL, NULL, NULL, true, NULL);
  WHEN 'broker_project_grants' THEN
    IF TG_OP = 'DELETE' THEN
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', OLD.project_id, NULL, NULL, NULL, OLD.principal_id, true, NULL);
    ELSE
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NEW.project_id, NULL, NULL, NULL, NEW.principal_id, true, NULL);
    END IF;
  WHEN 'broker_project_administrators' THEN
    IF TG_OP = 'DELETE' THEN
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, NULL, OLD.principal_id, true, NULL);
    ELSE
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, NULL, NEW.principal_id, true, NULL);
    END IF;
  WHEN 'broker_project_tasks' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'project_task', NEW.project_id, NEW.task_id, NULL, NULL, NULL, false,
      CASE WHEN NEW.parent_task_id IS NULL THEN NULL ELSE ARRAY['task:' || NEW.parent_task_id::text] END);
  WHEN 'broker_plan_events', 'broker_plan_revisions', 'broker_plan_pages' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'plan', NEW.project_id, NULL, NULL, NULL, NULL, false, NULL);
  WHEN 'broker_project_inspections' THEN
    -- An administrator's inspection began, ended or was removed: that person reads everything again.
    IF TG_OP = 'DELETE' THEN
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, NULL, OLD.principal_id, true, NULL);
    ELSE
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NEW.project_id, NULL, NULL, NULL, NEW.principal_id, true,
        NULL);
    END IF;
  WHEN 'broker_project_audit' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'audit', NEW.project_id, NULL, NULL, NULL, NULL, false, NULL);
  WHEN 'broker_figure_manuscript_events' THEN
    -- The mark shows on the figure's versions and on the analysis the figure was made from; a cleared or moved mark
    -- also leaves the version that held it (the figure's event before this one).
    SELECT f.source_task_id INTO source FROM enrichedlab_agent.broker_figure_requests f
    WHERE f.tenant_id = tenant AND f.task_id = NEW.figure_task_id;
    SELECT e.version_task_id INTO prev FROM enrichedlab_agent.broker_figure_manuscript_events e
    WHERE e.tenant_id = tenant AND e.figure_task_id = NEW.figure_task_id AND e.event_id <> NEW.event_id
    ORDER BY e.occurred_at DESC, e.event_id DESC LIMIT 1;
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'plan', NEW.project_id, NULL, NULL, NULL, NULL, false,
      ARRAY(SELECT DISTINCT 'task:' || x::text FROM unnest(ARRAY[NEW.figure_task_id, NEW.version_task_id, prev, source]) AS x
            WHERE x IS NOT NULL));
  WHEN 'broker_project_looks' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'look', NEW.project_id, NULL, NULL, NULL, NEW.principal_id, true, NULL);
  WHEN 'broker_research_runs' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'run', NEW.project_id, NULL, NULL, NULL, NULL, true, NULL);
  WHEN 'broker_research_turns', 'broker_research_run_events' THEN
    SELECT r.project_id INTO proj FROM enrichedlab_agent.broker_research_runs r
    WHERE r.tenant_id = tenant AND r.run_id = NEW.run_id;
    IF proj IS NOT NULL THEN
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'turn', proj, NULL, NULL, NULL, NULL, false, NULL);
    END IF;
  WHEN 'broker_tasks', 'broker_task_events', 'broker_task_reviews', 'broker_analysis_receipts', 'broker_figure_receipts',
       'broker_dataset_receipts', 'broker_query_receipts', 'broker_artifact_digests', 'broker_task_working_copies',
       'broker_task_output_closures', 'broker_task_scientific_evidence', 'broker_task_scientific_history',
       'broker_artifact_kinds', 'broker_artifacts', 'broker_attempt_outputs', 'broker_attempt_inputs',
       'broker_attempt_executions', 'broker_task_platform_tool_jobs' THEN
    IF t = 'broker_artifact_kinds' THEN
      SELECT a.task_id INTO task FROM enrichedlab_agent.broker_artifacts a
      WHERE a.tenant_id = tenant AND a.artifact_id = NEW.artifact_id;
    ELSE
      task := NEW.task_id;
    END IF;
    -- A file only operators see (and what is recorded about it) changes no workspace view; a file that stops being
    -- visible does.
    IF t = 'broker_artifacts' THEN
      vis := CASE WHEN TG_OP = 'UPDATE' AND OLD.visibility = 'researcher' THEN 'researcher' ELSE NEW.visibility END;
    ELSIF t IN ('broker_attempt_outputs', 'broker_artifact_kinds', 'broker_artifact_digests') THEN
      SELECT a.visibility INTO vis FROM enrichedlab_agent.broker_artifacts a
      WHERE a.tenant_id = tenant AND a.artifact_id = NEW.artifact_id;
    END IF;
    IF vis = 'operator' THEN
      RETURN NULL;
    END IF;
    -- Once per task per transaction: the keys depend only on the task, and readers read after the commit.
    IF task IS NOT NULL AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_changes c
                                        WHERE c.tenant_id = tenant AND c.txid = pg_current_xact_id()
                                          AND c.kind = 'task' AND c.task_id = task) THEN
      SELECT p.project_id, p.source_task_id INTO proj, source FROM enrichedlab_agent.broker_change_task_project(tenant, task) p;
      -- A dataset's step shows on the dataset's page.
      SELECT r.dataset_id INTO ds FROM enrichedlab_agent.broker_dataset_requests r
      WHERE r.tenant_id = tenant AND r.task_id = task;
      -- A task outside any project is read by its owner alone (broker_task_can), so the row names no lab.
      -- A finished analysis can wait for its reviewer, so every task change names the inbox.
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'task', proj, task, ds, NULL, NULL, true,
        CASE WHEN source IS NULL THEN NULL ELSE ARRAY['task:' || source::text] END);
      -- A shared note's state follows the reviews and files of the analyses it cites (broker_lab_note_state), so
      -- each lab whose notes cite this task reads its notes again.
      FOR grp IN SELECT DISTINCT n.group_key FROM enrichedlab_agent.broker_lab_notes n
                 WHERE n.tenant_id = tenant
                   AND n.evidence @> jsonb_build_array(jsonb_build_object('task_id', task::text)) LOOP
        PERFORM enrichedlab_agent.broker_add_change(tenant, 'note', NULL, NULL, NULL, grp, NULL, false, NULL);
      END LOOP;
    END IF;
  WHEN 'broker_analysis_requests', 'broker_figure_requests' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'request', NEW.project_id, NEW.task_id, NULL, NULL, NULL, false,
      CASE WHEN NEW.source_task_id IS NULL THEN NULL ELSE ARRAY['task:' || NEW.source_task_id::text] END);
  WHEN 'broker_dataset_requests' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'dataset', NEW.project_id, NEW.task_id, NEW.dataset_id, NULL, NULL,
      false, NULL);
  WHEN 'broker_datasets' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'dataset', NEW.project_id, NULL, NEW.dataset_id, NULL, NULL, false, NULL);
  WHEN 'broker_inbox_acknowledgements' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'seen', NULL, NULL, NULL, NULL, NEW.principal_id, true, NULL);
  WHEN 'broker_lab_notes' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'note', NULL, NULL, NULL, NEW.group_key, NULL, true,
      CASE WHEN NEW.project_id IS NULL THEN NULL ELSE ARRAY['project:' || NEW.project_id::text] END);
  WHEN 'broker_lab_note_events' THEN
    SELECT n.group_key, n.project_id INTO grp, proj FROM enrichedlab_agent.broker_lab_notes n
    WHERE n.tenant_id = tenant AND n.note_id = NEW.note_id;
    IF grp IS NOT NULL THEN
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'note', NULL, NULL, NULL, grp, NULL, true,
        ARRAY['project:' || proj::text]);
    END IF;
  WHEN 'broker_library_papers' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'library', NULL, NULL, NULL, NEW.group_key, NULL, true, NULL);
  WHEN 'broker_library_paper_events', 'broker_library_label_events' THEN
    SELECT p.group_key INTO grp FROM enrichedlab_agent.broker_library_papers p WHERE p.tenant_id = tenant AND p.paper_id = NEW.paper_id;
    IF grp IS NOT NULL THEN
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'library', NULL, NULL, NULL, grp, NULL, true, NULL);
    END IF;
  WHEN 'broker_group_memberships' THEN
    IF TG_OP = 'DELETE' THEN
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, OLD.group_key, OLD.principal_id, true, NULL);
    ELSE
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, NEW.group_key, NEW.principal_id, true, NULL);
    END IF;
  WHEN 'broker_principals' THEN
    -- A person's name shows on their labs' pages and in project lists, and a person disabled or enabled changes who
    -- may be invited.
    FOR grp IN SELECT m.group_key FROM enrichedlab_agent.broker_group_memberships m
               WHERE m.tenant_id = tenant AND m.principal_id = NEW.principal_id LOOP
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'person', NULL, NULL, NULL, grp, NULL, false, ARRAY['projects']);
    END LOOP;
  WHEN 'broker_render_catalogs', 'broker_analysis_catalogs' THEN
    -- The render or analysis service reported a new catalog: an editor open in any lab offers its choices.
    FOR grp IN SELECT g.group_key FROM enrichedlab_agent.broker_research_groups g
               WHERE g.tenant_id = tenant AND g.enabled LOOP
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'catalog', NULL, NULL, NULL, grp, NULL, false, ARRAY['catalogs']);
    END LOOP;
  WHEN 'broker_research_groups' THEN
    PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, NEW.group_key, NULL, false, NULL);
    -- An administrator inspecting one of the lab's projects reads it only while the lab is enabled.
    FOR who IN SELECT DISTINCT i.principal_id FROM enrichedlab_agent.broker_project_inspections i
               JOIN enrichedlab_agent.broker_projects p ON p.tenant_id = i.tenant_id AND p.project_id = i.project_id
               WHERE i.tenant_id = tenant AND p.group_key = NEW.group_key AND i.expires_at > clock_timestamp() LOOP
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, NULL, who, true, NULL);
    END LOOP;
  WHEN 'broker_agent_profiles' THEN
    FOR grp IN SELECT g.group_key FROM enrichedlab_agent.broker_group_profile_grants g
               WHERE g.tenant_id = tenant AND g.profile_key = NEW.profile_key LOOP
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, grp, NULL, false, NULL);
    END LOOP;
  WHEN 'broker_group_profile_grants' THEN
    -- The lab's methods changed: its members read everything again, `/me` included.
    IF TG_OP = 'DELETE' THEN
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, OLD.group_key, NULL, false, NULL);
    ELSE
      PERFORM enrichedlab_agent.broker_add_change(tenant, 'access', NULL, NULL, NULL, NEW.group_key, NULL, false, NULL);
    END IF;
  ELSE
    NULL;
  END CASE;
  RETURN NULL;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_note_change() FROM PUBLIC;

DO $$
DECLARE
  t text;
BEGIN
  -- Inserts only: these rows are immutable or only ever added.
  FOREACH t IN ARRAY ARRAY['broker_project_checkpoints', 'broker_project_tasks', 'broker_plan_events',
    'broker_plan_revisions', 'broker_plan_pages', 'broker_figure_manuscript_events', 'broker_research_turns',
    'broker_research_run_events', 'broker_task_events', 'broker_task_reviews', 'broker_analysis_requests',
    'broker_figure_requests', 'broker_dataset_requests', 'broker_lab_note_events', 'broker_library_papers',
    'broker_library_paper_events', 'broker_library_label_events', 'broker_task_output_closures',
    'broker_task_scientific_evidence', 'broker_task_scientific_history', 'broker_artifact_kinds', 'broker_artifacts',
    'broker_attempt_outputs', 'broker_attempt_inputs', 'broker_attempt_executions', 'broker_task_platform_tool_jobs',
    'broker_project_audit', 'broker_render_catalogs', 'broker_analysis_catalogs']
  LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS broker_note_change ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER broker_note_change AFTER INSERT ON enrichedlab_agent.%I FOR EACH ROW '
      'EXECUTE FUNCTION enrichedlab_agent.broker_note_change()', t);
  END LOOP;
  -- Inserts and updates: the newest state of a row is what a view shows.
  FOREACH t IN ARRAY ARRAY['broker_analysis_receipts', 'broker_figure_receipts', 'broker_dataset_receipts',
    'broker_query_receipts', 'broker_artifact_digests', 'broker_datasets', 'broker_lab_notes',
    'broker_inbox_acknowledgements']
  LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS broker_note_change ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER broker_note_change AFTER INSERT OR UPDATE ON enrichedlab_agent.%I FOR EACH ROW '
      'EXECUTE FUNCTION enrichedlab_agent.broker_note_change()', t);
  END LOOP;
  -- Access and method changes: inserts and deletes; updates below.
  FOREACH t IN ARRAY ARRAY['broker_project_grants', 'broker_group_memberships', 'broker_group_profile_grants',
    'broker_project_administrators']
  LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS broker_note_change ON enrichedlab_agent.%I', t);
    EXECUTE format('CREATE TRIGGER broker_note_change AFTER INSERT OR DELETE ON enrichedlab_agent.%I FOR EACH ROW '
      'EXECUTE FUNCTION enrichedlab_agent.broker_note_change()', t);
  END LOOP;
  -- Inspections: every insert, update and removal.
  DROP TRIGGER IF EXISTS broker_note_change ON enrichedlab_agent.broker_project_inspections;
  CREATE TRIGGER broker_note_change AFTER INSERT OR UPDATE OR DELETE ON enrichedlab_agent.broker_project_inspections
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
END $$;

-- A project grant's capabilities, and a membership's role. Sign-in rewrites membership rows without changing them;
-- that is not a change.
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_project_grants;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_project_grants FOR EACH ROW
  WHEN (OLD.capabilities IS DISTINCT FROM NEW.capabilities)
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_group_memberships;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_group_memberships FOR EACH ROW
  WHEN (OLD.role IS DISTINCT FROM NEW.role)
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();

-- A look at a newer revision changes the reader's inbox; a repeated look does not.
DROP TRIGGER IF EXISTS broker_note_change ON enrichedlab_agent.broker_project_looks;
CREATE TRIGGER broker_note_change AFTER INSERT ON enrichedlab_agent.broker_project_looks FOR EACH ROW
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_project_looks;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_project_looks FOR EACH ROW
  WHEN (OLD.revision IS DISTINCT FROM NEW.revision)
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();

-- A working copy kept or cleared; the custody dispatcher's periodic observation is not a change.
DROP TRIGGER IF EXISTS broker_note_change ON enrichedlab_agent.broker_task_working_copies;
CREATE TRIGGER broker_note_change AFTER INSERT ON enrichedlab_agent.broker_task_working_copies FOR EACH ROW
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_task_working_copies;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_task_working_copies FOR EACH ROW
  WHEN ((OLD.cleared_at, OLD.retention_until) IS DISTINCT FROM (NEW.cleared_at, NEW.retention_until))
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();

-- Projects: a new project, and a change a reader sees (title, visibility, revision, lab, owner), never `updated_at`
-- alone.
DROP TRIGGER IF EXISTS broker_note_change ON enrichedlab_agent.broker_projects;
CREATE TRIGGER broker_note_change AFTER INSERT ON enrichedlab_agent.broker_projects FOR EACH ROW
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_projects;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_projects FOR EACH ROW
  WHEN ((OLD.title, OLD.visibility, OLD.revision, OLD.group_key, OLD.owner_id)
        IS DISTINCT FROM (NEW.title, NEW.visibility, NEW.revision, NEW.group_key, NEW.owner_id))
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();

-- Tasks: a change of status, attempt, requested status, result or error; a lease renewal is not a change.
DROP TRIGGER IF EXISTS broker_note_change ON enrichedlab_agent.broker_tasks;
CREATE TRIGGER broker_note_change AFTER INSERT ON enrichedlab_agent.broker_tasks FOR EACH ROW
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_tasks;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_tasks FOR EACH ROW
  WHEN ((OLD.status, OLD.desired_status, OLD.attempt, OLD.result_class, OLD.error_code)
        IS DISTINCT FROM (NEW.status, NEW.desired_status, NEW.attempt, NEW.result_class, NEW.error_code))
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();

-- Research runs: a new run, and a change of turn, code, failures or status, except a running run that waits for its
-- tools and wakes again in the same turn (it does so every few seconds while an analysis works).
DROP TRIGGER IF EXISTS broker_note_change ON enrichedlab_agent.broker_research_runs;
CREATE TRIGGER broker_note_change AFTER INSERT ON enrichedlab_agent.broker_research_runs FOR EACH ROW
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_research_runs;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_research_runs FOR EACH ROW
  WHEN ((OLD.turn, OLD.code, OLD.failures, OLD.status) IS DISTINCT FROM (NEW.turn, NEW.code, NEW.failures, NEW.status)
        AND NOT (OLD.status IN ('running', 'waiting') AND NEW.status IN ('running', 'waiting')
                 AND OLD.turn = NEW.turn AND OLD.failures = NEW.failures
                 AND OLD.code IS NOT DISTINCT FROM NEW.code))
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
-- Finishing a scientific history moves the boundary its readers show.
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_task_scientific_history;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_task_scientific_history FOR EACH ROW
  WHEN (OLD.last_segment IS DISTINCT FROM NEW.last_segment)
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
-- A person's name changed (the sign-in rewrite that changes nothing else is not a change).
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_principals;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_principals FOR EACH ROW
  WHEN ((OLD.display_name, OLD.enabled) IS DISTINCT FROM (NEW.display_name, NEW.enabled))
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
-- A file registered again with a different visibility, role, label, type, size or digest.
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_artifacts;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_artifacts FOR EACH ROW
  WHEN ((OLD.visibility, OLD.role, OLD.label, OLD.media_type, OLD.size_bytes, OLD.sha256)
        IS DISTINCT FROM (NEW.visibility, NEW.role, NEW.label, NEW.media_type, NEW.size_bytes, NEW.sha256))
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
-- A lab or a method the catalog seed changed (a rerun that changes nothing but the time is not a change).
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_research_groups;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_research_groups FOR EACH ROW
  WHEN ((to_jsonb(OLD) - 'updated_at') IS DISTINCT FROM (to_jsonb(NEW) - 'updated_at'))
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();
DROP TRIGGER IF EXISTS broker_note_change_update ON enrichedlab_agent.broker_agent_profiles;
CREATE TRIGGER broker_note_change_update AFTER UPDATE ON enrichedlab_agent.broker_agent_profiles FOR EACH ROW
  WHEN ((to_jsonb(OLD) - 'updated_at') IS DISTINCT FROM (to_jsonb(NEW) - 'updated_at'))
  EXECUTE FUNCTION enrichedlab_agent.broker_note_change();

-- The reader's position in the feed: rows past the cursor whose transactions are older than any still running. It
-- returns the keys the reader may read, the projects among them, whether the reader's own access changed, and the new
-- position, which moves past rows the reader may not see so a later read never scans them again. A row is readable
-- under the rule of what it names: a project (broker_project_can), else a task (broker_task_can), else a lab (its
-- members), else a person (that person). An access row for the reader, or for a lab of the reader with no person
-- named, reports `access_changed` even when the reader can no longer read what it names.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_change_feed(p_tenant text, p_actor uuid, p_after_txid xid8,
  p_after_id bigint, p_limit integer)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  watermark xid8 := pg_snapshot_xmin(pg_current_snapshot());
  horizon enrichedlab_agent.broker_change_horizons%ROWTYPE;
  newest_txid xid8;
  newest_id bigint;
  last_txid xid8;
  last_id bigint;
  scanned integer;
  visible_keys text[];
  projects uuid[];
  access boolean;
  pruned boolean;
BEGIN
  IF p_actor IS NULL OR p_limit IS NULL OR p_limit NOT BETWEEN 1 AND 5000
     OR ((p_after_txid IS NULL) <> (p_after_id IS NULL)) THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:change_cursor_invalid';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_principals a
                 JOIN enrichedlab_agent.broker_tenants tn ON tn.tenant_id = a.tenant_id AND tn.enabled
                 WHERE a.tenant_id = p_tenant AND a.principal_id = p_actor AND a.enabled) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:principal_disabled';
  END IF;
  SELECT c.txid, c.change_id INTO newest_txid, newest_id FROM enrichedlab_agent.broker_changes c
  WHERE c.tenant_id = p_tenant AND c.txid < watermark ORDER BY c.txid DESC, c.change_id DESC LIMIT 1;
  SELECT * INTO horizon FROM enrichedlab_agent.broker_change_horizons WHERE tenant_id = p_tenant;
  pruned := FOUND;
  -- Once every kept row is pruned, the horizon is the newest position, so an idle reader is not sent back again. A
  -- horizon is always below the watermark (the prune removes only finished rows); one that is not is never handed out.
  IF pruned AND horizon.txid < watermark
     AND (newest_txid IS NULL OR (horizon.txid, horizon.change_id) > (newest_txid, newest_id)) THEN
    newest_txid := horizon.txid;
    newest_id := horizon.change_id;
  END IF;
  -- A first read starts at the newest final position, so it replays nothing.
  IF p_after_txid IS NULL THEN
    RETURN jsonb_build_object('position', CASE WHEN newest_txid IS NULL THEN 'x0-0'
                                               ELSE 'x' || newest_txid::text || '-' || newest_id::text END,
      'keys', '[]'::jsonb, 'projects', '[]'::jsonb, 'access_changed', false, 'behind', false);
  END IF;
  -- Behind: older than the pruned rows, at or above the watermark (every position the feed gives out is below it, and
  -- the watermark never falls, so this is a restored database or a position never given out), not a position the
  -- feed gave out (a kept row, the horizon or the empty start), or too far behind to list.
  IF (pruned AND (p_after_txid, p_after_id) < (horizon.txid, horizon.change_id))
     OR p_after_txid >= watermark
     OR NOT ((p_after_txid, p_after_id) = ('0'::xid8, 0::bigint)
             OR (pruned AND (p_after_txid, p_after_id) = (horizon.txid, horizon.change_id))
             OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_changes c
                        WHERE c.tenant_id = p_tenant AND c.txid = p_after_txid AND c.change_id = p_after_id)) THEN
    RETURN jsonb_build_object('position', CASE WHEN newest_txid IS NULL THEN 'x0-0'
                                               ELSE 'x' || newest_txid::text || '-' || newest_id::text END,
      'keys', '[]'::jsonb, 'projects', '[]'::jsonb, 'access_changed', false, 'behind', true);
  END IF;
  IF newest_txid IS NULL OR (newest_txid, newest_id) <= (p_after_txid, p_after_id) THEN
    RETURN jsonb_build_object('position', 'x' || p_after_txid::text || '-' || p_after_id::text,
      'keys', '[]'::jsonb, 'projects', '[]'::jsonb, 'access_changed', false, 'behind', false);
  END IF;
  SELECT count(*) INTO scanned FROM (
    SELECT 1 FROM enrichedlab_agent.broker_changes c
    WHERE c.tenant_id = p_tenant AND (c.txid, c.change_id) > (p_after_txid, p_after_id) AND c.txid < watermark
    ORDER BY c.txid, c.change_id LIMIT p_limit + 1) w;
  IF scanned > p_limit THEN
    RETURN jsonb_build_object('position', 'x' || newest_txid::text || '-' || newest_id::text,
      'keys', '[]'::jsonb, 'projects', '[]'::jsonb, 'access_changed', false, 'behind', true);
  END IF;
  last_txid := newest_txid;
  last_id := newest_id;
  WITH win AS (
    SELECT c.kind, c.project_id, c.task_id, c.group_key, c.principal_id, c.keys
    FROM enrichedlab_agent.broker_changes c
    WHERE c.tenant_id = p_tenant AND (c.txid, c.change_id) > (p_after_txid, p_after_id)
      AND (c.txid, c.change_id) <= (last_txid, last_id)),
  labs AS (
    SELECT m.group_key FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND g.enabled
    WHERE m.tenant_id = p_tenant AND m.principal_id = p_actor),
  member_of AS (
    SELECT m.group_key FROM enrichedlab_agent.broker_group_memberships m
    WHERE m.tenant_id = p_tenant AND m.principal_id = p_actor),
  -- Every project a row names, as its scope or as one of its keys.
  readable_projects AS (
    SELECT p.id FROM (SELECT w.project_id AS id FROM win w WHERE w.project_id IS NOT NULL
                      UNION SELECT CASE WHEN k LIKE 'project:%' THEN substr(k, 9)::uuid END
                      FROM win w, unnest(w.keys) AS k WHERE k LIKE 'project:%') p
    WHERE p.id IS NOT NULL AND enrichedlab_agent.broker_project_can(p_tenant, p_actor, p.id, 'read')),
  -- A task a row names besides its own (a source analysis, a figure's versions).
  readable_named_tasks AS (
    SELECT t.id FROM (SELECT DISTINCT CASE WHEN k LIKE 'task:%' THEN substr(k, 6)::uuid END AS id
                      FROM win w, unnest(w.keys) AS k
                      WHERE k LIKE 'task:%' AND k IS DISTINCT FROM 'task:' || w.task_id::text) t
    WHERE t.id IS NOT NULL AND enrichedlab_agent.broker_task_can(p_tenant, p_actor, t.id, 'read')),
  readable_tasks AS (
    SELECT t.id FROM (SELECT DISTINCT w.task_id AS id FROM win w WHERE w.task_id IS NOT NULL AND w.project_id IS NULL) t
    WHERE enrichedlab_agent.broker_task_can(p_tenant, p_actor, t.id, 'read')),
  classified AS (
    SELECT w.*,
      CASE
        WHEN w.kind IN ('seen', 'look') THEN w.principal_id = p_actor
          AND (w.project_id IS NULL OR w.project_id IN (SELECT id FROM readable_projects))
        WHEN w.project_id IS NOT NULL THEN w.project_id IN (SELECT id FROM readable_projects)
        WHEN w.task_id IS NOT NULL THEN w.task_id IN (SELECT id FROM readable_tasks)
        WHEN w.group_key IS NOT NULL THEN w.group_key IN (SELECT group_key FROM labs)
        WHEN w.principal_id IS NOT NULL THEN w.principal_id = p_actor
        ELSE false END AS readable,
      (w.kind = 'access' AND (w.principal_id = p_actor
        OR (w.principal_id IS NULL AND w.group_key IN (SELECT group_key FROM member_of)))) AS mine
    FROM win w)
  SELECT coalesce(array_agg(DISTINCT k ORDER BY k) FILTER (WHERE k IS NOT NULL AND v.readable
           AND CASE WHEN k LIKE 'project:%' THEN substr(k, 9)::uuid IN (SELECT id FROM readable_projects)
                    WHEN k LIKE 'task:%' AND k IS DISTINCT FROM 'task:' || v.task_id::text
                      THEN substr(k, 6)::uuid IN (SELECT id FROM readable_named_tasks)
                    ELSE true END), '{}'),
         coalesce(array_agg(DISTINCT v.project_id) FILTER (WHERE v.project_id IS NOT NULL AND v.readable), '{}'),
         coalesce(bool_or(v.mine), false)
    INTO visible_keys, projects, access
  FROM classified v LEFT JOIN LATERAL unnest(v.keys) AS k ON true;
  RETURN jsonb_build_object('position', 'x' || last_txid::text || '-' || last_id::text,
    'keys', to_jsonb(visible_keys), 'projects', to_jsonb(projects), 'access_changed', access, 'behind', false);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_change_feed(text, uuid, xid8, bigint, integer) FROM PUBLIC;

-- Whether work still runs in each named project the reader may read (the per-project stream's rule): an analysis
-- queued, leased, starting, running or cancelling, or a research run queued, running or waiting.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_projects_busy(p_tenant text, p_actor uuid, p_projects uuid[])
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT coalesce(jsonb_object_agg(p.id::text,
    EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t
              ON t.tenant_id = pt.tenant_id AND t.task_id = pt.task_id
            WHERE pt.tenant_id = p_tenant AND pt.project_id = p.id
              AND t.status IN ('queued', 'leased', 'starting', 'running', 'cancelling'))
    OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_research_runs r
               WHERE r.tenant_id = p_tenant AND r.project_id = p.id AND r.status IN ('queued', 'running', 'waiting'))),
    '{}'::jsonb)
  FROM unnest(coalesce(p_projects, '{}')) AS p(id)
  WHERE cardinality(coalesce(p_projects, '{}')) <= 64
    AND enrichedlab_agent.broker_project_can(p_tenant, p_actor, p.id, 'read')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_projects_busy(text, uuid, uuid[]) FROM PUBLIC;

-- Rows older than a day leave; the horizon remembers the highest one removed.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_prune_changes(p_tenant text)
RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  cutoff timestamptz := clock_timestamp() - interval '1 day';
  watermark xid8 := pg_snapshot_xmin(pg_current_snapshot());
  top_txid xid8;
  top_id bigint;
  removed bigint;
BEGIN
  -- Only rows older than a day whose transactions finished before every running one; a younger row, even below an
  -- older one in feed order, stays.
  WITH gone AS (
    DELETE FROM enrichedlab_agent.broker_changes c
    WHERE c.tenant_id = p_tenant AND c.at < cutoff AND c.txid < watermark
    RETURNING c.txid, c.change_id)
  SELECT count(*), (array_agg(g.txid ORDER BY g.txid DESC, g.change_id DESC))[1],
         (array_agg(g.change_id ORDER BY g.txid DESC, g.change_id DESC))[1]
    INTO removed, top_txid, top_id FROM gone g;
  IF removed = 0 THEN
    RETURN 0;
  END IF;
  INSERT INTO enrichedlab_agent.broker_change_horizons (tenant_id, txid, change_id) VALUES (p_tenant, top_txid, top_id)
  ON CONFLICT (tenant_id) DO UPDATE SET txid = EXCLUDED.txid, change_id = EXCLUDED.change_id, pruned_at = clock_timestamp()
  WHERE (EXCLUDED.txid, EXCLUDED.change_id) > (broker_change_horizons.txid, broker_change_horizons.change_id);
  RETURN removed;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_prune_changes(text) FROM PUBLIC;

-- Whether each of the caller's labs has exactly one enabled lab-data method (the rule of `broker_dataset_method`).
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_group_lab_data(p_tenant text, p_principal uuid)
RETURNS TABLE(group_key text, lab_data boolean) LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT m.group_key,
    (SELECT count(*) FROM enrichedlab_agent.broker_group_profile_grants g
     JOIN enrichedlab_agent.broker_agent_profiles p ON p.tenant_id = g.tenant_id AND p.profile_key = g.profile_key
     WHERE g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND p.enabled AND p.route = 'server-dataset') = 1
  FROM enrichedlab_agent.broker_group_memberships m
  WHERE m.tenant_id = p_tenant AND m.principal_id = p_principal
  ORDER BY m.group_key
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_group_lab_data(text, uuid) FROM PUBLIC;

-- A refused reply's reasoning, kept with its context and reply.
ALTER TABLE enrichedlab_agent.broker_research_run_events
  ADD COLUMN IF NOT EXISTS lead_reasoning_sha256 text;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'broker_research_run_events_reasoning_check') THEN
    ALTER TABLE enrichedlab_agent.broker_research_run_events ADD CONSTRAINT broker_research_run_events_reasoning_check
      CHECK (lead_reasoning_sha256 IS NULL
             OR (lead_reply_sha256 IS NOT NULL AND lead_reasoning_sha256 ~ '^sha256:[0-9a-f]{64}$'));
  END IF;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_yield_research_run(p_tenant text,p_run uuid,p_worker text,
  p_generation bigint,p_code text,p_reason text,p_lead_context_sha256 text,p_lead_reply_sha256 text,
  p_lead_reasoning_sha256 text)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; next_sequence bigint;
  previous record;
BEGIN
  IF p_code IS NULL OR p_code NOT IN ('waiting_for_tools','model_unavailable','proposal_invalid','checkpoint_changed','turn_limit','review_required')
     OR (p_reason IS NOT NULL AND p_reason !~ '^[a-z][a-z0-9_]{1,63}$')
     OR ((p_lead_context_sha256 IS NULL) <> (p_lead_reply_sha256 IS NULL))
     OR (p_lead_reply_sha256 IS NOT NULL AND (p_code <> 'proposal_invalid'
         OR p_lead_context_sha256 !~ '^sha256:[0-9a-f]{64}$' OR p_lead_reply_sha256 !~ '^sha256:[0-9a-f]{64}$'))
     OR (p_lead_reasoning_sha256 IS NOT NULL AND (p_lead_reply_sha256 IS NULL
         OR p_lead_reasoning_sha256 !~ '^sha256:[0-9a-f]{64}$')) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  IF p_lead_reply_sha256 IS NOT NULL AND (SELECT count(*) FROM enrichedlab_agent.broker_research_contexts c
      WHERE c.tenant_id=p_tenant AND c.sha256 IN (p_lead_context_sha256,p_lead_reply_sha256))
      <> (CASE WHEN p_lead_context_sha256=p_lead_reply_sha256 THEN 1 ELSE 2 END) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_not_kept'; END IF;
  IF p_lead_reasoning_sha256 IS NOT NULL AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_research_contexts c
      WHERE c.tenant_id=p_tenant AND c.sha256=p_lead_reasoning_sha256) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_not_kept'; END IF;
  UPDATE enrichedlab_agent.broker_research_runs SET
    failures=failures+CASE WHEN p_code IN ('model_unavailable','proposal_invalid') THEN 1 ELSE 0 END,
    status=CASE WHEN p_code IN ('turn_limit','review_required') THEN 'review_required'
      WHEN p_code IN ('model_unavailable','proposal_invalid') AND failures>=2 THEN 'paused' ELSE 'waiting' END,
    code=p_code,available_at=clock_timestamp()+CASE WHEN p_code='waiting_for_tools' THEN interval '5 seconds' ELSE interval '15 seconds' END,
    lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=p_run AND status='running' AND lease_owner=p_worker AND generation=p_generation AND lease_until>clock_timestamp()
    RETURNING * INTO r;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  SELECT e.sequence,e.code,e.turn INTO previous FROM enrichedlab_agent.broker_research_run_events e
    WHERE e.tenant_id=p_tenant AND e.run_id=p_run ORDER BY e.sequence DESC LIMIT 1;
  -- A run polls every few seconds while a tool works. Keep the first wait of a turn,
  -- not one row per poll; every other deferral is recorded.
  IF p_code='waiting_for_tools' AND previous.code='waiting_for_tools' AND previous.turn=r.turn THEN RETURN; END IF;
  next_sequence := coalesce(previous.sequence,0)+1;
  INSERT INTO enrichedlab_agent.broker_research_run_events(tenant_id,run_id,sequence,code,reason,generation,turn,failures,
    status_after,lead_context_sha256,lead_reply_sha256,lead_reasoning_sha256)
    VALUES(p_tenant,p_run,next_sequence,p_code,p_reason,p_generation,r.turn,r.failures,r.status,p_lead_context_sha256,
      p_lead_reply_sha256,p_lead_reasoning_sha256);
END $$;

-- The eight-argument signature stays valid for a coordinator that has not been upgraded.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_yield_research_run(p_tenant text,p_run uuid,p_worker text,
  p_generation bigint,p_code text,p_reason text,p_lead_context_sha256 text,p_lead_reply_sha256 text)
RETURNS void LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT enrichedlab_agent.broker_yield_research_run(p_tenant,p_run,p_worker,p_generation,p_code,p_reason,
    p_lead_context_sha256,p_lead_reply_sha256,NULL::text)
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_yield_research_run(text,uuid,text,bigint,text,text,text,text,text) FROM PUBLIC;

-- A reader of the project who is a member of its lab reads a text a decision of this run names: migration 050's rule,
-- with each call's reasoning and a refused reply's reasoning added.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_context(
  p_tenant text, p_actor uuid, p_run uuid, p_sha256 text)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  r enrichedlab_agent.broker_research_runs%ROWTYPE;
  grp text;
  body bytea;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id = p_tenant AND run_id = p_run;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, r.project_id, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF p_sha256 IS NULL OR p_sha256 !~ '^sha256:[0-9a-f]{64}$' THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:context_invalid';
  END IF;
  SELECT group_key INTO grp FROM enrichedlab_agent.broker_projects
  WHERE tenant_id = p_tenant AND project_id = r.project_id;
  IF NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = m.tenant_id AND a.principal_id = m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = m.tenant_id AND g.group_key = m.group_key AND g.enabled
    WHERE m.tenant_id = p_tenant AND m.group_key = grp AND m.principal_id = p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:context_lab_members_only';
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_research_turns t
    WHERE t.tenant_id = p_tenant AND t.run_id = p_run
      AND (p_sha256 IN (t.record->'lead'->>'context_sha256', t.record->'lead'->>'system_sha256',
                        t.record->'lead'->>'content_sha256', t.record->'lead'->>'reasoning_sha256',
                        t.record->'methods_specialist'->>'context_sha256', t.record->'methods_specialist'->>'system_sha256',
                        t.record->'methods_specialist'->>'content_sha256',
                        t.record->'methods_specialist'->>'reasoning_sha256')
        OR EXISTS (
          SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(t.record->'literature_claims'->'specialist') = 'array'
                                                  THEN t.record->'literature_claims'->'specialist' ELSE '[]'::jsonb END) s
          WHERE p_sha256 IN (s->>'context_sha256', s->>'system_sha256', s->>'content_sha256', s->>'reasoning_sha256'))))
     AND NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_research_run_events e
    WHERE e.tenant_id = p_tenant AND e.run_id = p_run
      AND p_sha256 IN (e.lead_context_sha256, e.lead_reply_sha256, e.lead_reasoning_sha256)) THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:context_not_found';
  END IF;
  SELECT content INTO body FROM enrichedlab_agent.broker_research_contexts
  WHERE tenant_id = p_tenant AND sha256 = p_sha256;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:context_not_kept';
  END IF;
  RETURN jsonb_build_object('sha256', p_sha256, 'bytes', octet_length(body), 'text', convert_from(body, 'UTF8'));
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (61) ON CONFLICT DO NOTHING;
COMMIT;
