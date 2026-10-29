-- The project list names who started each project, its question, the reader's own access and where its latest work
-- session and analyses stand, so a lab index can group and filter projects without opening each one.
-- Same signature, grant and authorization as before; a project the reader cannot read is still not listed.
BEGIN;

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
      'latest_run',(SELECT jsonb_build_object('run_id',r.run_id,'status',r.status,'code',r.code,'turn',r.turn,
          'max_turns',r.max_turns,'updated_at',r.updated_at)
        FROM enrichedlab_agent.broker_research_runs r
        WHERE r.tenant_id=p.tenant_id AND r.project_id=p.project_id ORDER BY r.created_at DESC,r.run_id LIMIT 1),
      'task_counts',(SELECT jsonb_build_object('total',count(*),
          'active',count(*) FILTER (WHERE t.status IN ('queued','leased','starting','running','cancelling')),
          'finished',count(*) FILTER (WHERE t.status IN ('completed','review_required')),
          'failed',count(*) FILTER (WHERE t.status='failed'))
        FROM enrichedlab_agent.broker_project_tasks pt
        JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=pt.tenant_id AND t.task_id=pt.task_id
        WHERE pt.tenant_id=p.tenant_id AND pt.project_id=p.project_id))
    FROM enrichedlab_agent.broker_projects p
    JOIN enrichedlab_agent.broker_principals o ON o.tenant_id=p.tenant_id AND o.principal_id=p.owner_id
    WHERE p.tenant_id=p_tenant AND enrichedlab_agent.broker_project_can(p_tenant,p_actor,p.project_id,'read')
    ORDER BY p.updated_at DESC,p.project_id LIMIT p_limit;
END $$;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_list_projects(text,uuid,integer) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (20) ON CONFLICT DO NOTHING;
COMMIT;
