-- One task lineage read with its recorded upstream inputs. The walk follows only
-- records that name a producing task: analysis and render requests, a dataset preparation's intake,
-- or an input custodian record that names a registered artifact. It never guesses from a path, name
-- or timestamp. Every task's ordinary reader rule is checked before its identifier enters the answer.
-- Version 1 and version 2 are unchanged.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_task_lineage_v3(
  p_tenant text,
  p_actor uuid,
  p_task uuid
)
RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
DECLARE
  base jsonb;
  lineage jsonb;
  nodes jsonb := '[]'::jsonb;
  edges jsonb := '[]'::jsonb;
  sources jsonb := '[]'::jsonb;
  source jsonb;
  queue uuid[] := ARRAY[p_task];
  depths integer[] := ARRAY[0];
  discovered uuid[] := ARRAY[p_task];
  visited uuid[] := ARRAY[]::uuid[];
  position integer := 1;
  node_task uuid;
  node_depth integer;
  upstream_input record;
  readable boolean;
  withheld integer := 0;
  truncated boolean := false;
  truncation_reason text;
BEGIN
  -- Version 2 authorizes the requested task and supplies its body. All reads made by this
  -- stable function use the statement's one transaction snapshot.
  base := enrichedlab_agent.broker_task_lineage_v2(p_tenant, p_actor, p_task);

  WHILE position <= cardinality(queue) LOOP
    node_task := queue[position];
    node_depth := depths[position];
    position := position + 1;

    -- `discovered` normally makes this unnecessary, but it also closes a malformed cycle if
    -- rows written before the current constraints contain one.
    IF node_task = ANY(visited) THEN
      CONTINUE;
    END IF;
    visited := array_append(visited, node_task);
    lineage := CASE WHEN node_task = p_task THEN base
                    ELSE enrichedlab_agent.broker_task_lineage_v2(p_tenant, p_actor, node_task) END;

    nodes := nodes || jsonb_build_array(jsonb_build_object(
      'task_id', node_task,
      'title', coalesce(nullif(lineage->'analysis'->>'title', ''),
                        nullif(lineage->'render'->>'title', ''),
                        nullif(lineage->'dataset'->>'title', ''),
                        nullif(lineage->'plan_step'->>'title', ''),
                        nullif(lineage->'method'->>'profile_name', ''),
                        lineage->'task'->>'objective'),
      'method_class', lineage->'method'->'tool_class',
      'method', jsonb_strip_nulls(lineage->'method' || jsonb_build_object(
        'tool', lineage->'inputs'->'platform_tool',
        'query', lineage->'query',
        'analysis', lineage->'analysis',
        'render', lineage->'render',
        'dataset', lineage->'dataset')),
      'attempts', (SELECT coalesce(jsonb_agg(jsonb_build_object(
          'attempt', a->'attempt',
          'execution', a->'execution',
          'outputs', coalesce(a->'outputs', '[]'::jsonb),
          'gaps', coalesce(a->'gaps', '[]'::jsonb)) ORDER BY (a->>'attempt')::integer), '[]'::jsonb)
        FROM jsonb_array_elements(coalesce(lineage->'attempts', '[]'::jsonb)) a),
      'outputs', coalesce(lineage->'outputs', '[]'::jsonb),
      'review', lineage->'review',
      'gaps', coalesce(lineage->'gaps', '[]'::jsonb)));

    -- A guarded query ends at the exact registered source declaration. A live source's
    -- observed version remains in this node's version-1 query record.
    SELECT jsonb_build_object(
        'kind', 'guarded_query',
        'node', node_task,
        'source', s.source_key,
        'profile', s.profile_key,
        'schema', s.schema_name,
        'snapshot', s.snapshot)
      INTO source
      FROM enrichedlab_agent.broker_task_queries q
      JOIN enrichedlab_agent.broker_sql_sources s
        ON s.tenant_id = q.tenant_id AND s.source_key = q.source_key
      WHERE q.tenant_id = p_tenant AND q.task_id = node_task;
    IF source IS NOT NULL AND NOT sources @> jsonb_build_array(source) THEN
      sources := sources || jsonb_build_array(source);
    END IF;

    -- A dataset intake ends at the file and guard decision the broker registered. Preparations
    -- reach this row by following their dataset request to the intake task below.
    SELECT jsonb_build_object(
        'kind', 'lab_dataset',
        'node', node_task,
        'dataset', d.dataset_id,
        'file_name', d.file_name,
        'sha256', d.sha256,
        'size_bytes', d.size_bytes,
        'guard', jsonb_build_object('decision', d.guard_outcome, 'policy', d.guard_policy))
      INTO source
      FROM enrichedlab_agent.broker_dataset_requests r
      JOIN enrichedlab_agent.broker_datasets d
        ON d.tenant_id = r.tenant_id AND d.dataset_id = r.dataset_id
      WHERE r.tenant_id = p_tenant AND r.task_id = node_task AND r.operation = 'intake';
    IF source IS NOT NULL AND NOT sources @> jsonb_build_array(source) THEN
      sources := sources || jsonb_build_array(source);
    END IF;

    -- One row here is one recorded input. Analysis and render requests name the table directly.
    -- A dataset preparation names its intake through the dataset record and that intake's receipt
    -- binds the registered result package. A custodian input becomes an edge only when its source
    -- explicitly names an artifact UUID; otherwise it is a terminal source below.
    FOR upstream_input IN
      SELECT x.producing_task, x.output_path, x.output_sha256, x.recorded_by,
             x.input_ref, x.input_class, x.sealed_sha256, x.source_snapshot
      FROM (
        SELECT r.source_task_id AS producing_task, r.table_path AS output_path,
               r.table_sha256 AS output_sha256, 'analysis_request'::text AS recorded_by,
               NULL::text AS input_ref, NULL::text AS input_class,
               NULL::text AS sealed_sha256, NULL::jsonb AS source_snapshot
        FROM enrichedlab_agent.broker_analysis_requests r
        WHERE r.tenant_id = p_tenant AND r.task_id = node_task
        UNION ALL
        SELECT r.source_task_id, r.table_path, r.table_sha256, 'render_request',
               NULL::text, NULL::text, NULL::text, NULL::jsonb
        FROM enrichedlab_agent.broker_figure_requests r
        WHERE r.tenant_id = p_tenant AND r.task_id = node_task
        UNION ALL
        SELECT d.intake_task_id, a.relative_path, a.sha256, 'dataset_request',
               NULL::text, NULL::text, NULL::text, NULL::jsonb
        FROM enrichedlab_agent.broker_dataset_requests r
        JOIN enrichedlab_agent.broker_datasets d
          ON d.tenant_id = r.tenant_id AND d.dataset_id = r.dataset_id
        LEFT JOIN LATERAL (
          SELECT x.package_sha256
          FROM enrichedlab_agent.broker_dataset_receipts x
          WHERE x.tenant_id = d.tenant_id AND x.task_id = d.intake_task_id AND x.status = 'profiled'
          ORDER BY x.attempt DESC, x.lease_generation DESC
          LIMIT 1
        ) receipt ON true
        LEFT JOIN LATERAL (
          SELECT output.relative_path, output.sha256
          FROM enrichedlab_agent.broker_artifacts output
          WHERE output.tenant_id = d.tenant_id AND output.task_id = d.intake_task_id
            AND output.sha256 = receipt.package_sha256 AND output.role = 'result'
            AND output.visibility = 'researcher'
          ORDER BY output.created_at DESC, output.artifact_id
          LIMIT 1
        ) a ON true
        WHERE r.tenant_id = p_tenant AND r.task_id = node_task AND r.operation = 'preparation'
        UNION ALL
        SELECT a.task_id, a.relative_path, a.sha256, 'input_custodian',
               i.input_ref, i.input_class, i.sealed_sha256, i.source
        FROM enrichedlab_agent.broker_attempt_inputs i
        LEFT JOIN LATERAL (
          SELECT CASE
            WHEN i.source->>'artifact_id' ~
              '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
              THEN (i.source->>'artifact_id')::uuid
            WHEN i.source->>'source_artifact_id' ~
              '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
              THEN (i.source->>'source_artifact_id')::uuid
          END AS artifact_id
        ) named ON true
        LEFT JOIN enrichedlab_agent.broker_artifacts a
          ON a.tenant_id = i.tenant_id AND a.artifact_id = named.artifact_id
         AND a.visibility = 'researcher'
        WHERE i.tenant_id = p_tenant AND i.task_id = node_task
      ) x
      ORDER BY x.recorded_by, x.output_path, x.input_ref, x.sealed_sha256
    LOOP
      IF upstream_input.producing_task IS NULL THEN
        -- A paper carries only its library identity. Every other custodian source carries the
        -- sealed reference and source snapshot already exposed by version 2, never file content.
        IF upstream_input.recorded_by = 'input_custodian' THEN
          IF upstream_input.input_class = 'paper' THEN
            SELECT jsonb_strip_nulls(jsonb_build_object(
                'kind', 'paper',
                'node', node_task,
                'reference', upstream_input.input_ref,
                'paper', coalesce(p.paper_id::text,
                  upstream_input.source_snapshot->>'paper_id',
                  upstream_input.source_snapshot->>'library_paper_id'),
                'lab', coalesce(p.group_key,
                  upstream_input.source_snapshot->>'lab',
                  upstream_input.source_snapshot->>'group_key'),
                'document', coalesce(p.document_id::text,
                  upstream_input.source_snapshot->>'document_id'),
                'source_sha256', coalesce(p.source_sha256,
                  upstream_input.source_snapshot->>'source_sha256'),
                'sealed_sha256', upstream_input.sealed_sha256))
              INTO source
              FROM (SELECT 1) one
              LEFT JOIN enrichedlab_agent.broker_library_papers p
                ON p.tenant_id = p_tenant
               AND p.paper_id = CASE
                 WHEN coalesce(upstream_input.source_snapshot->>'paper_id',
                               upstream_input.source_snapshot->>'library_paper_id') ~
                   '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                 THEN coalesce(upstream_input.source_snapshot->>'paper_id',
                               upstream_input.source_snapshot->>'library_paper_id')::uuid
               END;
          ELSE
            source := jsonb_build_object(
              'kind', 'sealed_input',
              'node', node_task,
              'reference', upstream_input.input_ref,
              'class', upstream_input.input_class,
              'sealed_sha256', upstream_input.sealed_sha256,
              'source_snapshot', upstream_input.source_snapshot);
          END IF;
          IF source IS NOT NULL AND NOT sources @> jsonb_build_array(source) THEN
            sources := sources || jsonb_build_array(source);
          END IF;
        END IF;
        CONTINUE;
      END IF;

      readable := enrichedlab_agent.broker_task_can(
        p_tenant, p_actor, upstream_input.producing_task, 'read');
      IF NOT readable THEN
        withheld := withheld + 1;
        CONTINUE;
      END IF;

      IF NOT (upstream_input.producing_task = ANY(discovered)) THEN
        IF node_depth >= 16 THEN
          truncated := true;
          truncation_reason := coalesce(truncation_reason, 'depth');
          CONTINUE;
        ELSIF cardinality(discovered) >= 200 THEN
          truncated := true;
          truncation_reason := coalesce(truncation_reason, 'nodes');
          CONTINUE;
        END IF;
        discovered := array_append(discovered, upstream_input.producing_task);
        queue := array_append(queue, upstream_input.producing_task);
        depths := array_append(depths, node_depth + 1);
      END IF;

      edges := edges || jsonb_build_array(jsonb_build_object(
        'from', upstream_input.producing_task,
        'to', node_task,
        'output', upstream_input.output_path,
        'digest', upstream_input.output_sha256,
        'recorded_by', upstream_input.recorded_by));
    END LOOP;
  END LOOP;

  RETURN (base - 'schema') || jsonb_build_object(
    'schema', 'enrichedlab.task-lineage.v3',
    'upstream', jsonb_build_object(
      'nodes', nodes,
      'edges', edges,
      'sources', sources,
      'withheld', withheld,
      'limits', jsonb_build_object(
        'max_depth', 16,
        'max_nodes', 200,
        'truncated', truncated,
        'reason', truncation_reason)));
END;
$$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_task_lineage_v3(text, uuid, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent
AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = 2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version = p_version);
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (65) ON CONFLICT DO NOTHING;
COMMIT;
