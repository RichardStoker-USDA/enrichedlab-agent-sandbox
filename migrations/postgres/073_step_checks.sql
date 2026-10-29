-- Step checks. Each research step now states what its result must look like, and code checks the
-- result's digest against that statement before the research lead reads it. For that the worker's digest summarizes
-- each listed column over every row of the table (`summary`): whether every value is a number, the smallest and largest
-- number as written (`min`, `max`; never for a table under a lab path, which the worker leaves out itself), and how many
-- distinct values. The smallest and largest are values of the table, so wherever a reader that is not a person is shown
-- a table without its cells (migration 059: a lab path, or any table but the headline statistics of a task that holds
-- the lab's own data), the summary loses them too and keeps its counts, as the missing counts are kept. A table whose
-- cells were already left out for size is treated the same way. Applied when a digest is read, so a digest stored by
-- any worker never brings a lab's values to the research lead or a service account; the step check then names such a
-- range as one the digest does not give. A digest without summaries reads exactly as before.
-- A project package carries every result's files and stored digest whole, to anyone who may read the project (migrations
-- 014 and 068), so the package was the one path left by which a lab's values reached a reader that is not a person; such
-- a reader now gets no package of a project any of whose tasks holds the lab's own data. No service account had a lab's
-- project open when this was written (two, both in qualification groups, no project grant).
-- Rollback: an older broker reads the same functions and ignores `summary`; they may stay.
BEGIN;

-- One table as a reader that is not a person sees it: no cells (named lab_data or lab_derived, as migration 059 names
-- them), and each column's summary without its smallest and largest value.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_table_without_lab_values(p_table jsonb)
RETURNS jsonb LANGUAGE sql IMMUTABLE SET search_path = pg_catalog AS $$
  SELECT CASE WHEN p_table ? 'cells' THEN (p_table - 'cells') || jsonb_build_object('cells_omitted',
           CASE WHEN starts_with(p_table->>'path', 'results/lab_data/') THEN 'lab_data' ELSE 'lab_derived' END)
         ELSE p_table END
      || CASE WHEN jsonb_typeof(p_table->'summary') = 'object' THEN jsonb_build_object('summary',
           coalesce((SELECT jsonb_object_agg(s.k, CASE WHEN jsonb_typeof(s.v) = 'object' THEN s.v - 'min' - 'max'
                                                       ELSE s.v END)
                     FROM jsonb_each(p_table->'summary') AS s(k, v)), '{}'::jsonb))
         ELSE '{}'::jsonb END
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_table_without_lab_values(jsonb) FROM PUBLIC;

-- Migration 059's reader rule, now for a table's summary values as for its cells.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_digest_without_lab_cells(p_tenant text, p_task uuid, p_digest jsonb)
RETURNS jsonb LANGUAGE sql STABLE SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT CASE
    WHEN jsonb_typeof(p_digest->'tables') IS DISTINCT FROM 'array' THEN p_digest
    ELSE (SELECT CASE WHEN bool_or(x.withheld) THEN jsonb_set(p_digest, '{tables}', jsonb_agg(CASE
              WHEN x.withheld THEN enrichedlab_agent.broker_table_without_lab_values(x.t)
              ELSE x.t END ORDER BY x.n)) ELSE p_digest END
          FROM (SELECT e.t, e.n, (e.t ? 'cells' OR e.t ? 'summary') AND (enrichedlab_agent.broker_is_lab_table_path(e.t->>'path')
                  OR (e.t->>'path' IS DISTINCT FROM 'results/statistics.csv' AND holds.lab)) AS withheld
                FROM jsonb_array_elements(p_digest->'tables') WITH ORDINALITY AS e(t, n),
                     (SELECT enrichedlab_agent.broker_task_holds_lab_data(p_tenant, p_task) AS lab) AS holds) x)
  END
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_digest_without_lab_cells(text, uuid, jsonb) FROM PUBLIC;

-- Who may have a project's package (migration 068), now never a reader that is not a person for a lab's data.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_package_scope(
  p_tenant text, p_actor uuid, p_project uuid)
RETURNS text LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  group_name text;
  may_read_reasoning boolean;
BEGIN
  IF NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,p_project,'read') THEN
    RETURN NULL;
  END IF;
  -- Migration 073: a reader that is not a person gets no package of a project any of whose tasks holds the lab's own
  -- data, since a package carries every result's files and stored digest whole (migration 059's reader rule).
  IF NOT enrichedlab_agent.broker_is_person(p_tenant, p_actor) AND EXISTS (
      SELECT 1 FROM enrichedlab_agent.broker_project_tasks pt
      WHERE pt.tenant_id = p_tenant AND pt.project_id = p_project
        AND enrichedlab_agent.broker_task_holds_lab_data(p_tenant, pt.task_id)) THEN
    RETURN NULL;
  END IF;
  IF EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_inspections i
    WHERE i.tenant_id=p_tenant AND i.project_id=p_project AND i.principal_id=p_actor
      AND i.expires_at>statement_timestamp()) THEN
    RETURN 'inspection';
  END IF;
  SELECT p.group_key INTO group_name FROM enrichedlab_agent.broker_projects p
    WHERE p.tenant_id=p_tenant AND p.project_id=p_project;
  SELECT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=m.tenant_id
      AND a.principal_id=m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id=m.tenant_id
      AND g.group_key=m.group_key AND g.enabled
    JOIN enrichedlab_agent.broker_projects p ON p.tenant_id=m.tenant_id
      AND p.group_key=m.group_key AND p.project_id=p_project
    WHERE m.tenant_id=p_tenant AND m.group_key=group_name AND m.principal_id=p_actor
      AND (p.owner_id=p_actor OR p.visibility='lab' OR EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_project_grants grant_record
        WHERE grant_record.tenant_id=p.tenant_id AND grant_record.project_id=p.project_id
          AND grant_record.principal_id=p_actor AND cardinality(grant_record.capabilities)>0)))
    INTO may_read_reasoning;
  RETURN CASE WHEN may_read_reasoning THEN 'ordinary_with_reasoning'
    ELSE 'ordinary_without_reasoning' END;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_package_scope(text,uuid,uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (73) ON CONFLICT DO NOTHING;
COMMIT;
