-- A person's review holds its analysis in the working plan. The coordinator leaves a step whose newest review is not
-- accepted out of every plan archive, but a review recorded between its read and the archive would still move that
-- step, and an archived analysis's review no longer reaches the lead. This archive function (048's, unchanged
-- otherwise) locks the moved steps' task rows, which conflicts with the row share lock a review write takes, and
-- refuses with plan_archive_reviewed when any moved step's newest person review is needs_changes or rejected. Only
-- person reviews count, as the coordinator's results read (064) shows them, so a service account's review retained
-- from before migration 024 neither holds a step there nor refuses it here. The coordinator reads again and decides
-- without that step. Schema version 87 gates coordinators that expect the refusal.
--
-- An earlier coordinator meets the refusal only when it would have moved a reviewed step, and then fails that turn
-- instead of hiding the review.
--
-- Rollback: move coordinators to an image without 087, run 048's broker_archive_research_plan and 086's
-- broker_schema_ready again, and delete schema_migrations version 87. No rows change.
BEGIN;
CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_archive_research_plan(p_tenant text, p_run uuid, p_worker text,
  p_generation bigint, p_expected bigint, p_keys text[])
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; prior jsonb; kept jsonb; moved jsonb; v_revision bigint;
  v_page integer;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run FOR UPDATE;
  -- Every comparison is NULL-safe: a missing worker, generation or revision must refuse, not pass.
  IF NOT FOUND OR r.status IS DISTINCT FROM 'running' OR r.lease_owner IS DISTINCT FROM p_worker
    OR r.generation IS DISTINCT FROM p_generation OR (r.lease_until > clock_timestamp()) IS NOT TRUE THEN
    RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  IF NOT enrichedlab_agent.broker_research_authorized(p_tenant,r.actor_id,r.project_id,r.allowed_profiles) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden'; END IF;
  PERFORM 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=r.project_id AND revision=p_expected FOR UPDATE;
  IF NOT FOUND THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  SELECT state INTO prior FROM enrichedlab_agent.broker_project_checkpoints WHERE tenant_id=p_tenant AND project_id=r.project_id AND revision=p_expected;
  IF p_keys IS NULL OR cardinality(p_keys) NOT BETWEEN 1 AND 128
    OR cardinality(p_keys) <> (SELECT count(DISTINCT k) FROM unnest(p_keys) k)
    OR EXISTS (SELECT 1 FROM unnest(p_keys) k WHERE k IS NULL OR (SELECT count(*) FROM jsonb_array_elements(prior->'plan') s
      WHERE s->>'action_key'=k)<>1) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:plan_archive_invalid'; END IF;
  IF EXISTS (SELECT 1 FROM jsonb_array_elements(prior->'plan') s WHERE s->>'action_key'=ANY(p_keys) AND (
      (s->>'status' IN ('completed','failed','skipped')) IS NOT TRUE
      OR (s->>'task_id' IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
        WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND pt.task_id=(s->>'task_id')::uuid
          AND t.status IN ('completed','failed','cancelled'))))) THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:plan_archive_unfinished'; END IF;
  -- A person's review holds its analysis in the working plan until it is accepted. Locking the moved steps' task rows
  -- conflicts with the row share lock broker_record_task_review takes, so a review either commits first and is seen
  -- here, or waits until this archive has committed.
  PERFORM 1 FROM enrichedlab_agent.broker_tasks t
    WHERE t.tenant_id=p_tenant AND t.task_id IN (SELECT (s->>'task_id')::uuid FROM jsonb_array_elements(prior->'plan') s
      WHERE s->>'action_key'=ANY(p_keys) AND s->>'task_id' IS NOT NULL)
    ORDER BY t.task_id FOR UPDATE;
  IF EXISTS (SELECT 1 FROM jsonb_array_elements(prior->'plan') s
      -- Person reviews only, as the coordinator's results read (064) shows them; a retained service-account review
      -- neither holds a step there nor refuses it here.
      CROSS JOIN LATERAL (SELECT v.decision FROM enrichedlab_agent.broker_task_reviews v
        JOIN enrichedlab_agent.broker_principals p ON p.tenant_id = v.tenant_id AND p.principal_id = v.reviewer_id
        WHERE v.tenant_id=p_tenant AND v.task_id=(s->>'task_id')::uuid AND p.principal_class = 'person'
        ORDER BY v.created_at DESC, v.review_id DESC LIMIT 1) n
      WHERE s->>'action_key'=ANY(p_keys) AND s->>'task_id' IS NOT NULL AND n.decision <> 'accepted') THEN
    RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:plan_archive_reviewed'; END IF;
  SELECT coalesce(jsonb_agg(e.s ORDER BY e.n) FILTER (WHERE NOT (e.s->>'action_key'=ANY(p_keys))),'[]'::jsonb),
         coalesce(jsonb_agg(e.s ORDER BY e.n) FILTER (WHERE e.s->>'action_key'=ANY(p_keys)),'[]'::jsonb)
    INTO kept, moved FROM jsonb_array_elements(prior->'plan') WITH ORDINALITY AS e(s,n);
  v_revision:=enrichedlab_agent.broker_write_checkpoint(p_tenant,r.actor_id,r.project_id,p_expected,gen_random_uuid(),
    jsonb_set(prior,'{plan}',kept),'plan_archive',p_run,NULL,NULL);
  SELECT coalesce(max(page),0)+1 INTO v_page FROM enrichedlab_agent.broker_plan_pages
    WHERE tenant_id=p_tenant AND project_id=r.project_id;
  INSERT INTO enrichedlab_agent.broker_plan_pages VALUES (p_tenant,r.project_id,v_page,v_revision,p_run,moved,
    'sha256:'||encode(sha256(convert_to(moved::text,'UTF8')),'hex'),clock_timestamp());
  INSERT INTO enrichedlab_agent.broker_plan_archived_keys
    SELECT p_tenant,r.project_id,e.s->>'action_key',v_page,e.n::integer,(e.s->>'task_id')::uuid,t.submitted_at,e.s->>'status'
    FROM jsonb_array_elements(moved) WITH ORDINALITY AS e(s,n)
    LEFT JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id=p_tenant AND t.task_id=(e.s->>'task_id')::uuid;
  RETURN jsonb_build_object('revision',v_revision,'page',v_page,'steps',jsonb_array_length(moved));
END $$;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,
    31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,
    59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version)
$$;

INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (87) ON CONFLICT DO NOTHING;
COMMIT;
