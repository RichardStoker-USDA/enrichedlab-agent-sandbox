-- Lab datasets. A person who may contribute to a project sends a table the lab
-- keeps (delimited text or an Excel workbook). The API sends it to the upload guard (use case `lab-dataset`) and records
-- the guard's decision with the dataset. For an allowed file it also submits the intake task under the lab's dataset
-- method (route `server-dataset`): the worker fetches the guard's rebuilt copy, a cell grid, checks its digest, keeps it
-- in a package and profiles it. A person then asks for a preparation: a typed specification, recorded in full, that the
-- worker's executor applies to the kept grid to write the table the template analyses and figures read. Every attempt
-- has one receipt. The broker never holds the file the person sent; the guard keeps the original under its own
-- retention, and the grid is the broker's copy.
BEGIN;

-- Routes whose tasks only a person's request creates, never a general submission or a research run's plan.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_only_route(p_route text)
RETURNS boolean LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT p_route IN ('server-render', 'server-analysis', 'server-dataset')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_request_only_route(text) FROM PUBLIC;

CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_datasets (
  tenant_id text NOT NULL,
  dataset_id uuid NOT NULL,
  project_id uuid NOT NULL,
  request_id uuid NOT NULL,
  -- The name the person's file had, for display; it never names a path.
  file_name text NOT NULL,
  media_type text NOT NULL,
  size_bytes bigint NOT NULL,
  sha256 text NOT NULL,
  guard_intake uuid NOT NULL,
  guard_outcome text NOT NULL,
  guard_findings jsonb NOT NULL,
  guard_policy text NOT NULL,
  grid_sha256 text,
  grid_bytes bigint,
  intake_task_id uuid,
  principal_id uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, dataset_id),
  UNIQUE (tenant_id, project_id, request_id),
  UNIQUE (tenant_id, intake_task_id),
  FOREIGN KEY (tenant_id, project_id) REFERENCES enrichedlab_agent.broker_projects (tenant_id, project_id),
  FOREIGN KEY (tenant_id, intake_task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (length(file_name) BETWEEN 1 AND 255 AND file_name !~ '[/\\[:cntrl:]]'),
  CHECK (media_type IN ('text/csv', 'text/tab-separated-values', 'application/vnd.ms-excel',
    'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet', 'application/vnd.ms-excel.sheet.macroenabled.12')),
  CHECK (size_bytes BETWEEN 1 AND 33554432),
  CHECK (sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (guard_outcome IN ('allowed', 'quarantined', 'blocked')),
  CHECK (jsonb_typeof(guard_findings) = 'array' AND jsonb_array_length(guard_findings) <= 32
    AND octet_length(guard_findings::text) <= 4096),
  CHECK (length(guard_policy) BETWEEN 1 AND 128),
  CHECK ((guard_outcome = 'allowed') = (grid_sha256 IS NOT NULL)),
  -- An allowed file's intake may wait for room in the lab's queue: a task implies a grid, not the reverse.
  CONSTRAINT broker_datasets_task_needs_grid
    CHECK ((grid_sha256 IS NULL) = (grid_bytes IS NULL) AND (intake_task_id IS NULL OR grid_sha256 IS NOT NULL)),
  CHECK (grid_sha256 IS NULL OR grid_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (grid_bytes IS NULL OR grid_bytes BETWEEN 1 AND 268435456)
);

CREATE INDEX IF NOT EXISTS broker_datasets_project
  ON enrichedlab_agent.broker_datasets (tenant_id, project_id, created_at DESC);

-- Earlier drafts of this migration tied an intake task to every grid, or wrote the current rule unnamed. A table they
-- made keeps that check, and the first refuses an allowed file whose intake waits for room in the lab's queue: every
-- check on the intake task except the named one above is dropped, and the named one added where it is missing.
DO $$
DECLARE c record;
BEGIN
  FOR c IN SELECT conname FROM pg_constraint
           WHERE conrelid = 'enrichedlab_agent.broker_datasets'::regclass AND contype = 'c'
             AND conname <> 'broker_datasets_task_needs_grid'
             AND position('intake_task_id' in pg_get_constraintdef(oid)) > 0
  LOOP
    EXECUTE format('ALTER TABLE enrichedlab_agent.broker_datasets DROP CONSTRAINT %I', c.conname);
  END LOOP;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'enrichedlab_agent.broker_datasets'::regclass
                   AND conname = 'broker_datasets_task_needs_grid') THEN
    ALTER TABLE enrichedlab_agent.broker_datasets ADD CONSTRAINT broker_datasets_task_needs_grid
      CHECK ((grid_sha256 IS NULL) = (grid_bytes IS NULL) AND (intake_task_id IS NULL OR grid_sha256 IS NOT NULL));
  END IF;
END $$;

-- One request per task over a dataset: its intake, or a preparation with the specification the person approved.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_dataset_requests (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  project_id uuid NOT NULL,
  dataset_id uuid NOT NULL,
  operation text NOT NULL,
  spec jsonb,
  spec_sha256 text,
  title text NOT NULL,
  previous_task_id uuid,
  request jsonb NOT NULL,
  request_sha256 text NOT NULL,
  principal_id uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_tasks (tenant_id, task_id),
  FOREIGN KEY (tenant_id, dataset_id) REFERENCES enrichedlab_agent.broker_datasets (tenant_id, dataset_id),
  FOREIGN KEY (tenant_id, previous_task_id) REFERENCES enrichedlab_agent.broker_dataset_requests (tenant_id, task_id),
  FOREIGN KEY (tenant_id, principal_id) REFERENCES enrichedlab_agent.broker_principals (tenant_id, principal_id),
  CHECK (operation IN ('intake', 'preparation')),
  CHECK ((operation = 'preparation') = (spec IS NOT NULL)),
  CHECK ((spec IS NULL) = (spec_sha256 IS NULL)),
  CHECK (spec IS NULL OR (jsonb_typeof(spec) = 'object' AND octet_length(spec::text) <= 262144
    AND spec->>'schema' = 'enrichedlab.dataset-preparation.v1')),
  CHECK (spec_sha256 IS NULL OR spec_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (length(title) <= 200),
  CHECK (previous_task_id IS NULL OR (operation = 'preparation' AND previous_task_id <> task_id)),
  CHECK (jsonb_typeof(request) = 'object'),
  CHECK (request_sha256 ~ '^[0-9a-f]{64}$')
);

CREATE INDEX IF NOT EXISTS broker_dataset_requests_dataset
  ON enrichedlab_agent.broker_dataset_requests (tenant_id, dataset_id, created_at DESC);

-- One receipt per attempt and lease. An intake that profiled the grid keeps the profile, which holds no cell of the
-- data rows; a preparation keeps its executor's receipt, which counts every change and names the first cells no rule
-- read with their text, for the people of the project.
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_dataset_receipts (
  tenant_id text NOT NULL,
  task_id uuid NOT NULL,
  attempt integer NOT NULL,
  lease_generation bigint NOT NULL,
  status text NOT NULL,
  error_code text,
  profile jsonb,
  summary jsonb,
  grid_sha256 text,
  table_path text,
  table_sha256 text,
  table_rows bigint,
  table_columns jsonb,
  package_sha256 text,
  executor text NOT NULL,
  executor_version integer NOT NULL,
  started_at timestamptz NOT NULL,
  finished_at timestamptz NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id, task_id, attempt, lease_generation),
  FOREIGN KEY (tenant_id, task_id) REFERENCES enrichedlab_agent.broker_dataset_requests (tenant_id, task_id),
  CHECK (attempt >= 1),
  CHECK (lease_generation > 0),
  CHECK (status IN ('profiled', 'prepared', 'refused', 'failed')),
  CHECK ((status IN ('profiled', 'prepared')) = (error_code IS NULL)),
  CHECK ((status IN ('profiled', 'prepared')) = (package_sha256 IS NOT NULL)),
  CHECK (error_code IS NULL OR error_code ~ '^[a-z][a-z0-9_]{2,63}$'),
  CHECK (profile IS NULL OR (status = 'profiled' AND jsonb_typeof(profile) = 'object'
    AND octet_length(profile::text) <= 2097152)),
  CHECK (summary IS NULL OR (jsonb_typeof(summary) = 'object' AND octet_length(summary::text) <= 1048576)),
  CHECK (grid_sha256 IS NULL OR grid_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK ((status = 'prepared') = (table_sha256 IS NOT NULL)),
  CHECK ((table_sha256 IS NULL) = (table_path IS NULL) AND (table_sha256 IS NULL) = (table_rows IS NULL)
    AND (table_sha256 IS NULL) = (table_columns IS NULL)),
  CHECK (table_path IS NULL OR table_path ~ '^results/lab_data/[a-z][a-z0-9_]{0,62}\.csv$'),
  CHECK (table_sha256 IS NULL OR table_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (table_rows IS NULL OR table_rows BETWEEN 1 AND 1000000),
  CHECK (table_columns IS NULL OR (jsonb_typeof(table_columns) = 'array'
    AND jsonb_array_length(table_columns) BETWEEN 1 AND 1000 AND octet_length(table_columns::text) <= 262144)),
  CHECK (package_sha256 IS NULL OR package_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (executor ~ '^[a-z][a-z0-9.-]{0,63}$' AND executor_version >= 1),
  CHECK (finished_at >= started_at)
);


-- A dataset row records the guard's decision and never changes, except that an allowed file's intake task is set once,
-- when the lab's queue had no room at upload and a person starts the intake later.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_dataset_intake_once()
RETURNS trigger LANGUAGE plpgsql SET search_path = pg_catalog, enrichedlab_agent AS $$
BEGIN
  IF TG_OP = 'UPDATE' AND OLD.intake_task_id IS NULL AND NEW.intake_task_id IS NOT NULL
    AND (to_jsonb(NEW) - 'intake_task_id') = (to_jsonb(OLD) - 'intake_task_id') THEN
    RETURN NEW;
  END IF;
  RAISE EXCEPTION 'broker task events are append-only';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_dataset_intake_once() FROM PUBLIC;
DO $$ BEGIN
  ALTER TABLE enrichedlab_agent.broker_datasets ENABLE ROW LEVEL SECURITY;
  ALTER TABLE enrichedlab_agent.broker_dataset_requests ENABLE ROW LEVEL SECURITY;
  ALTER TABLE enrichedlab_agent.broker_dataset_receipts ENABLE ROW LEVEL SECURITY;
  REVOKE ALL ON enrichedlab_agent.broker_datasets FROM PUBLIC;
  REVOKE ALL ON enrichedlab_agent.broker_dataset_requests FROM PUBLIC;
  REVOKE ALL ON enrichedlab_agent.broker_dataset_receipts FROM PUBLIC;
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_datasets;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_datasets
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.broker_dataset_intake_once();
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_dataset_requests;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_dataset_requests
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
  DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_dataset_receipts;
  CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_dataset_receipts
    FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();
END $$;

-- The lab's dataset method: exactly one enabled profile with route `server-dataset` granted to the project's lab.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_dataset_method(p_tenant text, p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  v_group text;
  v_count integer;
  v_profile text;
BEGIN
  SELECT group_key INTO v_group FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = p_project;
  SELECT count(*), min(p.profile_key) INTO v_count, v_profile
    FROM enrichedlab_agent.broker_agent_profiles p
    JOIN enrichedlab_agent.broker_group_profile_grants g ON g.tenant_id = p.tenant_id AND g.profile_key = p.profile_key
    WHERE p.tenant_id = p_tenant AND p.enabled AND p.route = 'server-dataset' AND g.group_key = v_group;
  IF v_count <> 1 THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:dataset_method_unavailable';
  END IF;
  RETURN jsonb_build_object('group', v_group, 'profile', v_profile);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_dataset_method(text, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_dataset_json(d enrichedlab_agent.broker_datasets)
RETURNS jsonb LANGUAGE sql STABLE SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT jsonb_build_object('dataset_id', d.dataset_id, 'project_id', d.project_id, 'request_id', d.request_id,
    'file_name', d.file_name, 'media_type', d.media_type, 'size_bytes', d.size_bytes, 'sha256', d.sha256,
    'guard', jsonb_build_object('intake', d.guard_intake, 'outcome', d.guard_outcome, 'findings', d.guard_findings,
      'policy', d.guard_policy),
    'grid_sha256', d.grid_sha256, 'grid_bytes', d.grid_bytes, 'intake_task', d.intake_task_id,
    'intake_waiting', d.guard_outcome = 'allowed' AND d.intake_task_id IS NULL,
    'principal_id', d.principal_id, 'created_at', d.created_at)
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_dataset_json(enrichedlab_agent.broker_datasets) FROM PUBLIC;

-- API, before the file goes to the guard: the person may contribute to the project and the lab has its dataset
-- method. A request id already used in the project returns that dataset, so a retry never sends the file again.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_dataset_upload_check(p_tenant text, p_actor uuid, p_project uuid,
  p_request_id uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  d enrichedlab_agent.broker_datasets%ROWTYPE;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'contribute') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:person_required';
  END IF;
  SELECT * INTO d FROM enrichedlab_agent.broker_datasets
    WHERE tenant_id = p_tenant AND project_id = p_project AND request_id = p_request_id;
  IF FOUND THEN
    RETURN jsonb_build_object('existing', enrichedlab_agent.broker_dataset_json(d));
  END IF;
  RETURN enrichedlab_agent.broker_dataset_method(p_tenant, p_project);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_dataset_upload_check(text, uuid, uuid, uuid) FROM PUBLIC;

-- The intake task of an allowed dataset, submitted for `p_actor` and linked to the dataset once; the caller holds the
-- project's lock. Raises `broker:group_queue_full` when the lab's queue has no room.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_dataset_submit_intake(p_tenant text, p_actor uuid,
  d enrichedlab_agent.broker_datasets, m jsonb)
RETURNS enrichedlab_agent.broker_datasets LANGUAGE plpgsql SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  v_title text := 'Profile of ' || d.file_name;
  v_request jsonb;
  v_sha text;
  v_task uuid;
  out enrichedlab_agent.broker_datasets%ROWTYPE;
BEGIN
  v_request := jsonb_build_object('schema', 'enrichedlab.dataset-intake-request.v1', 'project', d.project_id,
    'dataset', d.dataset_id, 'file_sha256', d.sha256, 'guard_intake', d.guard_intake, 'grid_sha256', d.grid_sha256,
    'grid_bytes', d.grid_bytes);
  v_sha := encode(sha256(convert_to(v_request::text, 'UTF8')), 'hex');
  v_task := enrichedlab_agent.broker_submit_project_task(p_tenant, p_actor, d.project_id,
    'dataset-' || d.request_id::text, NULL, m->>'group', m->>'profile', left(v_title, 200),
    'dataset-' || d.request_id::text, v_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id, task_id, origin, basis, principal_id)
    VALUES (p_tenant, v_task, 'person', 'submission', p_actor) ON CONFLICT (tenant_id, task_id) DO NOTHING;
  UPDATE enrichedlab_agent.broker_datasets SET intake_task_id = v_task
    WHERE tenant_id = p_tenant AND dataset_id = d.dataset_id AND intake_task_id IS NULL
    RETURNING * INTO out;
  INSERT INTO enrichedlab_agent.broker_dataset_requests (tenant_id, task_id, project_id, dataset_id, operation, title,
    request, request_sha256, principal_id)
  VALUES (p_tenant, v_task, d.project_id, d.dataset_id, 'intake', left(v_title, 200), v_request, v_sha, p_actor);
  RETURN out;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_dataset_submit_intake(text, uuid, enrichedlab_agent.broker_datasets, jsonb)
  FROM PUBLIC;

-- API: a person starts the intake of an allowed dataset that waited for room in the lab's queue. A dataset whose intake
-- has started is returned as it is.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_start_dataset_intake(p_tenant text, p_actor uuid, p_project uuid,
  p_dataset uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  d enrichedlab_agent.broker_datasets%ROWTYPE;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = p_project FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'contribute') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:person_required';
  END IF;
  SELECT * INTO d FROM enrichedlab_agent.broker_datasets
    WHERE tenant_id = p_tenant AND dataset_id = p_dataset AND project_id = p_project;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:dataset_not_found';
  END IF;
  IF d.guard_outcome <> 'allowed' THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:dataset_not_allowed';
  END IF;
  IF d.intake_task_id IS NULL THEN
    d := enrichedlab_agent.broker_dataset_submit_intake(p_tenant, p_actor, d,
      enrichedlab_agent.broker_dataset_method(p_tenant, p_project));
  END IF;
  RETURN enrichedlab_agent.broker_dataset_json(d);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_start_dataset_intake(text, uuid, uuid, uuid) FROM PUBLIC;

-- API, after the guard decided: the dataset with the guard's decision and, for an allowed file, its intake task, in one
-- transaction under the project's lock. The same request id with the same file returns the same dataset.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_dataset(p_tenant text, p_actor uuid, p_project uuid,
  p_request_id uuid, p_file_name text, p_media_type text, p_size bigint, p_sha256 text, p_guard_intake uuid,
  p_outcome text, p_findings jsonb, p_policy text, p_grid_sha256 text, p_grid_bytes bigint)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  d enrichedlab_agent.broker_datasets%ROWTYPE;
  m jsonb;
  v_dataset uuid := gen_random_uuid();
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = p_project FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'contribute') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:person_required';
  END IF;
  SELECT * INTO d FROM enrichedlab_agent.broker_datasets
    WHERE tenant_id = p_tenant AND project_id = p_project AND request_id = p_request_id;
  IF FOUND THEN
    IF d.sha256 IS DISTINCT FROM p_sha256 OR d.file_name IS DISTINCT FROM p_file_name
      OR d.media_type IS DISTINCT FROM p_media_type THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
    RETURN enrichedlab_agent.broker_dataset_json(d);
  END IF;
  m := enrichedlab_agent.broker_dataset_method(p_tenant, p_project);
  -- The guard's decision is kept whatever happens to the intake: the guard has decided and holds the file.
  INSERT INTO enrichedlab_agent.broker_datasets (tenant_id, dataset_id, project_id, request_id, file_name, media_type,
    size_bytes, sha256, guard_intake, guard_outcome, guard_findings, guard_policy, grid_sha256, grid_bytes,
    intake_task_id, principal_id)
  VALUES (p_tenant, v_dataset, p_project, p_request_id, p_file_name, p_media_type, p_size, p_sha256, p_guard_intake,
    p_outcome, coalesce(p_findings, '[]'::jsonb), p_policy, p_grid_sha256, p_grid_bytes, NULL, p_actor)
  RETURNING * INTO d;
  IF p_outcome = 'allowed' THEN
    BEGIN
      d := enrichedlab_agent.broker_dataset_submit_intake(p_tenant, p_actor, d, m);
    EXCEPTION WHEN raise_exception THEN
      -- A full queue leaves the intake waiting for a person to start it; anything else undoes the record.
      IF SQLERRM <> 'broker:group_queue_full' THEN
        RAISE;
      END IF;
    END;
  END IF;
  RETURN enrichedlab_agent.broker_dataset_json(d);
EXCEPTION WHEN check_violation THEN
  RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:dataset_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_dataset(text, uuid, uuid, uuid, text, text, bigint, text, uuid,
  text, jsonb, text, text, bigint) FROM PUBLIC;

-- API: a person's preparation of a profiled dataset, with the specification they approved, recorded with the task it
-- creates in one transaction. The specification's identity is the digest of its stored JSON. A repeat with the same
-- request id is judged on what the person sent.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_request_dataset_preparation(p_tenant text, p_actor uuid,
  p_project uuid, p_dataset uuid, p_spec jsonb, p_title text, p_previous uuid, p_request_id uuid)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  d enrichedlab_agent.broker_datasets%ROWTYPE;
  v_prior enrichedlab_agent.broker_dataset_requests%ROWTYPE;
  m jsonb;
  v_title text := btrim(coalesce(p_title, ''));
  v_spec_sha text;
  v_request jsonb;
  v_sha text;
  v_task uuid;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id = p_tenant AND project_id = p_project FOR UPDATE;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'contribute') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:person_required';
  END IF;
  SELECT r.* INTO v_prior FROM enrichedlab_agent.broker_project_tasks pt
    JOIN enrichedlab_agent.broker_dataset_requests r ON r.tenant_id = pt.tenant_id AND r.task_id = pt.task_id
    WHERE pt.tenant_id = p_tenant AND pt.project_id = p_project AND pt.action_key = 'preparation-' || p_request_id::text;
  IF FOUND THEN
    IF v_prior.dataset_id IS DISTINCT FROM p_dataset OR v_prior.spec IS DISTINCT FROM p_spec
      OR v_prior.title IS DISTINCT FROM v_title OR v_prior.previous_task_id IS DISTINCT FROM p_previous THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
    RETURN v_prior.task_id;
  END IF;
  SELECT * INTO d FROM enrichedlab_agent.broker_datasets
    WHERE tenant_id = p_tenant AND dataset_id = p_dataset AND project_id = p_project;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:dataset_not_found';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_dataset_receipts x
      JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = x.tenant_id AND t.task_id = x.task_id
      WHERE x.tenant_id = p_tenant AND x.task_id = d.intake_task_id AND x.status = 'profiled' AND t.status = 'completed') THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:dataset_not_profiled';
  END IF;
  IF jsonb_typeof(p_spec) IS DISTINCT FROM 'object' OR octet_length(p_spec::text) > 262144
    OR p_spec->>'schema' IS DISTINCT FROM 'enrichedlab.dataset-preparation.v1' OR length(v_title) > 200 THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:dataset_preparation_invalid';
  END IF;
  IF p_previous IS NOT NULL AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_dataset_requests r
      WHERE r.tenant_id = p_tenant AND r.task_id = p_previous AND r.dataset_id = p_dataset
        AND r.operation = 'preparation') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:dataset_previous_invalid';
  END IF;
  m := enrichedlab_agent.broker_dataset_method(p_tenant, p_project);
  v_spec_sha := encode(sha256(convert_to(p_spec::text, 'UTF8')), 'hex');
  v_request := jsonb_build_object('schema', 'enrichedlab.dataset-preparation-request.v1', 'project', p_project,
    'dataset', p_dataset, 'grid_sha256', d.grid_sha256, 'intake_task', d.intake_task_id, 'spec_sha256', v_spec_sha,
    'title', v_title, 'previous', p_previous);
  v_sha := encode(sha256(convert_to(v_request::text, 'UTF8')), 'hex');
  v_task := enrichedlab_agent.broker_submit_project_task(p_tenant, p_actor, p_project,
    'preparation-' || p_request_id::text, d.intake_task_id, m->>'group', m->>'profile',
    CASE WHEN v_title <> '' THEN v_title ELSE left('Prepared table from ' || d.file_name, 200) END,
    'preparation-' || p_request_id::text, v_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id, task_id, origin, basis, principal_id)
    VALUES (p_tenant, v_task, 'person', 'submission', p_actor) ON CONFLICT (tenant_id, task_id) DO NOTHING;
  INSERT INTO enrichedlab_agent.broker_dataset_requests (tenant_id, task_id, project_id, dataset_id, operation, spec,
    spec_sha256, title, previous_task_id, request, request_sha256, principal_id)
  VALUES (p_tenant, v_task, p_project, p_dataset, 'preparation', p_spec, v_spec_sha, v_title, p_previous, v_request,
    v_sha, p_actor);
  RETURN v_task;
EXCEPTION WHEN check_violation THEN
  RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:dataset_preparation_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_request_dataset_preparation(text, uuid, uuid, uuid, jsonb, text, uuid,
  uuid) FROM PUBLIC;

-- Worker: the request of its current, unexpired lease with what it reads: the guard's intake and grid for an intake;
-- the intake package's place in the artifact store, the grid's digest and the specification's stored text, whose digest
-- the request recorded, for a preparation.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_dataset(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint)
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT jsonb_build_object('operation', r.operation, 'dataset', d.dataset_id, 'file_name', d.file_name,
    'media_type', d.media_type, 'guard_intake', d.guard_intake, 'grid_sha256', d.grid_sha256,
    'grid_bytes', d.grid_bytes, 'spec_text', r.spec::text, 'spec_sha256', r.spec_sha256, 'title', r.title,
    'request_sha256', r.request_sha256, 'intake_task', d.intake_task_id,
    'intake_package', (SELECT jsonb_build_object('path', a.relative_path, 'bytes', a.size_bytes, 'sha256', a.sha256)
      FROM enrichedlab_agent.broker_dataset_receipts x
      JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id = x.tenant_id AND a.task_id = x.task_id
        AND a.sha256 = x.package_sha256 AND a.role = 'result'
      WHERE r.operation = 'preparation' AND x.tenant_id = d.tenant_id AND x.task_id = d.intake_task_id
        AND x.status = 'profiled'
      ORDER BY x.attempt DESC, x.lease_generation DESC LIMIT 1))
  FROM enrichedlab_agent.broker_tasks t
  JOIN enrichedlab_agent.broker_dataset_requests r USING (tenant_id, task_id)
  JOIN enrichedlab_agent.broker_datasets d ON d.tenant_id = r.tenant_id AND d.dataset_id = r.dataset_id
  WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
    AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running') AND t.desired_status = 'run'
    AND t.lease_expires_at > CURRENT_TIMESTAMP
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_dataset(text, uuid, text, integer, bigint) FROM PUBLIC;

-- Worker: one attempt's receipt, also while a cancellation the worker is answering waits for it. A repeat must match.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_record_dataset_receipt(p_tenant text, p_task uuid, p_worker text,
  p_attempt integer, p_generation bigint, p_status text, p_error text, p_profile jsonb, p_summary jsonb,
  p_grid_sha256 text, p_table_path text, p_table_sha256 text, p_table_rows bigint, p_table_columns jsonb,
  p_package_sha256 text, p_executor text, p_executor_version integer, p_started timestamptz, p_finished timestamptz)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  v enrichedlab_agent.broker_dataset_receipts%ROWTYPE;
  v_operation text;
BEGIN
  SELECT r.operation INTO v_operation FROM enrichedlab_agent.broker_tasks t
    JOIN enrichedlab_agent.broker_dataset_requests r USING (tenant_id, task_id)
    WHERE t.tenant_id = p_tenant AND t.task_id = p_task AND t.lease_owner = p_worker AND t.attempt = p_attempt
      AND t.lease_generation = p_generation AND t.status IN ('leased', 'starting', 'running', 'cancelling')
      AND t.lease_expires_at > clock_timestamp()
    FOR UPDATE OF t;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  IF p_finished < p_started OR p_started > clock_timestamp() + interval '1 minute'
    OR (p_status = 'profiled' AND v_operation <> 'intake') OR (p_status = 'prepared' AND v_operation <> 'preparation') THEN
    RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:dataset_receipt_invalid';
  END IF;
  SELECT * INTO v FROM enrichedlab_agent.broker_dataset_receipts
  WHERE tenant_id = p_tenant AND task_id = p_task AND attempt = p_attempt AND lease_generation = p_generation;
  IF FOUND THEN
    IF v.status IS DISTINCT FROM p_status OR v.error_code IS DISTINCT FROM p_error OR v.profile IS DISTINCT FROM p_profile
      OR v.summary IS DISTINCT FROM p_summary OR v.grid_sha256 IS DISTINCT FROM p_grid_sha256
      OR v.table_path IS DISTINCT FROM p_table_path OR v.table_sha256 IS DISTINCT FROM p_table_sha256
      OR v.table_rows IS DISTINCT FROM p_table_rows OR v.table_columns IS DISTINCT FROM p_table_columns
      OR v.package_sha256 IS DISTINCT FROM p_package_sha256 OR v.executor IS DISTINCT FROM p_executor
      OR v.executor_version IS DISTINCT FROM p_executor_version OR v.started_at IS DISTINCT FROM p_started
      OR v.finished_at IS DISTINCT FROM p_finished THEN
      RAISE EXCEPTION USING ERRCODE = '23505', MESSAGE = 'broker:idempotency_conflict';
    END IF;
  ELSE
    INSERT INTO enrichedlab_agent.broker_dataset_receipts (tenant_id, task_id, attempt, lease_generation, status,
      error_code, profile, summary, grid_sha256, table_path, table_sha256, table_rows, table_columns, package_sha256,
      executor, executor_version, started_at, finished_at)
    VALUES (p_tenant, p_task, p_attempt, p_generation, p_status, p_error, p_profile, p_summary, p_grid_sha256,
      p_table_path, p_table_sha256, p_table_rows, p_table_columns, p_package_sha256, p_executor, p_executor_version,
      p_started, p_finished)
    RETURNING * INTO v;
  END IF;
  RETURN jsonb_build_object('status', v.status, 'recorded_at', v.recorded_at);
EXCEPTION WHEN check_violation THEN
  RAISE EXCEPTION USING ERRCODE = '22023', MESSAGE = 'broker:dataset_receipt_invalid';
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_record_dataset_receipt(text, uuid, text, integer, bigint, text, text,
  jsonb, jsonb, text, text, text, bigint, jsonb, text, text, integer, timestamptz, timestamptz) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_dataset_receipts_json(p_tenant text, p_task uuid)
RETURNS jsonb LANGUAGE sql STABLE SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT coalesce(jsonb_agg(jsonb_build_object('attempt', x.attempt, 'status', x.status, 'error_code', x.error_code,
      'profile', x.profile, 'summary', x.summary, 'grid_sha256', x.grid_sha256, 'table_path', x.table_path,
      'table_sha256', x.table_sha256, 'table_rows', x.table_rows, 'table_columns', x.table_columns,
      'package_sha256', x.package_sha256, 'executor', x.executor, 'executor_version', x.executor_version,
      'started_at', x.started_at, 'finished_at', x.finished_at) ORDER BY x.attempt, x.lease_generation), '[]'::jsonb)
  FROM enrichedlab_agent.broker_dataset_receipts x WHERE x.tenant_id = p_tenant AND x.task_id = p_task
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_dataset_receipts_json(text, uuid) FROM PUBLIC;

-- API: the project's datasets, newest first, each with its intake's status and newest receipt and its preparations,
-- for anyone who may read the project.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_datasets(p_tenant text, p_actor uuid, p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  RETURN coalesce((SELECT jsonb_agg(enrichedlab_agent.broker_dataset_json(d) || jsonb_build_object(
      'intake', CASE WHEN d.intake_task_id IS NULL THEN NULL ELSE (SELECT jsonb_build_object('task_id', t.task_id,
        'status', t.status, 'receipt', (SELECT jsonb_build_object('status', x.status, 'error_code', x.error_code,
            'sheets', jsonb_array_length(coalesce(x.profile->'sheets', '[]'::jsonb)), 'finished_at', x.finished_at)
          FROM enrichedlab_agent.broker_dataset_receipts x WHERE x.tenant_id = t.tenant_id AND x.task_id = t.task_id
          ORDER BY x.attempt DESC, x.lease_generation DESC LIMIT 1))
        FROM enrichedlab_agent.broker_tasks t WHERE t.tenant_id = d.tenant_id AND t.task_id = d.intake_task_id) END,
      'preparations', coalesce((SELECT jsonb_agg(jsonb_build_object('task_id', r.task_id, 'title', r.title,
          'table_name', r.spec->>'table_name', 'previous', r.previous_task_id, 'status', t.status,
          'created_at', r.created_at, 'receipt', (SELECT jsonb_build_object('status', x.status,
            'error_code', x.error_code, 'table_path', x.table_path, 'table_sha256', x.table_sha256,
            'table_rows', x.table_rows, 'finished_at', x.finished_at)
            FROM enrichedlab_agent.broker_dataset_receipts x WHERE x.tenant_id = r.tenant_id AND x.task_id = r.task_id
            ORDER BY x.attempt DESC, x.lease_generation DESC LIMIT 1)) ORDER BY r.created_at DESC, r.task_id)
        FROM enrichedlab_agent.broker_dataset_requests r
        JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = r.tenant_id AND t.task_id = r.task_id
        WHERE r.tenant_id = d.tenant_id AND r.dataset_id = d.dataset_id AND r.operation = 'preparation'), '[]'::jsonb))
    ORDER BY d.created_at DESC, d.dataset_id)
    FROM enrichedlab_agent.broker_datasets d WHERE d.tenant_id = p_tenant AND d.project_id = p_project), '[]'::jsonb);
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_datasets(text, uuid, uuid) FROM PUBLIC;

-- API: one dataset in full: the guard's decision, the intake's receipts with the profile, and each preparation with its
-- specification and receipts, for the people who may read the project.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_dataset_detail(p_tenant text, p_actor uuid, p_project uuid,
  p_dataset uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  d enrichedlab_agent.broker_datasets%ROWTYPE;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant, p_actor, p_project, 'read') THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:project_not_found';
  END IF;
  -- The detail holds the lab's cells (profile names, recodes, unread cells): the project's people read it, not a
  -- service account that may read the project.
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) THEN
    RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'broker:person_required';
  END IF;
  SELECT * INTO d FROM enrichedlab_agent.broker_datasets
    WHERE tenant_id = p_tenant AND dataset_id = p_dataset AND project_id = p_project;
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'broker:dataset_not_found';
  END IF;
  RETURN enrichedlab_agent.broker_dataset_json(d) || jsonb_build_object(
    'intake', CASE WHEN d.intake_task_id IS NULL THEN NULL ELSE (SELECT jsonb_build_object('task_id', t.task_id,
      'status', t.status, 'error_code', t.error_code,
      'receipts', enrichedlab_agent.broker_dataset_receipts_json(p_tenant, t.task_id))
      FROM enrichedlab_agent.broker_tasks t WHERE t.tenant_id = p_tenant AND t.task_id = d.intake_task_id) END,
    'preparations', coalesce((SELECT jsonb_agg(jsonb_build_object('task_id', r.task_id, 'title', r.title,
        'spec', r.spec, 'spec_sha256', r.spec_sha256, 'previous', r.previous_task_id, 'status', t.status,
        'error_code', t.error_code, 'request_sha256', r.request_sha256, 'principal_id', r.principal_id,
        'created_at', r.created_at, 'receipts', enrichedlab_agent.broker_dataset_receipts_json(p_tenant, r.task_id))
        ORDER BY r.created_at DESC, r.task_id)
      FROM enrichedlab_agent.broker_dataset_requests r
      JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = r.tenant_id AND t.task_id = r.task_id
      WHERE r.tenant_id = p_tenant AND r.dataset_id = d.dataset_id AND r.operation = 'preparation'), '[]'::jsonb));
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_dataset_detail(text, uuid, uuid, uuid) FROM PUBLIC;

-- The dataset method, like the figure and template-analysis methods, runs only from a person's request: the profile
-- choices mark it not offered, person submissions refuse it and a research run cannot be given it. One function now
-- names these routes.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_profile_choices(p_tenant text, p_principal uuid)
RETURNS TABLE(profile_key text, name text, description text, tool_bundle text, max_run_seconds integer, offered boolean)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT DISTINCT profiles.profile_key, profiles.name, profiles.description,
         profiles.tool_bundle, profiles.max_run_seconds, NOT enrichedlab_agent.broker_request_only_route(profiles.route)
  FROM enrichedlab_agent.broker_group_memberships AS memberships
  JOIN enrichedlab_agent.broker_group_profile_grants AS grants
    ON grants.tenant_id = memberships.tenant_id
   AND grants.group_key = memberships.group_key
  JOIN enrichedlab_agent.broker_agent_profiles AS profiles
    ON profiles.tenant_id = grants.tenant_id
   AND profiles.profile_key = grants.profile_key
  WHERE memberships.tenant_id = p_tenant
    AND memberships.principal_id = p_principal
    AND profiles.enabled
  ORDER BY profiles.name, profiles.profile_key
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_profile_choices(text, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_submit_person_task(p_tenant text,p_actor uuid,p_group text,p_profile text,p_objective text,p_key text,p_sha text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE task uuid;
BEGIN
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_agent_profiles
      WHERE tenant_id=p_tenant AND profile_key=p_profile AND enrichedlab_agent.broker_request_only_route(route)) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden';
  END IF;
  task:=enrichedlab_agent.broker_submit_task(p_tenant,p_actor,p_group,p_profile,p_objective,p_key,p_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id,task_id,origin,basis,principal_id)
    VALUES(p_tenant,task,'person','submission',p_actor) ON CONFLICT (tenant_id,task_id) DO NOTHING;
  RETURN task;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_submit_person_project_task(p_tenant text,p_actor uuid,p_project uuid,p_action text,p_parent uuid,p_group text,p_profile text,p_objective text,p_key text,p_sha text)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE task uuid;
BEGIN
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_agent_profiles
      WHERE tenant_id=p_tenant AND profile_key=p_profile AND enrichedlab_agent.broker_request_only_route(route)) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden';
  END IF;
  task:=enrichedlab_agent.broker_submit_project_task(p_tenant,p_actor,p_project,p_action,p_parent,p_group,p_profile,p_objective,p_key,p_sha);
  INSERT INTO enrichedlab_agent.broker_task_origins(tenant_id,task_id,origin,basis,principal_id)
    VALUES(p_tenant,task,'person','submission',p_actor) ON CONFLICT (tenant_id,task_id) DO NOTHING;
  RETURN task;
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_authorized(p_tenant text,p_actor uuid,p_project uuid,p_profiles text[])
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'steer')
    AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'contribute')
    AND cardinality(p_profiles) BETWEEN 1 AND 16
    AND NOT EXISTS (
      SELECT 1 FROM unnest(p_profiles) k WHERE k IS NULL OR NOT EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_projects p
        JOIN enrichedlab_agent.broker_group_profile_grants g USING(tenant_id,group_key)
        JOIN enrichedlab_agent.broker_agent_profiles f USING(tenant_id,profile_key)
        WHERE p.tenant_id=p_tenant AND p.project_id=p_project AND f.profile_key=k AND f.enabled
          AND NOT enrichedlab_agent.broker_request_only_route(f.route)
      )
    )
$$;

-- The project's task items (migration 058) with one addition: a dataset's intake or preparation names its dataset,
-- operation and the preparation it changes.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_task_items(p_tenant text, p_project uuid,
  p_after_submitted timestamptz, p_after_task uuid, p_limit integer)
RETURNS SETOF jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, enrichedlab_agent AS $$
  WITH line AS (
    SELECT q.task_id, row_number() OVER (ORDER BY q.submitted_at, q.task_id) - 1 AS ahead
    FROM enrichedlab_agent.broker_tasks q
    JOIN enrichedlab_agent.broker_research_groups qg ON qg.tenant_id = q.tenant_id AND qg.group_key = q.group_key
    WHERE q.tenant_id = p_tenant AND q.status = 'queued' AND q.desired_status = 'run'
      AND q.attempt < q.max_task_attempts AND qg.enabled
      AND enrichedlab_agent.broker_execution_allowed(q.tenant_id, q.task_id)
  )
  SELECT to_jsonb(t) FROM (
    SELECT t.task_id, pt.action_key, pt.parent_task_id, t.profile_key AS profile, t.status, t.attempt, t.updated_at,
      t.submitted_at,
      CASE WHEN fr.task_id IS NULL THEN NULL ELSE jsonb_build_object('title', t.objective, 'feature', fr.feature,
        'source_task', fr.source_task_id, 'table_path', fr.table_path, 'previous', fr.previous_task_id) END AS figure,
      CASE WHEN ar.task_id IS NULL THEN NULL ELSE jsonb_build_object('title', t.objective, 'feature', ar.feature,
        'source_task', ar.source_task_id, 'table_path', ar.table_path, 'previous', ar.previous_task_id)
        END AS template_analysis,
      CASE WHEN dr.task_id IS NULL THEN NULL ELSE jsonb_build_object('title', t.objective, 'operation', dr.operation,
        'dataset', dr.dataset_id, 'previous', dr.previous_task_id) END AS dataset,
      CASE WHEN t.status = 'queued' AND t.desired_status = 'run' THEN jsonb_build_object(
        'ahead', line.ahead,
        'lab_full', coalesce((SELECT count(*) FROM enrichedlab_agent.broker_tasks a
          WHERE a.tenant_id = t.tenant_id AND a.group_key = t.group_key
            AND a.status IN ('leased', 'starting', 'running', 'cancelling') AND a.lease_expires_at >= now())
          >= g.max_active_agents, false),
        'person_full', coalesce((SELECT count(*) FROM enrichedlab_agent.broker_tasks a
          WHERE a.tenant_id = t.tenant_id AND a.group_key = t.group_key AND a.principal_id = t.principal_id
            AND a.status IN ('leased', 'starting', 'running', 'cancelling') AND a.lease_expires_at >= now())
          >= g.max_active_agents_per_principal, false)) END AS queue
    FROM enrichedlab_agent.broker_project_tasks pt
    JOIN enrichedlab_agent.broker_tasks t USING (tenant_id, task_id)
    LEFT JOIN enrichedlab_agent.broker_figure_requests fr ON fr.tenant_id = pt.tenant_id AND fr.task_id = pt.task_id
    LEFT JOIN enrichedlab_agent.broker_analysis_requests ar ON ar.tenant_id = pt.tenant_id AND ar.task_id = pt.task_id
    LEFT JOIN enrichedlab_agent.broker_dataset_requests dr ON dr.tenant_id = pt.tenant_id AND dr.task_id = pt.task_id
    LEFT JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id = t.tenant_id AND g.group_key = t.group_key
    LEFT JOIN line ON line.task_id = t.task_id
    WHERE pt.tenant_id = p_tenant AND pt.project_id = p_project
      AND (p_after_submitted IS NULL OR (t.submitted_at, t.task_id) < (p_after_submitted, p_after_task))
    ORDER BY t.submitted_at DESC, t.task_id DESC
    LIMIT p_limit + 1
  ) t;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_task_items(text, uuid, timestamptz, uuid, integer) FROM PUBLIC;

-- Task lineage (migration 058) with lab datasets: like a template analysis, a person's request is its basis; an intake
-- read the file a person sent (class `object_data`, as the guard rebuilt it) and a preparation the intake's kept grid
-- (`prior_output`); both ran the broker's reviewed executor (class `vetted_service`) in the image the method names. The
-- `dataset` record gives the file's digest, the guard's intake and policy, the grid, the specification's digest, the
-- executor and the prepared table's digest.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_lineage(p_tenant text, p_actor uuid, p_task uuid)
 RETURNS jsonb
 LANGUAGE plpgsql
 STABLE SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'enrichedlab_agent'
AS $function$
DECLARE t enrichedlab_agent.broker_tasks%ROWTYPE; project uuid; action text;
  o enrichedlab_agent.broker_task_origins%ROWTYPE; g enrichedlab_agent.broker_task_generated_code%ROWTYPE;
  m enrichedlab_agent.broker_task_methods%ROWTYPE;
  step jsonb; step_revision bigint; step_now jsonb; step_now_revision bigint; plan jsonb;
  workspace jsonb; workspace_revision bigint; turn_revision bigint; tool jsonb;
  tool_class text; output_class text; gaps jsonb:='[]'; outputs jsonb; has_template boolean;
  q enrichedlab_agent.broker_task_queries%ROWTYPE; src enrichedlab_agent.broker_sql_sources%ROWTYPE;
  classes jsonb:='[]'; classes_by text;
  fr enrichedlab_agent.broker_figure_requests%ROWTYPE; fx enrichedlab_agent.broker_figure_receipts%ROWTYPE;
  ar enrichedlab_agent.broker_analysis_requests%ROWTYPE; ax enrichedlab_agent.broker_analysis_receipts%ROWTYPE;
  dr enrichedlab_agent.broker_dataset_requests%ROWTYPE; dx enrichedlab_agent.broker_dataset_receipts%ROWTYPE;
  ds enrichedlab_agent.broker_datasets%ROWTYPE;
  account_name text; account_kind text; unpinned boolean;
BEGIN
  SELECT * INTO t FROM enrichedlab_agent.broker_tasks WHERE tenant_id=p_tenant AND task_id=p_task;
  IF NOT FOUND OR NOT enrichedlab_agent.broker_task_can(p_tenant,p_actor,p_task,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:task_not_found'; END IF;
  SELECT pt.project_id,pt.action_key INTO project,action FROM enrichedlab_agent.broker_project_tasks pt
    WHERE pt.tenant_id=p_tenant AND pt.task_id=p_task;
  SELECT * INTO o FROM enrichedlab_agent.broker_task_origins WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO g FROM enrichedlab_agent.broker_task_generated_code WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO m FROM enrichedlab_agent.broker_task_methods WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO q FROM enrichedlab_agent.broker_task_queries WHERE tenant_id=p_tenant AND task_id=p_task;
  SELECT * INTO fr FROM enrichedlab_agent.broker_figure_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF fr.task_id IS NOT NULL THEN
    SELECT * INTO fx FROM enrichedlab_agent.broker_figure_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
  END IF;
  SELECT * INTO ar FROM enrichedlab_agent.broker_analysis_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF ar.task_id IS NOT NULL THEN
    SELECT * INTO ax FROM enrichedlab_agent.broker_analysis_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
  END IF;
  SELECT * INTO dr FROM enrichedlab_agent.broker_dataset_requests WHERE tenant_id=p_tenant AND task_id=p_task;
  IF dr.task_id IS NOT NULL THEN
    SELECT * INTO dx FROM enrichedlab_agent.broker_dataset_receipts WHERE tenant_id=p_tenant AND task_id=p_task
      ORDER BY attempt DESC,lease_generation DESC LIMIT 1;
    SELECT * INTO ds FROM enrichedlab_agent.broker_datasets WHERE tenant_id=p_tenant AND dataset_id=dr.dataset_id;
  END IF;
  IF q.task_id IS NOT NULL THEN
    SELECT * INTO src FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id=p_tenant AND source_key=q.source_key;
  END IF;
  -- The display name only: a task reader may not hold the capability that shows emails.
  SELECT a.display_name,CASE WHEN a.principal_class <> 'person' THEN 'service_account' ELSE 'person' END
    INTO account_name,account_kind FROM enrichedlab_agent.broker_principals a
    WHERE a.tenant_id=p_tenant AND a.principal_id=coalesce(o.principal_id,t.principal_id);
  has_template:=m.template_ref IS NOT NULL;
  SELECT jsonb_build_object('tool_id',j.tool_id,'catalog_digest',j.catalog_digest,'input_sha256',j.input_sha256)
    INTO tool FROM enrichedlab_agent.broker_task_platform_tool_jobs j
    WHERE j.tenant_id=p_tenant AND j.task_id=p_task ORDER BY j.attempt DESC,j.lease_generation DESC LIMIT 1;

  IF o.task_id IS NULL THEN gaps:=gaps||'"origin_not_recorded"'::jsonb;
  ELSIF o.origin='unrecorded' THEN gaps:=gaps||'"origin_before_records"'::jsonb; END IF;
  IF m.task_id IS NULL THEN gaps:=gaps||'"method_not_recorded"'::jsonb;
  ELSIF m.basis='recorded_later' THEN gaps:=gaps||'"method_recorded_later"'::jsonb; END IF;

  IF project IS NULL THEN
    gaps:=gaps||'"not_in_project"'::jsonb;
  ELSIF fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL OR dr.task_id IS NOT NULL THEN
    NULL;
  ELSE
    IF o.origin='research_run' THEN
      SELECT u.checkpoint_revision INTO turn_revision FROM enrichedlab_agent.broker_research_turns u
        WHERE u.tenant_id=p_tenant AND u.run_id=o.run_id AND u.turn=o.turn;
    END IF;
    SELECT c.state->'workspace',c.revision INTO workspace,workspace_revision FROM enrichedlab_agent.broker_project_checkpoints c
      WHERE c.tenant_id=p_tenant AND c.project_id=project
        AND (CASE WHEN turn_revision IS NOT NULL THEN c.revision=turn_revision ELSE c.created_at<=t.submitted_at END)
      ORDER BY c.revision DESC LIMIT 1;
    IF workspace IS NULL OR jsonb_typeof(workspace)<>'object' THEN
      workspace:=NULL; gaps:=gaps||'"inputs_not_recorded"'::jsonb;
    END IF;
    -- The step as first recorded for this task, and the newest revision that names it.
    SELECT s,c.revision INTO step,step_revision FROM enrichedlab_agent.broker_project_checkpoints c
      CROSS JOIN LATERAL jsonb_array_elements(coalesce(c.state->'plan','[]'::jsonb)) s
      WHERE c.tenant_id=p_tenant AND c.project_id=project AND s->>'task_id'=p_task::text
      ORDER BY c.revision ASC LIMIT 1;
    SELECT s,c.revision INTO step_now,step_now_revision FROM enrichedlab_agent.broker_project_checkpoints c
      CROSS JOIN LATERAL jsonb_array_elements(coalesce(c.state->'plan','[]'::jsonb)) s
      WHERE c.tenant_id=p_tenant AND c.project_id=project AND s->>'task_id'=p_task::text
      ORDER BY c.revision DESC LIMIT 1;
    IF step IS NULL THEN gaps:=gaps||'"plan_step_not_recorded"'::jsonb; END IF;
  END IF;
  -- What kind of data the task read: a query's source as registered, otherwise the classes the input custodian recorded
  -- for the latest attempt's sealed copies. With neither, a task that had inputs names the gap.
  IF fr.task_id IS NOT NULL THEN
    classes:='["prior_output"]'::jsonb; classes_by:='render_request';
  ELSIF ar.task_id IS NOT NULL THEN
    classes:='["prior_output"]'::jsonb; classes_by:='analysis_request';
  ELSIF dr.task_id IS NOT NULL THEN
    -- An intake reads the file a person sent, as the guard rebuilt it; a preparation reads the intake's kept grid.
    classes:=CASE WHEN dr.operation='intake' THEN '["object_data"]'::jsonb ELSE '["prior_output"]'::jsonb END;
    classes_by:='dataset_request';
  ELSIF src.source_key IS NOT NULL THEN
    classes:=jsonb_build_array(src.data_class); classes_by:='query_source';
  ELSE
    SELECT coalesce(jsonb_agg(c ORDER BY c),'[]'::jsonb) INTO classes FROM (SELECT DISTINCT i.input_class c
      FROM enrichedlab_agent.broker_attempt_inputs i WHERE i.tenant_id=p_tenant AND i.task_id=p_task AND i.attempt=t.attempt) x;
    IF jsonb_array_length(classes)>0 THEN classes_by:='input_custodian'; END IF;
  END IF;
  IF (workspace IS NOT NULL OR tool IS NOT NULL) AND jsonb_array_length(classes)=0 THEN
    gaps:=gaps||'"input_class_not_recorded"'::jsonb; END IF;

  -- What ran decides the class first: a block rendered from a configured method's settings,
  -- admitted model-written code, then a reviewed platform tool's job. A template the task was
  -- submitted under, with none of these, ran no model-written code, which is a gap.
  tool_class:=CASE WHEN fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL OR dr.task_id IS NOT NULL THEN 'vetted_service'
    WHEN q.task_id IS NOT NULL THEN 'guarded_query'
    WHEN g.task_id IS NOT NULL AND g.configuration IS NOT NULL THEN 'configured'
    WHEN g.task_id IS NOT NULL THEN 'agent_built' WHEN tool IS NOT NULL THEN 'vetted_service'
    WHEN has_template THEN 'agent_built' ELSE 'agent_session' END;
  IF has_template AND g.task_id IS NULL AND tool IS NULL THEN gaps:=gaps||'"code_not_recorded"'::jsonb; END IF;
  output_class:=CASE tool_class WHEN 'agent_built' THEN 'agent_built_tool_output' WHEN 'vetted_service' THEN 'vetted_tool_output'
    WHEN 'configured' THEN 'configured_tool_output' WHEN 'guarded_query' THEN 'query_output' ELSE 'agent_session_output' END;
  -- A figure version or a template analysis ran in the render service, so the image it names is the one its newest
  -- receipt records, and only a completed one's receipt must name one (a refusal comes back before anything runs); any
  -- other task ran in the image its profile named.
  unpinned:=CASE WHEN fr.task_id IS NOT NULL THEN fx.status='completed' AND fx.image_digest IS NULL
    WHEN ar.task_id IS NOT NULL THEN ax.status='completed' AND ax.image_digest IS NULL
    ELSE coalesce(t.profile_snapshot->>'image_ref','') !~ '@sha256:[0-9a-f]{64}$' END;
  IF unpinned THEN
    gaps:=gaps||'"image_not_digest_pinned"'::jsonb; END IF;

  SELECT coalesce(jsonb_agg(jsonb_build_object('artifact_id',a.artifact_id,'role',a.role,'label',a.label,
      'relative_path',a.relative_path,'media_type',a.media_type,'size_bytes',a.size_bytes,'sha256',a.sha256,
      'created_at',a.created_at,'provenance_class',output_class) ORDER BY a.created_at,a.artifact_id),'[]'::jsonb)
    INTO outputs FROM enrichedlab_agent.broker_artifacts a
    WHERE a.tenant_id=p_tenant AND a.task_id=p_task AND a.visibility='researcher';
  -- A figure version or template analysis that did not complete has no files by design: the service refused its
  -- settings or R failed, and its receipt says so. Only a completed one without its files is a gap.
  IF jsonb_array_length(outputs)=0 AND (CASE WHEN fr.task_id IS NOT NULL OR ar.task_id IS NOT NULL OR dr.task_id IS NOT NULL
      THEN t.status='completed'
      ELSE t.status IN ('completed','failed','cancelled','review_required') END) THEN
    gaps:=gaps||'"no_outputs_recorded"'::jsonb; END IF;

  IF step IS NOT NULL THEN
    plan:=jsonb_build_object('action_key',step->>'action_key','title',step->>'title','rationale',step->>'rationale',
      'status',step_now->>'status','revision',step_revision,
      -- A step first recorded after the checkpoint the task was submitted against was added to the plan later.
      'added_after_submission',workspace_revision IS NOT NULL AND step_revision>workspace_revision,
      'current',CASE WHEN step_now_revision<>step_revision AND (step_now->>'title' IS DISTINCT FROM step->>'title'
          OR step_now->>'rationale' IS DISTINCT FROM step->>'rationale')
        THEN jsonb_build_object('title',step_now->>'title','rationale',step_now->>'rationale','revision',step_now_revision) END);
  END IF;

  RETURN jsonb_build_object(
    'schema','enrichedlab.task-lineage.v1',
    'task',jsonb_build_object('task_id',t.task_id,'project_id',project,'action_key',action,'objective',t.objective,
      'status',t.status,'result_class',t.result_class,'error_code',t.error_code,'attempt',t.attempt,
      'submitted_at',t.submitted_at,'started_at',t.started_at,'finished_at',t.finished_at),
    'origin',CASE WHEN o.task_id IS NULL THEN jsonb_build_object('origin','unrecorded','basis',NULL,'principal_id',t.principal_id)
      ELSE jsonb_build_object('origin',o.origin,'basis',o.basis,'principal_id',o.principal_id,'run_id',o.run_id,
        'turn',o.turn,'recorded_at',o.recorded_at) END
      ||jsonb_build_object('principal_name',account_name,'principal_kind',account_kind),
    'plan_step',plan,
    'inputs',jsonb_build_object('workspace',workspace,'checkpoint_revision',workspace_revision,'platform_tool',tool,
      'classes',classes,'classes_recorded_by',classes_by),
    'method',jsonb_build_object('profile',t.profile_key,'profile_name',m.profile_name,'tool_class',tool_class,
      'template_ref',coalesce(g.template_ref,m.template_ref),'template_sha256',coalesce(g.template_sha256,m.template_sha256),
      'recorded',m.basis,'harness',t.profile_snapshot->'harness',
      'role',t.profile_snapshot->'role','tool_bundle',t.profile_snapshot->'tool_bundle'),
    'code',CASE WHEN g.task_id IS NULL THEN NULL ELSE jsonb_build_object('path',g.path,'sha256',g.sha256,
      'size_bytes',g.size_bytes,'admission',g.admission,'configuration',g.configuration,'run_id',g.run_id,'turn',g.turn,
      'created_at',g.created_at) END,
    'query',CASE WHEN q.task_id IS NULL THEN NULL ELSE jsonb_build_object('source',q.source_key,'sha256',q.sql_sha256,
      'guard',q.guard,'relations',to_jsonb(q.relations),'row_limit',q.row_limit,'run_id',q.run_id,'turn',q.turn,
      'created_at',q.created_at,'title',src.title,'release',src.release,
      'snapshot_sha256',src.snapshot->>'manifest_sha256','data_class',src.data_class,
      'snapshot_kind',src.snapshot->>'kind','source_version',(SELECT r.source_version FROM enrichedlab_agent.broker_query_receipts r
        WHERE r.tenant_id=p_tenant AND r.task_id=p_task AND r.status='completed'
        ORDER BY r.attempt DESC,r.lease_generation DESC LIMIT 1)) END,
    'render',CASE WHEN fr.task_id IS NULL THEN NULL ELSE jsonb_build_object('source_task',fr.source_task_id,
      'table_path',fr.table_path,'table_sha256',fr.table_sha256,'source_package_sha256',fr.source_package_sha256,
      'feature',fr.feature,'controls',fr.controls,'title',fr.title,'previous',fr.previous_task_id,
      'request_sha256',fr.request_sha256,'created_at',fr.created_at,'service_version',fx.service_version,
      'image_digest',fx.image_digest) END,
    'analysis',CASE WHEN ar.task_id IS NULL THEN NULL ELSE jsonb_build_object('source_task',ar.source_task_id,
      'table_path',ar.table_path,'table_sha256',ar.table_sha256,'source_package_sha256',ar.source_package_sha256,
      'feature',ar.feature,'settings',ar.settings,'title',ar.title,'previous',ar.previous_task_id,
      'request_sha256',ar.request_sha256,'created_at',ar.created_at,'service_version',ax.service_version,
      'image_digest',ax.image_digest,'method',ax.method) END,
    'dataset',CASE WHEN dr.task_id IS NULL THEN NULL ELSE jsonb_build_object('dataset',dr.dataset_id,
      'operation',dr.operation,'file_name',ds.file_name,'file_sha256',ds.sha256,'guard_intake',ds.guard_intake,
      'guard_policy',ds.guard_policy,'grid_sha256',ds.grid_sha256,'spec_sha256',dr.spec_sha256,
      'previous',dr.previous_task_id,'title',dr.title,'request_sha256',dr.request_sha256,'created_at',dr.created_at,
      'executor',dx.executor,'executor_version',dx.executor_version,'table_sha256',dx.table_sha256) END,
    'execution',jsonb_build_object('image_ref',t.profile_snapshot->'image_ref','cpu_limit',t.profile_snapshot->'cpu_limit',
      'memory_limit',t.profile_snapshot->'memory_limit','max_run_seconds',t.profile_snapshot->'max_run_seconds',
      'attempts',t.attempt,'scientific_history',(SELECT count(*) FROM enrichedlab_agent.broker_task_scientific_history h
        WHERE h.tenant_id=p_tenant AND h.task_id=p_task)),
    'outputs',outputs,
    'review',jsonb_build_object('state',CASE WHEN tool_class IN ('agent_built','agent_session','configured','guarded_query') THEN 'unvalidated' ELSE 'not_reviewed' END),
    'gaps',gaps);
END $function$;

-- The inbox (migration 049) and the project list's counts (migration 054) leave out a lab dataset's intake and
-- preparation as they leave out figure versions: they ready the lab's data for analyses and are not analyses
-- themselves. A person reads them in the dataset view; the analyses made from a prepared table are reviewed as usual.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_inbox(p_tenant text, p_actor uuid)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  WITH projects AS (
    SELECT p.project_id, p.group_key, p.title, p.revision,
           enrichedlab_agent.broker_project_can(p_tenant, p_actor, p.project_id, 'steer') AS steer,
           enrichedlab_agent.broker_project_can(p_tenant, p_actor, p.project_id, 'review') AS review
    FROM enrichedlab_agent.broker_projects p
    WHERE p.tenant_id = p_tenant
  ),
  latest_runs AS (
    SELECT DISTINCT ON (r.project_id) r.project_id, r.run_id, r.status, r.code, r.turn, r.updated_at
    FROM enrichedlab_agent.broker_research_runs r
    WHERE r.tenant_id = p_tenant
    ORDER BY r.project_id, r.created_at DESC, r.run_id DESC
  ),
  -- The agent stopped and asks for a person: the newest run of a project the caller may steer or review.
  review_requests AS (
    SELECT 'review_request' AS kind, r.run_id::text AS ref, r.run_id::text AS version, r.updated_at AS at,
           jsonb_build_object('lab', p.group_key, 'project_id', p.project_id, 'project_title', p.title, 'run_id', r.run_id,
             'code', r.code, 'turn', r.turn,
             -- The recorded reason, in the agent's own words: the methods check's when it asked for review, else the
             -- lead's summary; none when the run stopped before a decision.
             'reason_source', CASE WHEN t.record->'method_review'->>'decision' IN ('needs_review', 'revise') THEN 'methods_check'
                                   WHEN t.record IS NOT NULL THEN 'lead' END,
             'reason', CASE WHEN t.record->'method_review'->>'decision' IN ('needs_review', 'revise') THEN t.record->'method_review'->>'reason'
                            ELSE t.record->'proposal'->>'summary' END) AS item
    FROM latest_runs r
    JOIN projects p ON p.project_id = r.project_id
    LEFT JOIN LATERAL (
      SELECT rt.record FROM enrichedlab_agent.broker_research_turns rt
      WHERE rt.tenant_id = p_tenant AND rt.run_id = r.run_id ORDER BY rt.turn DESC LIMIT 1
    ) t ON true
    WHERE r.status = 'review_required' AND (p.steer OR p.review)
  ),
  -- Finished analyses whose newest review by a person does not cover their current result files.
  analysis_reviews AS (
    SELECT 'analysis_review' AS kind, t.task_id::text AS ref,
           md5(enrichedlab_agent.broker_task_review_outputs(p_tenant, t.task_id)::text) AS version,
           coalesce(t.finished_at, t.updated_at) AS at,
           jsonb_build_object('lab', p.group_key, 'project_id', p.project_id, 'project_title', p.title, 'task_id', t.task_id,
             'action_key', pt.action_key, 'status', t.status) AS item
    FROM projects p
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = p_tenant AND pt.project_id = p.project_id
    JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = p_tenant AND t.task_id = pt.task_id
    WHERE p.review AND enrichedlab_agent.broker_is_person(p_tenant, p_actor)
      AND t.status IN ('completed', 'review_required')
      -- A figure version redraws results the project already holds, and a lab dataset's intake or preparation readies a
      -- lab's own data for analyses, so neither waits for review.
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr
        WHERE fr.tenant_id = p_tenant AND fr.task_id = t.task_id)
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_dataset_requests dr
        WHERE dr.tenant_id = p_tenant AND dr.task_id = t.task_id)
      AND NOT EXISTS (
        SELECT 1 FROM (
          SELECT rv.outputs FROM enrichedlab_agent.broker_task_reviews rv
          JOIN enrichedlab_agent.broker_principals a ON a.tenant_id = rv.tenant_id AND a.principal_id = rv.reviewer_id
          WHERE rv.tenant_id = p_tenant AND rv.task_id = t.task_id AND a.principal_class = 'person'
          ORDER BY rv.created_at DESC, rv.review_id DESC LIMIT 1
        ) newest
        WHERE newest.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, t.task_id))
  ),
  -- Steps of the current plan whose analysis the system could not confirm, versioned by the revision that marked them.
  step_checks AS (
    SELECT 'step_check' AS kind, p.project_id::text || ':' || (s->>'action_key') AS ref,
           coalesce((SELECT e.revision FROM enrichedlab_agent.broker_plan_events e
                     WHERE e.tenant_id = p_tenant AND e.project_id = p.project_id AND e.action_key = s->>'action_key'
                       AND e.status_after = 'uncertain' ORDER BY e.revision DESC LIMIT 1), p.revision)::text AS version,
           c.created_at AS at,
           jsonb_build_object('lab', p.group_key, 'project_id', p.project_id, 'project_title', p.title,
             'action_key', s->>'action_key', 'title', s->>'title', 'task_id', s->>'task_id') AS item
    FROM projects p
    JOIN enrichedlab_agent.broker_project_checkpoints c
      ON c.tenant_id = p_tenant AND c.project_id = p.project_id AND c.revision = p.revision
    CROSS JOIN LATERAL jsonb_array_elements(c.state->'plan') s
    WHERE p.steer AND s->>'status' = 'uncertain'
  ),
  -- Papers in a lab's library that no curator has accepted, for the lab's curators; one item per lab.
  paper_acceptance AS (
    SELECT 'paper_acceptance' AS kind, w.group_key AS ref, md5(string_agg(w.paper_id::text, ',' ORDER BY w.paper_id)) AS version,
           max(w.created_at) AS at, jsonb_build_object('lab', w.group_key, 'papers', count(*)) AS item
    FROM (
      SELECT lp.group_key, lp.paper_id, lp.created_at FROM enrichedlab_agent.broker_library_papers lp
      WHERE lp.tenant_id = p_tenant AND enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, lp.group_key)
        AND enrichedlab_agent.broker_library_removal(p_tenant, lp.paper_id) IS NULL
        AND enrichedlab_agent.broker_library_acceptance(p_tenant, lp.paper_id) IS NULL
    ) w
    GROUP BY w.group_key
  ),
  -- Shared notes the lead no longer reads because their review-by date passed or their evidence changed.
  note_attention AS (
    SELECT 'note_attention' AS kind, n.note_id::text AS ref,
           (x.state->>'state') || ':' || (x.state->>'review_by') || coalesce(':' || (x.state->>'task_id'), '') AS version,
           (x.state->>'review_by')::timestamptz AS at,
           jsonb_build_object('lab', n.group_key, 'note_id', n.note_id, 'project_id', n.project_id, 'state', x.state->>'state',
             'cause', x.state->>'cause', 'review_by', x.state->>'review_by') AS item
    FROM enrichedlab_agent.broker_lab_notes n
    CROSS JOIN LATERAL (SELECT enrichedlab_agent.broker_lab_note_state(p_tenant, n) AS state) x
    WHERE n.tenant_id = p_tenant AND enrichedlab_agent.broker_lab_curator(p_tenant, p_actor, n.group_key)
      AND x.state->>'state' IN ('expired', 'paused')
  ),
  items AS (
    SELECT * FROM review_requests UNION ALL SELECT * FROM analysis_reviews UNION ALL SELECT * FROM step_checks
    UNION ALL SELECT * FROM paper_acceptance UNION ALL SELECT * FROM note_attention
  )
  SELECT jsonb_build_object(
    'can_acknowledge', enrichedlab_agent.broker_is_person(p_tenant, p_actor),
    'items', coalesce(jsonb_agg(i.item || jsonb_build_object('kind', i.kind, 'ref', i.ref, 'version', i.version, 'at', i.at,
      'seen_at', a.acknowledged_at) ORDER BY i.at DESC, i.kind, i.ref), '[]'::jsonb))
  FROM items i
  LEFT JOIN enrichedlab_agent.broker_inbox_acknowledgements a
    ON a.tenant_id = p_tenant AND a.principal_id = p_actor AND a.kind = i.kind AND a.item_ref = i.ref AND a.item_version = i.version
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_inbox(text, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_list_projects(p_tenant text,p_actor uuid,p_limit integer)
RETURNS SETOF jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
BEGIN
  IF p_limit NOT BETWEEN 1 AND 200 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:list_limit_invalid'; END IF;
  RETURN QUERY SELECT (to_jsonb(p)-'tenant_id')||jsonb_build_object(
      'owner',jsonb_build_object('principal_id',o.principal_id,'display_name',o.display_name,
        'kind',CASE WHEN o.issuer='kubernetes://serviceaccount' THEN 'service_account' ELSE 'person' END),
      'question',(SELECT c.state->>'goal' FROM enrichedlab_agent.broker_project_checkpoints c
        WHERE c.tenant_id=p.tenant_id AND c.project_id=p.project_id AND c.revision=p.revision),
      'capabilities',(SELECT coalesce(jsonb_agg(k),'[]'::jsonb)
        FROM unnest(ARRAY['read','review','contribute','steer','cancel','manage']) k
        WHERE enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,k)),
      'last_look',enrichedlab_agent.broker_project_last_look(p.tenant_id,p.project_id,p_actor),
      'latest_run',(SELECT jsonb_build_object('run_id',r.run_id,'status',r.status,'code',r.code,'turn',r.turn,
          'max_turns',r.max_turns,'updated_at',r.updated_at)
        FROM enrichedlab_agent.broker_research_runs r
        WHERE r.tenant_id=p.tenant_id AND r.project_id=p.project_id ORDER BY r.created_at DESC,r.run_id LIMIT 1),
      'task_counts',(SELECT jsonb_build_object('total',count(*),
          'active',count(*) FILTER (WHERE t.status IN ('queued','leased','starting','running','cancelling')),
          'finished',count(*) FILTER (WHERE t.status IN ('completed','review_required')),
          'failed',count(*) FILTER (WHERE t.status='failed'),
          -- A finished analysis whose newest review by a person still covers its result files.
          'reviewed',count(*) FILTER (WHERE t.status IN ('completed','review_required') AND v.covers),
          'rejected',count(*) FILTER (WHERE t.status IN ('completed','review_required') AND v.covers AND v.decision='rejected'),
          'changes',count(*) FILTER (WHERE t.status IN ('completed','review_required') AND v.covers AND v.decision='needs_changes'))
        FROM enrichedlab_agent.broker_project_tasks pt
        JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=pt.tenant_id AND t.task_id=pt.task_id
        LEFT JOIN LATERAL (
          SELECT rv.decision, rv.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, rv.task_id) AS covers
          FROM enrichedlab_agent.broker_task_reviews rv
          JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=rv.tenant_id AND a.principal_id=rv.reviewer_id
          WHERE rv.tenant_id=pt.tenant_id AND rv.task_id=pt.task_id AND a.issuer<>'kubernetes://serviceaccount'
          ORDER BY rv.created_at DESC, rv.review_id DESC LIMIT 1) v ON true
        WHERE pt.tenant_id=p.tenant_id AND pt.project_id=p.project_id
          AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr
            WHERE fr.tenant_id=pt.tenant_id AND fr.task_id=pt.task_id)
          AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_dataset_requests dr
            WHERE dr.tenant_id=pt.tenant_id AND dr.task_id=pt.task_id)))
    FROM enrichedlab_agent.broker_projects p
    JOIN enrichedlab_agent.broker_principals o ON o.tenant_id=p.tenant_id AND o.principal_id=p.owner_id
    WHERE p.tenant_id=p_tenant AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,'read')
    ORDER BY p.updated_at DESC,p.project_id LIMIT p_limit;
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_projects(text,uuid,integer) FROM PUBLIC;

-- Whether a result table's path is under the lab's own folders: a prepared table (`results/lab_data/`) or rows an analysis
-- computed from one (`results/lab_derived/`).
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_is_lab_table_path(p_path text)
RETURNS boolean LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT coalesce(starts_with(p_path, 'results/lab_data/') OR starts_with(p_path, 'results/lab_derived/'), false)
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_is_lab_table_path(text) FROM PUBLIC;

-- A task holds the lab's own data (row values reach people, never a model) when it or any task it was made
-- from is a dataset intake or preparation, an analysis or figure of a table under `results/lab_data/` or
-- `results/lab_derived/`, a task whose digest lists such a table, or a task whose attempt was given a lab object or an
-- earlier run's output (input classes `object_data` and `prior_output`; an output volume names its run, not the task
-- that wrote it, so it counts as the lab's whatever it holds). A task is made from the source of its analysis or figure
-- request and from its project parent. The walk keeps each task once, so it ends on any chain or cycle, and it has no
-- depth limit: a step taken any number of steps after the lab's data still holds it. A lab database's query result is
-- not the lab's data in this sense: the research lead may read it within the reader login's grants.
-- Its result files are read by the project's people only; a service account that may read the project, and the
-- research lead, read its digest without the cells of its tables.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_holds_lab_data(p_tenant text, p_task uuid)
RETURNS boolean LANGUAGE sql STABLE SET search_path = pg_catalog, enrichedlab_agent AS $$
  WITH RECURSIVE line(task_id) AS (
    SELECT p_task
    UNION
    SELECT s.source_task_id
    FROM line
    JOIN (SELECT a.task_id, a.source_task_id FROM enrichedlab_agent.broker_analysis_requests a WHERE a.tenant_id = p_tenant
          UNION ALL
          SELECT f.task_id, f.source_task_id FROM enrichedlab_agent.broker_figure_requests f WHERE f.tenant_id = p_tenant
          UNION ALL
          SELECT t.task_id, t.parent_task_id FROM enrichedlab_agent.broker_project_tasks t
          WHERE t.tenant_id = p_tenant AND t.parent_task_id IS NOT NULL) s
      ON s.task_id = line.task_id
  )
  SELECT EXISTS (SELECT 1 FROM line JOIN enrichedlab_agent.broker_dataset_requests r
                   ON r.tenant_id = p_tenant AND r.task_id = line.task_id)
      OR EXISTS (SELECT 1 FROM line JOIN enrichedlab_agent.broker_analysis_requests a
                   ON a.tenant_id = p_tenant AND a.task_id = line.task_id
                  WHERE enrichedlab_agent.broker_is_lab_table_path(a.table_path))
      OR EXISTS (SELECT 1 FROM line JOIN enrichedlab_agent.broker_figure_requests f
                   ON f.tenant_id = p_tenant AND f.task_id = line.task_id
                  WHERE enrichedlab_agent.broker_is_lab_table_path(f.table_path))
      OR EXISTS (SELECT 1 FROM line JOIN enrichedlab_agent.broker_attempt_inputs i
                   ON i.tenant_id = p_tenant AND i.task_id = line.task_id
                  WHERE i.input_class IN ('object_data', 'prior_output'))
      OR EXISTS (SELECT 1 FROM line JOIN enrichedlab_agent.broker_artifact_digests d
                   ON d.tenant_id = p_tenant AND d.task_id = line.task_id
                  CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(d.digest->'tables') = 'array'
                                                          THEN d.digest->'tables' ELSE '[]'::jsonb END) AS t(item)
                  WHERE enrichedlab_agent.broker_is_lab_table_path(t.item->>'path'))
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_holds_lab_data(text, uuid) FROM PUBLIC;

-- A digest as the research lead and a service account read it: a table under `results/lab_data/` or
-- `results/lab_derived/` lists its columns and counts without cells whatever task wrote it, and for a task that holds
-- the lab's own data so does every other table except the headline statistics the tool computed
-- (`results/statistics.csv`), which the lead may discuss. Applied when a digest is read, so a stored digest that holds
-- cells, whenever it was written, never brings them to a reader that is not a person.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_digest_without_lab_cells(p_tenant text, p_task uuid, p_digest jsonb)
RETURNS jsonb LANGUAGE sql STABLE SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT CASE
    WHEN jsonb_typeof(p_digest->'tables') IS DISTINCT FROM 'array' THEN p_digest
    ELSE (SELECT CASE WHEN bool_or(x.withheld) THEN jsonb_set(p_digest, '{tables}', jsonb_agg(CASE
              WHEN x.withheld THEN (x.t - 'cells') || jsonb_build_object('cells_omitted',
                CASE WHEN starts_with(x.t->>'path', 'results/lab_data/') THEN 'lab_data' ELSE 'lab_derived' END)
              ELSE x.t END ORDER BY x.n)) ELSE p_digest END
          FROM (SELECT e.t, e.n, e.t ? 'cells' AND (enrichedlab_agent.broker_is_lab_table_path(e.t->>'path')
                  OR (e.t->>'path' IS DISTINCT FROM 'results/statistics.csv' AND holds.lab)) AS withheld
                FROM jsonb_array_elements(p_digest->'tables') WITH ORDINALITY AS e(t, n),
                     (SELECT enrichedlab_agent.broker_task_holds_lab_data(p_tenant, p_task) AS lab) AS holds) x)
  END
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_digest_without_lab_cells(text, uuid, jsonb) FROM PUBLIC;

-- The research lead's results (migration 051's reader): each digest without the lab's cells.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_research_run_results(p_tenant text, p_run uuid, p_worker text, p_generation bigint)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'enrichedlab_agent'
AS $function$
DECLARE
  r enrichedlab_agent.broker_research_runs%ROWTYPE;
  plan jsonb;
  results jsonb;
  findings jsonb;
  latest jsonb;
  reviews jsonb;
  ledger jsonb;
  attempts jsonb;
  revisions integer;
  revision jsonb;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs
  WHERE tenant_id = p_tenant AND run_id = p_run AND status = 'running' AND lease_owner = p_worker
    AND generation = p_generation AND lease_until > clock_timestamp();
  IF NOT FOUND THEN
    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'broker:lease_fence_lost';
  END IF;
  SELECT c.state->'plan' INTO plan
  FROM enrichedlab_agent.broker_projects p
  JOIN enrichedlab_agent.broker_project_checkpoints c
    ON c.tenant_id = p.tenant_id AND c.project_id = p.project_id AND c.revision = p.revision
  WHERE p.tenant_id = p_tenant AND p.project_id = r.project_id;
  SELECT coalesce(jsonb_agg(x.item ORDER BY x.task_id, x.created_at DESC), '[]'::jsonb) INTO results
  FROM (
    SELECT a.task_id, a.created_at, jsonb_build_object(
      'task_id', a.task_id,
      'ref', 'artifact://tasks/' || a.task_id::text || '/' || a.artifact_id::text,
      'label', a.label,
      'status', d.status,
      'digest_sha256', d.digest_sha256,
      'created_at', to_char(a.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
      'digest', enrichedlab_agent.broker_digest_without_lab_cells(p_tenant, a.task_id, d.digest)) AS item,
      row_number() OVER (PARTITION BY a.task_id ORDER BY a.created_at DESC, a.artifact_id) AS n
    FROM enrichedlab_agent.broker_project_tasks pt
    JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id = pt.tenant_id AND a.task_id = pt.task_id
    JOIN enrichedlab_agent.broker_artifact_digests d
      ON d.tenant_id = a.tenant_id AND d.artifact_id = a.artifact_id AND d.artifact_sha256 = a.sha256
    WHERE pt.tenant_id = p_tenant AND pt.project_id = r.project_id
      AND a.visibility = 'researcher' AND a.role = 'result'
      AND EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(plan, '[]'::jsonb)) s WHERE s->>'task_id' = a.task_id::text)
  ) x WHERE x.n <= 4;
  SELECT jsonb_build_object('run_id', t.run_id, 'turn', t.turn, 'recorded_at', t.created_at,
           'answer', t.record->'proposal'->'answer', 'findings', t.record->'proposal'->'findings')
    INTO findings
  FROM enrichedlab_agent.broker_research_turns t
  JOIN enrichedlab_agent.broker_research_runs rr ON rr.tenant_id = t.tenant_id AND rr.run_id = t.run_id
  WHERE t.tenant_id = p_tenant AND rr.project_id = r.project_id
    AND jsonb_typeof(t.record->'proposal'->'findings') = 'array'
    AND jsonb_array_length(t.record->'proposal'->'findings') > 0
  ORDER BY t.created_at DESC, t.turn DESC
  LIMIT 1;
  -- The newest decision that states an answer, even one with no findings: the
  -- earlier findings above are history and never stand for a newer answer.
  SELECT jsonb_build_object('run_id', t.run_id, 'turn', t.turn, 'recorded_at', t.created_at,
           'answer', t.record->'proposal'->'answer',
           'findings', jsonb_array_length(coalesce(t.record->'proposal'->'findings', '[]'::jsonb)))
    INTO latest
  FROM enrichedlab_agent.broker_research_turns t
  JOIN enrichedlab_agent.broker_research_runs rr ON rr.tenant_id = t.tenant_id AND rr.run_id = t.run_id
  WHERE t.tenant_id = p_tenant AND rr.project_id = r.project_id
    AND t.record->'proposal' ? 'answer'
  ORDER BY t.created_at DESC, t.turn DESC
  LIMIT 1;
  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id', v.task_id, 'review_id', v.review_id, 'decision', v.decision,
      'note', CASE WHEN v.decision <> 'accepted' THEN v.note END, 'recorded_at', v.created_at,
      'covers_current_outputs', v.outputs = enrichedlab_agent.broker_task_review_outputs(p_tenant, v.task_id))
      ORDER BY v.task_id), '[]'::jsonb) INTO reviews
  FROM (
    SELECT DISTINCT ON (rv.task_id) rv.task_id, rv.review_id, rv.decision, rv.note, rv.created_at, rv.outputs
    FROM enrichedlab_agent.broker_task_reviews rv
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id = rv.tenant_id AND pt.task_id = rv.task_id
    JOIN enrichedlab_agent.broker_principals p ON p.tenant_id = rv.tenant_id AND p.principal_id = rv.reviewer_id
    WHERE rv.tenant_id = p_tenant AND pt.project_id = r.project_id AND p.principal_class = 'person'
      AND EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(plan, '[]'::jsonb)) s WHERE s->>'task_id' = rv.task_id::text)
    ORDER BY rv.task_id, rv.created_at DESC, rv.review_id DESC
  ) v;
  -- The newest account of the question's parts this run's lead gave, to update, not re-derive.
  SELECT jsonb_build_object('turn', t.turn, 'answer', t.record->'proposal'->'answer',
           'parts', t.record->'proposal'->'parts')
    INTO ledger
  FROM enrichedlab_agent.broker_research_turns t
  WHERE t.tenant_id = p_tenant AND t.run_id = p_run
    AND jsonb_typeof(t.record->'proposal'->'parts') = 'array'
    AND jsonb_array_length(t.record->'proposal'->'parts') > 0
  ORDER BY t.turn DESC
  LIMIT 1;
  -- This run's refused lead replies, newest first and at most twelve: each one's turn and fixed reason code, never
  -- the reply.
  SELECT coalesce(jsonb_agg(jsonb_build_object('turn', e.turn, 'refused', e.reason) ORDER BY e.sequence DESC),
           '[]'::jsonb)
    INTO attempts
  FROM (SELECT ev.sequence, ev.turn, ev.reason FROM enrichedlab_agent.broker_research_run_events ev
         WHERE ev.tenant_id = p_tenant AND ev.run_id = p_run AND ev.code = 'proposal_invalid'
           AND ev.reason IS NOT NULL
         ORDER BY ev.sequence DESC LIMIT 12) e;
  -- The revisions this run's methods checks asked for, and the newest turn's review when it was one:
  -- its reason and typed findings, with the words of the part each names that code took from the proposal sent back,
  -- for the lead's next call.
  SELECT count(*) INTO revisions FROM enrichedlab_agent.broker_research_turns t
    WHERE t.tenant_id = p_tenant AND t.run_id = p_run AND t.record->>'outcome' = 'revise';
  SELECT jsonb_build_object('turn', t.turn, 'reason', t.record->'method_review'->'reason',
           'findings', coalesce(t.record->'method_review'->'findings', '[]'::jsonb),
           'targets', coalesce(t.record->'revision'->'targets', '[]'::jsonb),
           'query_facts', coalesce(t.record->'revision'->'query_facts', '[]'::jsonb))
    INTO revision
  FROM enrichedlab_agent.broker_research_turns t
  WHERE t.tenant_id = p_tenant AND t.run_id = p_run AND t.turn = r.turn AND t.record->>'outcome' = 'revise';
  RETURN jsonb_build_object('results', results, 'earlier_findings', findings, 'latest_answer', latest, 'reviews', reviews,
    'ledger', ledger, 'attempts', attempts, 'revisions', revisions, 'revision_budget', 2, 'revision_request', revision);
END;
$function$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_results(text,uuid,text,bigint) FROM PUBLIC;

-- A reader of an artifact may read its digest for the artifact's current bytes, or learn that none exists (migration
-- 021's reader); a reader that is not a person reads it without the lab's cells.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_artifact_digest(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_artifact uuid
)
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT jsonb_build_object(
    'artifact_id', a.artifact_id,
    'artifact_sha256', a.sha256,
    'status', coalesce(d.status, 'not_recorded'),
    'digest_sha256', d.digest_sha256,
    'recorded_at', d.recorded_at,
    'digest', CASE WHEN enrichedlab_agent.broker_is_person(p_tenant, p_principal) THEN d.digest
                   ELSE enrichedlab_agent.broker_digest_without_lab_cells(p_tenant, a.task_id, d.digest) END)
  FROM enrichedlab_agent.broker_artifacts a
  LEFT JOIN enrichedlab_agent.broker_artifact_digests d
    ON d.tenant_id = a.tenant_id AND d.artifact_id = a.artifact_id AND d.artifact_sha256 = a.sha256
  WHERE a.tenant_id = p_tenant AND a.task_id = p_task AND a.artifact_id = p_artifact
    AND a.visibility = 'researcher'
    AND enrichedlab_agent.broker_task_can(p_tenant, p_principal, a.task_id, 'read')
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_get_artifact_digest(text,uuid,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_get_artifact(
  p_tenant text,
  p_principal uuid,
  p_task uuid,
  p_artifact uuid
)
RETURNS TABLE(
  artifact_id uuid, role text, label text, relative_path text, media_type text,
  size_bytes bigint, sha256 text, created_at timestamptz
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT artifacts.artifact_id, artifacts.role, artifacts.label,
         artifacts.relative_path, artifacts.media_type, artifacts.size_bytes,
         artifacts.sha256, artifacts.created_at
  FROM enrichedlab_agent.broker_artifacts AS artifacts
  JOIN enrichedlab_agent.broker_tasks AS tasks
    ON tasks.tenant_id = artifacts.tenant_id AND tasks.task_id = artifacts.task_id
  WHERE artifacts.tenant_id = p_tenant
    AND artifacts.task_id = p_task
    AND artifacts.artifact_id = p_artifact
    AND artifacts.visibility = 'researcher'
    AND enrichedlab_agent.broker_task_can(p_tenant,p_principal,tasks.task_id,'read')
    AND (NOT enrichedlab_agent.broker_task_holds_lab_data(p_tenant, tasks.task_id)
         OR enrichedlab_agent.broker_is_person(p_tenant, p_principal))
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (59) ON CONFLICT DO NOTHING;
COMMIT;
