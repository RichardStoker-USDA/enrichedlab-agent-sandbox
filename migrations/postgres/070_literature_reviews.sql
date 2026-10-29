-- Literature review outcomes: immutable run artifacts, scoped to project lab members.
BEGIN;
CREATE TABLE IF NOT EXISTS enrichedlab_agent.broker_literature_reviews (
  tenant_id text NOT NULL,
  run_id uuid NOT NULL,
  inputs_sha256 text NOT NULL CHECK (inputs_sha256 ~ '^sha256:[0-9a-f]{64}$'),
  record jsonb NOT NULL CHECK (jsonb_typeof(record)='object' AND octet_length(record::text)<=262144),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (tenant_id,run_id,inputs_sha256),
  FOREIGN KEY (tenant_id,run_id) REFERENCES enrichedlab_agent.broker_research_runs (tenant_id,run_id)
);
ALTER TABLE enrichedlab_agent.broker_literature_reviews ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON enrichedlab_agent.broker_literature_reviews FROM PUBLIC;
DROP TRIGGER IF EXISTS immutable_record ON enrichedlab_agent.broker_literature_reviews;
CREATE TRIGGER immutable_record BEFORE UPDATE OR DELETE ON enrichedlab_agent.broker_literature_reviews
  FOR EACH ROW EXECUTE FUNCTION enrichedlab_agent.reject_broker_event_mutation();

-- A null record reads the cache; a checked or refused record fills it. Both operations require the current lease and membership.
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_literature_review_for_run(
  p_tenant text,p_run uuid,p_worker text,p_generation bigint,p_inputs text,p_record jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE held jsonb; answer jsonb;
BEGIN
  PERFORM 1 FROM enrichedlab_agent.broker_research_runs
    WHERE tenant_id=p_tenant AND run_id=p_run AND status='running' AND lease_owner=p_worker
      AND generation=p_generation AND lease_until>clock_timestamp() FOR UPDATE;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  held := enrichedlab_agent.broker_research_run_library(p_tenant,p_run,p_worker,p_generation,'{}'::uuid[]);
  IF held->>'member' IS DISTINCT FROM 'true' THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:context_lab_members_only';
  END IF;
  IF p_inputs IS NULL OR p_inputs !~ '^sha256:[0-9a-f]{64}$' THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_invalid';
  END IF;
  IF p_record IS NOT NULL THEN
    IF p_record->>'inputs_sha256' IS DISTINCT FROM p_inputs
      OR jsonb_typeof(p_record->'specialist') IS DISTINCT FROM 'array' THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_invalid';
    END IF;
    IF jsonb_typeof(p_record->'reading'->'rounds') IS DISTINCT FROM 'array' THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_invalid';
    END IF;
    IF EXISTS (SELECT 1 FROM jsonb_array_elements(p_record->'reading'->'rounds') rr
      WHERE jsonb_typeof(rr) IS DISTINCT FROM 'object'
        OR jsonb_typeof(rr->'shown') IS DISTINCT FROM 'array') THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_invalid';
    END IF;
    IF p_record->>'status' = 'refused' THEN
      IF p_record ? 'review' OR jsonb_typeof(p_record->'faults') IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_invalid';
      END IF;
      IF jsonb_array_length(p_record->'faults')=0 OR EXISTS (
        SELECT 1 FROM jsonb_array_elements(p_record->'faults') f WHERE jsonb_typeof(f)<>'string') THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_invalid';
      END IF;
    ELSIF jsonb_typeof(p_record->'review') IS DISTINCT FROM 'object'
      OR coalesce(p_record->>'status','read') <> 'read' THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:context_invalid';
    END IF;
    INSERT INTO enrichedlab_agent.broker_literature_reviews(tenant_id,run_id,inputs_sha256,record)
      VALUES(p_tenant,p_run,p_inputs,p_record) ON CONFLICT DO NOTHING;
  END IF;
  SELECT record INTO answer FROM enrichedlab_agent.broker_literature_reviews
    WHERE tenant_id=p_tenant AND run_id=p_run AND inputs_sha256=p_inputs;
  RETURN answer;
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_literature_review(
  p_tenant text,p_actor uuid,p_run uuid,p_inputs text)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE project uuid; grp text; answer jsonb;
BEGIN
  SELECT project_id INTO project FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run;
  IF project IS NULL OR NOT enrichedlab_agent.broker_project_can(p_tenant,p_actor,project,'read') THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  SELECT group_key INTO grp FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=project;
  IF NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_group_memberships m
    JOIN enrichedlab_agent.broker_principals a ON a.tenant_id=m.tenant_id AND a.principal_id=m.principal_id AND a.enabled
    JOIN enrichedlab_agent.broker_research_groups g ON g.tenant_id=m.tenant_id AND g.group_key=m.group_key AND g.enabled
    WHERE m.tenant_id=p_tenant AND m.group_key=grp AND m.principal_id=p_actor) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:context_lab_members_only';
  END IF;
  SELECT record INTO answer FROM enrichedlab_agent.broker_literature_reviews
    WHERE tenant_id=p_tenant AND run_id=p_run AND inputs_sha256=p_inputs
      AND jsonb_typeof(record->'review')='object' AND record->>'status' IS DISTINCT FROM 'refused';
  IF answer IS NULL THEN RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:context_not_found'; END IF;
  RETURN answer;
END;
$$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_literature_review_for_run(text,uuid,text,bigint,text,jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_literature_review(text,uuid,uuid,text) FROM PUBLIC;

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
                        t.record->'methods_specialist'->>'reasoning_sha256',
                        t.record->'methods_specialist_refused'->'receipt'->>'context_sha256',
                        t.record->'methods_specialist_refused'->'receipt'->>'system_sha256',
                        t.record->'methods_specialist_refused'->'receipt'->>'content_sha256',
                        t.record->'methods_specialist_refused'->'receipt'->>'reasoning_sha256')
        OR EXISTS (
          SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(t.record->'literature_claims'->'specialist') = 'array'
                                                  THEN t.record->'literature_claims'->'specialist' ELSE '[]'::jsonb END) s
          WHERE p_sha256 IN (s->>'context_sha256', s->>'system_sha256', s->>'content_sha256', s->>'reasoning_sha256'))
        OR EXISTS (
          SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(t.record->'reading'->'rounds') = 'array'
                                                  THEN t.record->'reading'->'rounds' ELSE '[]'::jsonb END) rr
          WHERE p_sha256 IN (rr->'lead'->>'context_sha256', rr->'lead'->>'system_sha256',
                             rr->'lead'->>'content_sha256', rr->'lead'->>'reasoning_sha256'))))
     AND NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_literature_reviews v,
      LATERAL jsonb_array_elements(v.record->'specialist') s
    WHERE v.tenant_id=p_tenant AND v.run_id=p_run
      AND p_sha256 IN (s->>'context_sha256',s->>'system_sha256',s->>'content_sha256',s->>'reasoning_sha256'))
     AND NOT EXISTS (
    SELECT 1 FROM enrichedlab_agent.broker_research_turns t,
      LATERAL jsonb_array_elements(coalesce(t.record->'literature_review'->'specialist','[]'::jsonb)) s
    WHERE t.tenant_id=p_tenant AND t.run_id=p_run
      AND p_sha256 IN (s->>'context_sha256',s->>'system_sha256',s->>'content_sha256',s->>'reasoning_sha256'))
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

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_project_package_components(
  p_tenant text, p_actor uuid, p_project uuid)
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
DECLARE
  group_name text;
  disclosure_scope text;
  may_read_reasoning boolean;
  reasoning jsonb := '[]'::jsonb;
  digests jsonb;
  origins jsonb;
  papers jsonb;
BEGIN
  disclosure_scope := enrichedlab_agent.broker_project_package_scope(
    p_tenant,p_actor,p_project);
  IF disclosure_scope IS NULL THEN
    RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='broker:project_not_found';
  END IF;
  SELECT group_key INTO group_name FROM enrichedlab_agent.broker_projects
    WHERE tenant_id=p_tenant AND project_id=p_project;

  may_read_reasoning := disclosure_scope='ordinary_with_reasoning';

  IF may_read_reasoning THEN
    WITH addresses AS (
      SELECT t.run_id,t.turn,'lead'::text AS role,t.record->'lead'->>'reasoning_sha256' AS sha256
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT t.run_id,t.turn,'methods_specialist',t.record->'methods_specialist'->>'reasoning_sha256'
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT t.run_id,t.turn,'methods_specialist_refused',
        t.record->'methods_specialist_refused'->'receipt'->>'reasoning_sha256'
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT t.run_id,t.turn,'literature_specialist',s->>'reasoning_sha256'
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      CROSS JOIN LATERAL jsonb_array_elements(CASE
        WHEN jsonb_typeof(t.record->'literature_claims'->'specialist')='array'
        THEN t.record->'literature_claims'->'specialist' ELSE '[]'::jsonb END) s
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT t.run_id,t.turn,'literature_review',s->>'reasoning_sha256'
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      CROSS JOIN LATERAL jsonb_array_elements(coalesce(t.record->'literature_review'->'specialist','[]'::jsonb)) s
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT t.run_id,t.turn,'lead_reading',rr->'lead'->>'reasoning_sha256'
      FROM enrichedlab_agent.broker_research_turns t JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
      CROSS JOIN LATERAL jsonb_array_elements(CASE
        WHEN jsonb_typeof(t.record->'reading'->'rounds')='array'
        THEN t.record->'reading'->'rounds' ELSE '[]'::jsonb END) rr
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      UNION
      SELECT e.run_id,coalesce(e.turn,0),'lead_refused',e.lead_reasoning_sha256
      FROM enrichedlab_agent.broker_research_run_events e JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=e.tenant_id AND r.run_id=e.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project
    )
    SELECT coalesce(jsonb_agg(jsonb_build_object('run_id',a.run_id,'turn',a.turn,
      'role',a.role,'sha256',a.sha256,'text',convert_from(c.content,'UTF8'))
      ORDER BY a.run_id,a.turn,a.role,a.sha256),'[]'::jsonb) INTO reasoning
    FROM (SELECT DISTINCT run_id,turn,role,sha256 FROM addresses WHERE sha256 IS NOT NULL) a
    JOIN enrichedlab_agent.broker_research_contexts c
      ON c.tenant_id=p_tenant AND c.sha256=a.sha256;
  END IF;

  SELECT coalesce(jsonb_agg(jsonb_build_object('task_id',a.task_id,'artifact_id',a.artifact_id,
    'run_id',o.run_id,'turn',o.turn,'status',d.status,'digest_sha256',d.digest_sha256,
    'digest',d.digest,'recorded_at',d.recorded_at) ORDER BY a.task_id,a.created_at,a.artifact_id),'[]'::jsonb)
    INTO digests
  FROM enrichedlab_agent.broker_project_tasks pt
  JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id=pt.tenant_id AND a.task_id=pt.task_id
  JOIN enrichedlab_agent.broker_artifact_digests d ON d.tenant_id=a.tenant_id
    AND d.artifact_id=a.artifact_id AND d.artifact_sha256=a.sha256
  LEFT JOIN enrichedlab_agent.broker_task_origins o ON o.tenant_id=a.tenant_id AND o.task_id=a.task_id
  WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project AND a.visibility='researcher';

  SELECT coalesce(jsonb_agg(to_jsonb(o)-'tenant_id' ORDER BY o.recorded_at,o.task_id),'[]'::jsonb)
    INTO origins FROM enrichedlab_agent.broker_task_origins o
    JOIN enrichedlab_agent.broker_project_tasks pt ON pt.tenant_id=o.tenant_id AND pt.task_id=o.task_id
    WHERE pt.tenant_id=p_tenant AND pt.project_id=p_project;

  WITH touched AS (
    SELECT DISTINCT passage->>'paper_id' AS paper_id
    FROM enrichedlab_agent.broker_research_turns t
    JOIN enrichedlab_agent.broker_research_runs r ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
    CROSS JOIN LATERAL jsonb_array_elements(
      (CASE WHEN jsonb_typeof(t.record->'context_inputs'->'literature'->'passages')='array'
        THEN t.record->'context_inputs'->'literature'->'passages' ELSE '[]'::jsonb END)
      || (CASE WHEN jsonb_typeof(t.record->'context_inputs'->'literature'->'withheld')='array'
        THEN t.record->'context_inputs'->'literature'->'withheld' ELSE '[]'::jsonb END)) passage
    WHERE r.tenant_id=p_tenant AND r.project_id=p_project AND passage ? 'paper_id'
    UNION
    SELECT DISTINCT substring(shown->>'ref' FROM '^library://([0-9a-f-]{36})(/|$)')
    FROM enrichedlab_agent.broker_literature_reviews v
    JOIN enrichedlab_agent.broker_research_runs r ON r.tenant_id=v.tenant_id AND r.run_id=v.run_id
    CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(v.record->'reading'->'rounds')='array'
      THEN v.record->'reading'->'rounds' ELSE '[]'::jsonb END) rr
    CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(rr->'shown')='array'
      THEN rr->'shown' ELSE '[]'::jsonb END) shown
    WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      AND shown->>'ref' ~ '^library://[0-9a-f-]{36}(/|$)'
    UNION
    -- Papers the lead read in a turn: each shown part's ref names its paper.
    SELECT DISTINCT substring(shown->>'ref' FROM '^library://([0-9a-f-]{36})(/|$)')
    FROM enrichedlab_agent.broker_research_turns t
    JOIN enrichedlab_agent.broker_research_runs r ON r.tenant_id=t.tenant_id AND r.run_id=t.run_id
    CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(t.record->'reading'->'rounds')='array'
      THEN t.record->'reading'->'rounds' ELSE '[]'::jsonb END) rr
    CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(rr->'shown')='array'
      THEN rr->'shown' ELSE '[]'::jsonb END) shown
    WHERE r.tenant_id=p_tenant AND r.project_id=p_project
      AND shown->>'ref' ~ '^library://[0-9a-f-]{36}(/|$)'
  )
  -- An inspection is one disclosure chain regardless of whether its administrator
  -- also belongs to the lab, so it always stores the common ID-only paper view.
  SELECT coalesce(jsonb_agg(CASE WHEN p.paper_id IS NULL
      OR disclosure_scope<>'ordinary_with_reasoning'
    THEN jsonb_build_object('paper_id',t.paper_id)
    ELSE jsonb_build_object('paper_id',t.paper_id,'source_sha256',p.source_sha256,'byte_size',p.byte_size,
      'title',p.title,'authors',p.authors,'year',p.year,'doi',p.doi,'page_count',p.page_count,
      'parse',p.parse,'created_at',p.created_at) END ORDER BY t.paper_id),'[]'::jsonb) INTO papers
  FROM touched t LEFT JOIN enrichedlab_agent.broker_library_papers p
    ON p.tenant_id=p_tenant AND p.group_key=group_name AND p.paper_id::text=t.paper_id;

  RETURN jsonb_build_object('disclosure_scope',disclosure_scope,
    'reasoning_access',may_read_reasoning,'reasoning',reasoning,
    'digests',digests,'task_origins',origins,'papers',papers,
    'literature_reviews',CASE WHEN may_read_reasoning THEN coalesce((
      SELECT jsonb_agg(jsonb_build_object('run_id',v.run_id,'inputs_sha256',v.inputs_sha256,'status',coalesce(v.record->>'status','read'),'record',v.record)
        ORDER BY v.created_at,v.run_id,v.inputs_sha256)
      FROM enrichedlab_agent.broker_literature_reviews v JOIN enrichedlab_agent.broker_research_runs r
        ON r.tenant_id=v.tenant_id AND r.run_id=v.run_id
      WHERE r.tenant_id=p_tenant AND r.project_id=p_project),'[]'::jsonb) ELSE '[]'::jsonb END);
END;
$$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_context(text,uuid,uuid,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_project_package_components(text,uuid,uuid) FROM PUBLIC;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (70) ON CONFLICT DO NOTHING;
COMMIT;
