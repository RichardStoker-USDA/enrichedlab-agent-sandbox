-- Methods review findings and revisions. The methods specialist may send a proposal back to the lead
-- with typed findings instead of stopping the run for a person: the turn commits with outcome `revise`, submits no
-- analysis, keeps the proposal it sent back in the turn record as `revised_proposal` (never `proposal`, so no reader
-- takes it for an answer) and returns the run to the queue for the lead's next call. A run may be sent back twice; the
-- database refuses a third, and the coordinator then records the review as one a person must decide. The run's results
-- read gives the newest turn's review when it was a revision, and how many revisions this run used.
-- The inbox shows the methods check's reason for a run stopped after its revisions were used. Everything else in the three
-- functions is migrations 040's (the turn commit and the inbox) and 045's (the results read).
-- Rollback: an older coordinator never commits `revise` and ignores the added keys.
BEGIN;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_commit_research_turn(p_tenant text, p_run uuid, p_worker text, p_generation bigint, p_expected bigint, p_state jsonb, p_actions jsonb, p_record jsonb, p_outcome text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'enrichedlab_agent'
AS $function$
DECLARE r enrichedlab_agent.broker_research_runs%ROWTYPE; project enrichedlab_agent.broker_projects%ROWTYPE;
  tmpl enrichedlab_agent.broker_profile_templates%ROWTYPE;
  a jsonb; code jsonb; task uuid; tasks jsonb:='[]'; plan jsonb; revision bigint; pending integer;
  cfg jsonb; chosen jsonb; rendered text; pick record; fill record; described jsonb;
  src enrichedlab_agent.broker_sql_sources%ROWTYPE; q jsonb;
BEGIN
  SELECT * INTO r FROM enrichedlab_agent.broker_research_runs WHERE tenant_id=p_tenant AND run_id=p_run FOR UPDATE;
  IF NOT FOUND OR r.status<>'running' OR r.lease_owner IS DISTINCT FROM p_worker OR r.generation<>p_generation OR r.lease_until<=clock_timestamp() THEN
    RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='broker:lease_fence_lost'; END IF;
  SELECT * INTO project FROM enrichedlab_agent.broker_projects WHERE tenant_id=p_tenant AND project_id=r.project_id FOR UPDATE;
  IF NOT enrichedlab_agent.broker_research_authorized(p_tenant,r.actor_id,r.project_id,r.allowed_profiles) THEN
    RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='broker:profile_forbidden'; END IF;
  IF project.revision<>p_expected THEN RAISE EXCEPTION USING ERRCODE='23505',MESSAGE='broker:checkpoint_conflict'; END IF;
  IF r.turn>=r.max_turns OR p_outcome NOT IN ('delegate','review_required','paused','revise')
    OR jsonb_typeof(p_actions) IS DISTINCT FROM 'array' OR jsonb_array_length(p_actions)>r.max_parallel
    OR (p_outcome='delegate') IS DISTINCT FROM (jsonb_array_length(p_actions)>0)
    OR jsonb_typeof(p_record) IS DISTINCT FROM 'object' OR octet_length(p_record::text)>131072 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  -- A revision keeps the proposal it sent back as `revised_proposal`, never as `proposal`, so no
  -- reader of answers takes it for one; a run is sent back at most twice.
  IF p_outcome='revise' AND (p_record ? 'proposal' OR jsonb_typeof(p_record->'revised_proposal') IS DISTINCT FROM 'object') THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
  IF p_outcome='revise' AND (SELECT count(*) FROM enrichedlab_agent.broker_research_turns t
      WHERE t.tenant_id=p_tenant AND t.run_id=p_run AND t.record->>'outcome'='revise') >= 2 THEN
    RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:revision_budget_spent'; END IF;
  SELECT count(*) INTO pending FROM enrichedlab_agent.broker_project_tasks pt JOIN enrichedlab_agent.broker_tasks t USING(tenant_id,task_id)
    WHERE pt.tenant_id=p_tenant AND pt.project_id=r.project_id AND t.status IN ('queued','leased','starting','running','cancelling')
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr WHERE fr.tenant_id=pt.tenant_id AND fr.task_id=pt.task_id);
  IF pending+jsonb_array_length(p_actions)>r.max_parallel THEN RAISE EXCEPTION USING ERRCODE='53300',MESSAGE='broker:group_queue_full'; END IF;
  FOR a IN SELECT value FROM jsonb_array_elements(p_actions) LOOP
    IF (a->>'profile'=ANY(r.allowed_profiles)) IS NOT TRUE
      OR (a->>'action_key' ~ '^[a-zA-Z0-9_-]{1,100}$') IS NOT TRUE
      OR (a->>'request_sha256' ~ '^[0-9a-f]{64}$') IS NOT TRUE OR NOT EXISTS (
        SELECT 1 FROM jsonb_array_elements(p_state->'plan') s WHERE s->>'action_key'=a->>'action_key' AND s->>'status'='pending' AND s->>'task_id' IS NULL
      ) OR EXISTS (SELECT 1 FROM enrichedlab_agent.broker_project_tasks WHERE tenant_id=p_tenant AND project_id=r.project_id AND action_key=a->>'action_key') THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:research_run_invalid'; END IF;
    code:=a->'generated_code'; cfg:=NULL; described:=NULL; q:=a->'query';
    SELECT * INTO tmpl FROM enrichedlab_agent.broker_profile_templates WHERE tenant_id=p_tenant AND profile_key=a->>'profile';
    IF FOUND THEN
      -- Every comparison is NULL-safe: a missing field must refuse, not pass.
      IF jsonb_typeof(code) IS DISTINCT FROM 'object'
        OR EXISTS (SELECT 1 FROM jsonb_object_keys(code) k WHERE k NOT IN ('template_ref','path','content','sha256','admission','configuration'))
        OR (code->>'template_ref') IS DISTINCT FROM tmpl.template_ref
        OR (code->>'path') IS DISTINCT FROM tmpl.block_path
        OR jsonb_typeof(code->'content') IS DISTINCT FROM 'string'
        OR (octet_length(code->>'content') BETWEEN 1 AND 16384) IS NOT TRUE
        OR (code->>'sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(code->>'content','UTF8')),'hex')
        OR jsonb_typeof(code->'admission') IS DISTINCT FROM 'object'
        OR (code->'admission'->>'result') IS DISTINCT FROM 'accepted'
        OR (code->'admission'->>'block_sha256') IS DISTINCT FROM (code->>'sha256') THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
      -- A configured method carries settings, never code: render its block again from the
      -- template row and refuse any difference. A code template carries no configuration.
      cfg:=tmpl.contract->'configuration';
      IF cfg IS NULL THEN
        IF code ? 'configuration' THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
      ELSE
        chosen:=code->'configuration'->'settings';
        IF jsonb_typeof(code->'configuration') IS DISTINCT FROM 'object'
          OR EXISTS (SELECT 1 FROM jsonb_object_keys(code->'configuration') k WHERE k NOT IN ('schema','block_sha256','settings'))
          OR (code->'configuration'->>'schema') IS DISTINCT FROM 'enrichedlab.configured-block.v1'
          OR (cfg->>'schema') IS DISTINCT FROM 'enrichedlab.configured-block.v1'
          OR jsonb_typeof(cfg->'block') IS DISTINCT FROM 'string'
          OR (cfg->>'block_sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(cfg->>'block','UTF8')),'hex')
          OR (code->'configuration'->>'block_sha256') IS DISTINCT FROM (cfg->>'block_sha256')
          OR jsonb_typeof(chosen) IS DISTINCT FROM 'object'
          OR jsonb_typeof(cfg->'settings') IS DISTINCT FROM 'object'
          OR (SELECT array_agg(k ORDER BY k) FROM jsonb_object_keys(chosen) k)
            IS DISTINCT FROM (SELECT array_agg(k ORDER BY k) FROM jsonb_object_keys(cfg->'settings') k) THEN
          RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
        rendered:=cfg->>'block'; described:='[]'::jsonb;
        FOR pick IN SELECT key,value FROM jsonb_each(chosen) ORDER BY key LOOP
          IF jsonb_typeof(pick.value) IS DISTINCT FROM 'string'
            OR jsonb_typeof(cfg->'settings'->pick.key->'choices'->(pick.value#>>'{}')->'slots') IS DISTINCT FROM 'object' THEN
            RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
          FOR fill IN SELECT key,value FROM jsonb_each_text(cfg->'settings'->pick.key->'choices'->(pick.value#>>'{}')->'slots') LOOP
            rendered:=replace(rendered,'{{'||fill.key||'}}',fill.value);
          END LOOP;
          -- The words the workspace shows come from the template row, never from the proposal.
          described:=described||jsonb_build_array(jsonb_build_object('setting',pick.key,
            'label',cfg->'settings'->pick.key->>'label','choice',pick.value#>>'{}',
            'choice_label',cfg->'settings'->pick.key->'choices'->(pick.value#>>'{}')->>'label'));
        END LOOP;
        IF strpos(rendered,'{{')>0 OR rendered IS DISTINCT FROM (code->>'content') THEN
          RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid'; END IF;
      END IF;
    ELSIF code IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:generated_code_invalid';
    END IF;
    -- A source's method carries the query its guard admitted, and nothing else runs it: no code, and
    -- no query for any other method. The guard ran in the coordinator; the row keeps its text and digest.
    SELECT * INTO src FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id=p_tenant AND profile_key=a->>'profile';
    IF src.source_key IS NOT NULL THEN
      IF code IS NOT NULL OR jsonb_typeof(q) IS DISTINCT FROM 'object'
        OR EXISTS (SELECT 1 FROM jsonb_object_keys(q) k WHERE k NOT IN ('source','proposed_sql','admitted_sql','sha256','guard','relations','row_limit'))
        OR (q->>'source') IS DISTINCT FROM src.source_key
        OR jsonb_typeof(q->'proposed_sql') IS DISTINCT FROM 'string'
        OR (octet_length(q->>'proposed_sql') BETWEEN 1 AND 8192) IS NOT TRUE
        OR jsonb_typeof(q->'admitted_sql') IS DISTINCT FROM 'string'
        OR (octet_length(q->>'admitted_sql') BETWEEN 1 AND 8300) IS NOT TRUE
        OR (q->>'sha256') IS DISTINCT FROM 'sha256:'||encode(sha256(convert_to(q->>'admitted_sql','UTF8')),'hex')
        OR (q->>'guard') IS DISTINCT FROM 'guarded-sql/v1'
        OR jsonb_typeof(q->'relations') IS DISTINCT FROM 'array' OR jsonb_array_length(q->'relations')=0
        OR EXISTS (SELECT 1 FROM jsonb_array_elements(q->'relations') x WHERE jsonb_typeof(x) IS DISTINCT FROM 'string'
          OR NOT EXISTS (SELECT 1 FROM jsonb_array_elements(src.relations) s WHERE s->>'name'=x#>>'{}'))
        OR (CASE WHEN (q->>'row_limit') ~ '^[0-9]{1,6}$' THEN (q->>'row_limit')::integer BETWEEN 0 AND src.row_cap ELSE false END) IS NOT TRUE THEN
        RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:query_invalid'; END IF;
    ELSIF q IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='broker:query_invalid';
    END IF;
    task:=enrichedlab_agent.broker_submit_project_task(p_tenant,r.actor_id,r.project_id,a->>'action_key',NULL,project.group_key,a->>'profile',a->>'objective',
      'research-'||p_run::text||'-'||(r.turn+1)::text||'-'||(a->>'action_key'),a->>'request_sha256');
    IF src.source_key IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_task_queries(tenant_id,task_id,source_key,profile_key,proposed_sql,admitted_sql,
        sql_sha256,guard,relations,row_limit,run_id,turn)
        VALUES(p_tenant,task,src.source_key,src.profile_key,q->>'proposed_sql',q->>'admitted_sql',q->>'sha256',q->>'guard',
          ARRAY(SELECT jsonb_array_elements_text(q->'relations')),(q->>'row_limit')::integer,p_run,r.turn+1);
    END IF;
    IF code IS NOT NULL THEN
      INSERT INTO enrichedlab_agent.broker_task_generated_code(tenant_id,task_id,profile_key,template_ref,template_sha256,path,
        content,size_bytes,sha256,admission,run_id,turn,configuration)
        VALUES(p_tenant,task,a->>'profile',tmpl.template_ref,tmpl.template_sha256,tmpl.block_path,code->>'content',
          octet_length(code->>'content'),code->>'sha256',code->'admission',p_run,r.turn+1,
          CASE WHEN cfg IS NULL THEN NULL ELSE (code->'configuration')||jsonb_build_object('described',described) END);
    END IF;
    tasks:=tasks||jsonb_build_array(jsonb_build_object('task_id',task,'action_key',a->>'action_key'));
    SELECT jsonb_agg(CASE WHEN s->>'action_key'=a->>'action_key' THEN s||jsonb_build_object('task_id',task,'status','active') ELSE s END ORDER BY ord)
      INTO plan FROM jsonb_array_elements(p_state->'plan') WITH ORDINALITY AS e(s,ord);
    p_state:=jsonb_set(p_state,'{plan}',plan);
  END LOOP;
  revision:=enrichedlab_agent.broker_write_checkpoint(p_tenant,r.actor_id,r.project_id,p_expected,gen_random_uuid(),p_state,'lead_turn',p_run,r.turn+1,NULL);
  INSERT INTO enrichedlab_agent.broker_research_turns VALUES(p_tenant,p_run,r.turn+1,p_generation,p_expected,revision,
    p_record||jsonb_build_object('submitted_tasks',tasks,'actor_id',r.actor_id,'outcome',p_outcome),clock_timestamp());
  UPDATE enrichedlab_agent.broker_research_runs SET turn=turn+1,failures=0,
    status=CASE WHEN p_outcome='delegate' THEN 'waiting' WHEN p_outcome='revise' THEN 'queued' ELSE p_outcome END,
    code=CASE WHEN p_outcome='delegate' THEN 'waiting_for_tools' ELSE p_outcome END,
    available_at=clock_timestamp()+interval '5 seconds',lease_owner=NULL,lease_until=NULL,updated_at=clock_timestamp()
    WHERE tenant_id=p_tenant AND run_id=p_run;
  RETURN jsonb_build_object('revision',revision,'tasks',tasks,'turn',r.turn+1);
END $function$;

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
      'digest', d.digest) AS item,
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
           'targets', coalesce(t.record->'revision'->'targets', '[]'::jsonb))
    INTO revision
  FROM enrichedlab_agent.broker_research_turns t
  WHERE t.tenant_id = p_tenant AND t.run_id = p_run AND t.turn = r.turn AND t.record->>'outcome' = 'revise';
  RETURN jsonb_build_object('results', results, 'earlier_findings', findings, 'latest_answer', latest, 'reviews', reviews,
    'ledger', ledger, 'attempts', attempts, 'revisions', revisions, 'revision_budget', 2, 'revision_request', revision);
END;
$function$;

-- Migration 040's inbox, except that a run stopped for a person after its revisions were used shows the methods check's
-- reason, as one stopped by `needs_review` does: the newest turn then holds a `revise` review the run could not act on.
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
      -- A figure version redraws results the project already holds, so it does not wait for review.
      AND NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_figure_requests fr
        WHERE fr.tenant_id = p_tenant AND fr.task_id = t.task_id)
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

REVOKE ALL ON FUNCTION enrichedlab_agent.broker_commit_research_turn(text,uuid,text,bigint,bigint,jsonb,jsonb,jsonb,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_research_run_results(text,uuid,text,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION enrichedlab_agent.broker_inbox(text, uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION enrichedlab_agent.broker_schema_ready(p_version integer)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,enrichedlab_agent AS $$
  SELECT p_version IN (2,3,7,8,9,10,11,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49) AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=2)
    AND EXISTS (SELECT 1 FROM enrichedlab_agent.schema_migrations WHERE version=p_version);
$$;
INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (49) ON CONFLICT DO NOTHING;
COMMIT;
