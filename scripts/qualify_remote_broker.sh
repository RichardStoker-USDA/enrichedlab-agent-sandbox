#!/bin/sh
set -eu

mode=${1:-all}
case "$mode" in
    all|campaigns|runtime-events|direct-readiness|custodian-role|evidence|evidence-service|projects|project-stream|project-list|research-runs|review|generated-code|lineage|attempt-lineage|lineage-upstream|result-digests|configured-methods|person-review|lab-notes|lab-library|lead-library|tool-groups|knowledge-person|decision-contexts|working-copies|plan-ledger|plan-pages|methods-revision|specialist-texts|query-facts|lab-methods|inbox|reference-sql|figure-versions|answer-documents|workspace-reads|table-reads|result-kinds|figure-kinds|figure-previews|template-analyses|lab-datasets|lead-analyses|lab-objects|change-feed|live-output|identity-enrollment|artifact-recovery|orientation) ;;
    *) printf '%s\n' 'Usage: qualify_remote_broker.sh [all|campaigns|runtime-events|direct-readiness|custodian-role|evidence|evidence-service|projects|project-stream|project-list|research-runs|review|generated-code|lineage|attempt-lineage|lineage-upstream|result-digests|configured-methods|person-review|lab-notes|lab-library|lead-library|tool-groups|knowledge-person|decision-contexts|working-copies|plan-ledger|plan-pages|methods-revision|specialist-texts|query-facts|lab-methods|inbox|reference-sql|figure-versions|answer-documents|workspace-reads|table-reads|result-kinds|figure-kinds|figure-previews|template-analyses|lab-datasets|lead-analyses|lab-objects|change-feed|live-output|identity-enrollment|artifact-recovery|orientation]' >&2; exit 2 ;;
esac

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)
postgres_image=${ENRICHEDLAB_BROKER_POSTGRES_IMAGE:-postgres@sha256:1c59e2c3c818eaa0f0628f695b36e7c9e362d6b219b36a54a32df645cbd7e1af}
run_id=$$
container="enrichedlab-broker-pg-qual-$run_id"
volume="enrichedlab-broker-pg-qual-$run_id"
temp_root=$(mktemp -d "${TMPDIR:-/tmp}/enrichedlab-broker-qual.XXXXXX")
dsn_file="$temp_root/migration.dsn"
database=broker_qual
started=false

# With QUALIFY_EVIDENCE_CAPTURE=1, a passing mode also requires a complete record in every tenant: the recorder until
# the log is empty, every scope prepared, catch-up over every captured table, then no row without a record or a gap
# entry. Recorder and preparation calls are bounded as the append service bounds them. It prints the live and caught-up records and the gap
# entries by table and reason (rows a fixture wrote with triggers off, whose scope it made inconsistent, are expected
# among them).
capture_report() {
    [ "${QUALIFY_EVIDENCE_CAPTURE:-}" = 1 ] && [ "$started" = true ] || return 0
    rounds=0
    while [ "$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --command="SET statement_timeout = '800ms'; SELECT evidence.capture_record(200)->>'taken'")" != 0 ]; do
        rounds=$((rounds + 1)); [ "$rounds" -lt 1000 ] || return 1
    done
    for tenant in $(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
        --no-align --command="SELECT tenant_id FROM evidence.capture_tenants ORDER BY 1"); do
        rounds=0
        while [ "$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
            --no-align --set=ON_ERROR_STOP=on --command="SET statement_timeout = '800ms';
              SELECT evidence.capture_prepare_scopes(tenant_id, standalone_group, 50)->>'remaining'
              FROM evidence.capture_tenants WHERE tenant_id = '$tenant'")" != 0 ]; do
            rounds=$((rounds + 1)); [ "$rounds" -lt 1000 ] || return 1
        done
        docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on \
            --command="SELECT evidence.capture_catchup('$tenant')" >/dev/null || return 1
    done
    report=$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --set=ON_ERROR_STOP=on --command="SELECT (SELECT count(*) FILTER (WHERE payload::jsonb->>'origin' = 'live') || ' live, '
            || count(*) FILTER (WHERE payload::jsonb->>'origin' = 'caught_up') || ' caught up'
            FROM evidence.records WHERE event_type LIKE 'broker.%')
          || ' in ' || (SELECT count(*) FROM evidence.capture_tenants) || ' tenants, missing '
          || coalesce((SELECT sum((evidence.capture_missing(tenant_id)->>'missing')::bigint)
            FROM evidence.capture_tenants), 0)
          || ', gaps: ' || coalesce((SELECT string_agg(n || ' ' || table_name || ' ' || reason, ', ' ORDER BY table_name)
            FROM (SELECT table_name, reason, count(*) n FROM evidence.capture_gaps GROUP BY 1, 2) g), 'none')" \
        | tail -1) || return 1
    printf 'Evidence capture: %s\n' "$report"
    case "$report" in *'missing 0,'*) return 0 ;; *) return 1 ;; esac
}

cleanup() {
    status=$?
    if [ "$status" -eq 0 ] && ! capture_report; then
        status=1
    fi
    if [ "$started" = true ]; then
        docker rm --force "$container" >/dev/null 2>&1 || true
    fi
    docker volume rm "$volume" >/dev/null 2>&1 || true
    rm -rf "$temp_root"
    trap - EXIT
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

for command_name in cargo docker; do
    command -v "$command_name" >/dev/null 2>&1 || {
        printf '%s\n' "$command_name is required" >&2
        exit 1
    }
done

docker volume create "$volume" >/dev/null
docker run --detach \
    --name "$container" \
    --mount "type=volume,src=$volume,dst=/var/lib/postgresql" \
    --mount "type=bind,src=$repo_root,dst=/workspace,readonly" \
    --publish 127.0.0.1::5432 \
    --env POSTGRES_DB="$database" \
    --env POSTGRES_HOST_AUTH_METHOD=trust \
    "$postgres_image" >/dev/null
started=true

ready=false
attempt=0
while [ "$attempt" -lt 60 ]; do
    if docker exec "$container" pg_isready --host 127.0.0.1 --username postgres --dbname "$database" >/dev/null 2>&1; then
        ready=true
        break
    fi
    attempt=$((attempt + 1))
    sleep 1
done
[ "$ready" = true ] || {
    printf '%s\n' 'PostgreSQL did not become ready within 60 seconds' >&2
    exit 1
}

mapping=$(docker port "$container" 5432/tcp)
port=$(printf '%s\n' "$mapping" | awk -F: 'NF { value=$NF } END { print value }')
case "$port" in
    ''|*[!0-9]*) printf '%s\n' 'Docker did not return a numeric loopback port' >&2; exit 1 ;;
esac
umask 077
printf 'host=127.0.0.1 port=%s user=postgres dbname=%s sslmode=disable\n' "$port" "$database" >"$dsn_file"

cd "$repo_root"
cargo build --locked --bin agent-sandbox-broker >/dev/null
broker_bin="$repo_root/target/debug/agent-sandbox-broker"
"$broker_bin" migrate --migration-dsn-file "$dsn_file"

docker exec "$container" psql --username postgres --dbname "$database" \
    --set=ON_ERROR_STOP=on \
    --command='CREATE ROLE broker_api_qual LOGIN; CREATE ROLE broker_worker_qual LOGIN; CREATE ROLE broker_director_qual LOGIN; CREATE ROLE broker_custodian_qual LOGIN;'
docker exec "$container" psql --username postgres --dbname "$database" \
    --set=api_role=broker_api_qual \
    --set=worker_role=broker_worker_qual \
    --set=director_role=broker_director_qual \
    --set=custodian_role=broker_custodian_qual \
    --file=/workspace/migrations/postgres/grant_broker_runtime.psql

# Migration 075's evidence record: the SQL qualification under the real roles, then concurrent appends to one run
# stream, which must take consecutive sequences and leave a chain that verifies.
run_evidence() {
    evidence_log=$(mktemp "${TMPDIR:-/tmp}/enrichedlab-evidence-layout.XXXXXX")
    docker exec "$container" psql --username postgres --dbname "$database" --quiet \
        --file=/workspace/tests/fixtures/broker_evidence_qualification.sql
    pids=''
    for n in $(seq 1 24); do
        docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
            --set=ON_ERROR_STOP=on --command="SET SESSION AUTHORIZATION broker_api_qual; SELECT evidence.append(
              'evidence-site','broker/api','lab-a','project','00000000-0000-4000-8000-0000000000a1',
              '00000000-0000-4000-8000-00000000a0a1','step.started','parallel-key-$n',clock_timestamp(),p,
              encode(sha256(convert_to(p,'UTF8')),'hex')) FROM (SELECT '{\"n\":$n}'::text p) x;" >/dev/null &
        pids="$pids $!"
    done
    for pid in $pids; do wait "$pid"; done
    # Partition maintenance from two sessions at once: both must succeed.
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
        --set=ON_ERROR_STOP=on --command="SET SESSION AUTHORIZATION broker_worker_qual;
          SELECT evidence.ensure_record_partitions(12);" >/dev/null &
    first=$!
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
        --set=ON_ERROR_STOP=on --command="SET SESSION AUTHORIZATION broker_worker_qual;
          SELECT evidence.ensure_record_partitions(12);" >/dev/null &
    second=$!
    wait "$first"
    wait "$second"
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
        --set=ON_ERROR_STOP=on --command="DO \$\$ DECLARE s uuid; v jsonb; n bigint; m bigint; BEGIN
          SELECT stream_id INTO s FROM evidence.streams WHERE run_id='00000000-0000-4000-8000-00000000a0a1';
          SELECT count(*), max(sequence) INTO n, m FROM evidence.receipts WHERE stream_id = s;
          v := evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001', s, 'audit');
          IF n <> 27 OR m <> 27 OR NOT (v->>'valid')::boolean THEN
            RAISE EXCEPTION 'concurrent appends: % receipts, max sequence %, chain %', n, m, v; END IF; END \$\$;"
    docker exec "$container" psql --username postgres --dbname "$database" --quiet \
        --file=/workspace/tests/fixtures/broker_evidence_lineage_qualification.sql
    docker exec "$container" psql --username postgres --dbname "$database" --quiet \
        --file=/workspace/tests/fixtures/broker_evidence_capture_qualification.sql
    # Capture concurrency. Helpers: run psql in the container; wait until a session matching a marker waits on a lock.
    pg() { docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on "$@"; }
    await_lock_wait() {
        tries=0
        until [ "$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
            --no-align --command="SELECT count(*) FROM pg_stat_activity WHERE query LIKE '%$1%'
              AND wait_event_type = 'Lock' AND pid <> pg_backend_pid()")" -ge 1 ]; do
            tries=$((tries + 1)); [ "$tries" -lt 100 ] || { printf '%s\n' "$2" >&2; exit 1; }
            sleep 0.1
        done
    }
    await_active() {
        tries=0
        until [ "$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
            --no-align --command="SELECT count(*) FROM pg_stat_activity WHERE query LIKE '%$1%' AND state = 'active'
              AND pid <> pg_backend_pid()")" -ge 1 ]; do
            tries=$((tries + 1)); [ "$tries" -lt 100 ] || { printf '%s\n' "$2" >&2; exit 1; }
            sleep 0.1
        done
    }
    event() { # <task suffix> <sequence>
        printf "INSERT INTO enrichedlab_agent.broker_task_events (tenant_id, task_id, sequence, kind, state, code, metadata,
          occurred_at) VALUES ('capture-site', '20000000-0000-4000-8000-0000000000%s', %s, 'state', 'running', 'running',
          '{}', '2026-10-25T05:07:00Z');" "$1" "$2"
    }
    record() { pg --tuples-only --no-align --command="SET statement_timeout = '800ms'; SELECT evidence.capture_record(10000)->>'taken'"; }
    # The recorder defers a batch without waiting for the held lock: three calls, each deferred, the fastest well under
    # the recorder's 200 ms lock wait (a lock it waited for would take at least that long).
    deferred_at_once() {
        pg --command="SET statement_timeout = '800ms';
            DO \$\$ DECLARE t timestamptz; best interval := interval '1 hour'; r jsonb; BEGIN
              FOR i IN 1..3 LOOP
                t := clock_timestamp();
                r := evidence.capture_record(10000);
                IF r->>'deferred' IS DISTINCT FROM 'true' THEN RAISE EXCEPTION 'not deferred'; END IF;
                best := least(best, clock_timestamp() - t);
              END LOOP;
              IF best > interval '150 milliseconds' THEN RAISE EXCEPTION 'the recorder waited % before deferring', best; END IF;
            END \$\$;" >/dev/null 2>"$evidence_log.third" || { printf '%s\n' "$1" >&2; cat "$evidence_log.third" >&2; exit 1; }
    }
    # 1. Writers take no stream lock: while a session holds lab A's lab stream (as an append does), two writers each
    # commit an event for c1 (that stream) and one for c3 (lab A's project stream), in opposite orders, without waiting.
    pg --command="BEGIN; SELECT 1 FROM evidence.streams WHERE tenant_id = 'capture-site' AND group_key = 'lab-a'
        AND scope_kind = 'lab' AND project_id IS NULL AND run_id IS NULL FOR UPDATE; SELECT pg_sleep(6); COMMIT;" \
        >/dev/null &
    holder=$!
    await_active 'pg_sleep(6)' 'the stream holder did not start'
    pg --command="BEGIN; /* writer-one */ $(event c1 11) $(event c3 11) COMMIT;" >/dev/null
    pg --command="BEGIN; /* writer-two */ $(event c3 12) $(event c1 12) COMMIT;" >/dev/null
    [ "$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --command="SELECT count(*) FROM pg_stat_activity WHERE query LIKE '%pg_sleep(6)%' AND state = 'active'
          AND pid <> pg_backend_pid()")" -ge 1 ] \
        || { printf '%s\n' 'a writer waited for a held stream' >&2; exit 1; }
    wait "$holder"
    # 2. No lock inversion. T2 holds task c4's row and writes an event into lab A's project stream; T1 writes an event into
    # that stream, makes every constraint immediate (once a way to record early) and then waits for c4's row. T2 commits
    # and then T1: neither takes a stream lock, so neither deadlocks.
    pg --command="BEGIN; /* holder-t2 */ SELECT 1 FROM enrichedlab_agent.broker_tasks WHERE tenant_id = 'capture-site'
        AND task_id = '20000000-0000-4000-8000-0000000000c4' FOR UPDATE; $(event c3 15) SELECT pg_sleep(4); COMMIT;" \
        >/dev/null 2>"$evidence_log.first" &
    t2=$!
    await_active 'pg_sleep(4)' 'the row holder did not start'
    pg --command="BEGIN; /* waiter-t1 */ $(event c3 16) SET CONSTRAINTS ALL IMMEDIATE;
        SELECT 1 FROM enrichedlab_agent.broker_tasks WHERE tenant_id = 'capture-site'
          AND task_id = '20000000-0000-4000-8000-0000000000c4' FOR UPDATE; COMMIT;" >/dev/null 2>"$evidence_log.second" &
    t1=$!
    await_lock_wait 'waiter-t1' 'the second transaction did not wait for the held row'
    t2_status=0; wait "$t2" || t2_status=$?
    t1_status=0; wait "$t1" || t1_status=$?
    if [ "$t1_status" -ne 0 ] || [ "$t2_status" -ne 0 ]; then
        printf 'lock order: %s %s\n' "$t1_status" "$t2_status" >&2
        cat "$evidence_log.first" "$evidence_log.second" >&2
        exit 1
    fi
    # 3. A held project row defers only the recorder. One session holds the row of a project whose scope was never
    # prepared (written with triggers off), then writes an event and commits. Meanwhile a writer commits an event into
    # lab A's project stream and the held project's first captured row without waiting; the recorder, which would
    # prepare that scope, does not wait for the row: it defers the batch, and records everything, with no gap, once the
    # row is released.
    pg --command="SET session_replication_role = replica;
        INSERT INTO enrichedlab_agent.broker_projects (tenant_id, project_id, group_key, owner_id, title, visibility)
          VALUES ('capture-site', '20000000-0000-4000-8000-0000000000a2', 'lab-a',
            '20000000-0000-4000-8000-000000000001', 'Second capture project', 'lab');
        INSERT INTO enrichedlab_agent.broker_tasks (tenant_id, task_id, principal_id, group_key, profile_key, objective,
            status, desired_status, attempt, max_task_attempts, logical_agent, profile_snapshot, idempotency_key,
            request_sha256, lease_owner, lease_generation, lease_expires_at, submitted_at)
          VALUES ('capture-site', '20000000-0000-4000-8000-0000000000c5', '20000000-0000-4000-8000-000000000001',
            'lab-a', 'general-research', 'Synthetic capture task', 'completed', 'run', 1, 1,
            'task-200000000000400080000000000000c5', '{}', 'capture-c5', repeat('a', 64), NULL, 0, NULL,
            '2026-10-25T05:00:00Z');
        INSERT INTO enrichedlab_agent.broker_project_tasks (tenant_id, project_id, task_id, action_key, parent_task_id)
          VALUES ('capture-site', '20000000-0000-4000-8000-0000000000a2', '20000000-0000-4000-8000-0000000000c5',
            'step-5', NULL);" >/dev/null
    record >/dev/null
    pg --command="BEGIN; SELECT 1 FROM enrichedlab_agent.broker_projects WHERE tenant_id = 'capture-site'
        AND project_id = '20000000-0000-4000-8000-0000000000a2' FOR UPDATE; SELECT pg_sleep(8); $(event c3 14) COMMIT;" \
        >/dev/null 2>"$evidence_log.first" &
    holder=$!
    await_active 'pg_sleep(8)' 'the project lock holder did not start'
    pg --command="BEGIN; /* writer-three */ $(event c3 13) $(event c5 1) COMMIT;" >/dev/null
    [ "$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --command="SELECT count(*) FROM pg_stat_activity WHERE query LIKE '%pg_sleep(8)%' AND state = 'active'
          AND pid <> pg_backend_pid()")" -ge 1 ] \
        || { printf '%s\n' 'the writer finished only after the project row was released' >&2; exit 1; }
    deferred_at_once 'the recorder did not defer a batch whose project row was held'
    first_status=0; wait "$holder" || first_status=$?
    if [ "$first_status" -ne 0 ]; then
        printf 'held project: %s\n' "$first_status" >&2
        cat "$evidence_log.first" >&2
        exit 1
    fi
    record >/dev/null
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --command="SELECT count(*) FROM evidence.records rc JOIN evidence.producers p ON p.tenant_id = rc.tenant_id
            AND p.producer_id = rc.producer_id WHERE rc.tenant_id = 'capture-site' AND p.identity = 'broker:capture'
            AND rc.event_type = 'broker.task_events' AND rc.payload::jsonb->>'origin' = 'live'
            AND ((rc.payload::jsonb->'key'->>'sequence')::int IN (11, 12, 13, 14, 15, 16)
              OR rc.project_id = '20000000-0000-4000-8000-0000000000a2')" \
        | grep -qx 9 || { printf '%s\n' 'the concurrent writers were not all recorded live' >&2; exit 1; }
    pg --tuples-only --no-align --command="SELECT count(*) FROM evidence.capture_gaps WHERE tenant_id = 'capture-site'
        AND project_id = '20000000-0000-4000-8000-0000000000a2'" | grep -qx 0 \
        || { printf '%s\n' 'the held project row left a gap entry' >&2; exit 1; }
    # 3b. Capture never takes part in a deadlock with a broker transaction. Two projects have scopes never prepared and
    # one captured event each. A broker transaction locks the second project's row, waits, then locks the first's; the
    # recorder runs in between and would share-lock the first and wait for the second. It defers instead, and the broker
    # transaction commits.
    pg --command="SET session_replication_role = replica;
        INSERT INTO enrichedlab_agent.broker_projects (tenant_id, project_id, group_key, owner_id, title, visibility)
          VALUES ('capture-site', '20000000-0000-4000-8000-0000000000a3', 'lab-a',
                  '20000000-0000-4000-8000-000000000001', 'Third capture project', 'lab'),
                 ('capture-site', '20000000-0000-4000-8000-0000000000a4', 'lab-a',
                  '20000000-0000-4000-8000-000000000001', 'Fourth capture project', 'lab');
        INSERT INTO enrichedlab_agent.broker_tasks (tenant_id, task_id, principal_id, group_key, profile_key, objective,
            status, desired_status, attempt, max_task_attempts, logical_agent, profile_snapshot, idempotency_key,
            request_sha256, lease_owner, lease_generation, lease_expires_at, submitted_at)
          SELECT 'capture-site', t.task_id, '20000000-0000-4000-8000-000000000001', 'lab-a', 'general-research',
            'Synthetic capture task', 'completed', 'run', 1, 1, 'task-' || replace(t.task_id::text, '-', ''), '{}',
            'capture-' || right(t.task_id::text, 2), repeat('a', 64), NULL, 0, NULL, '2026-10-25T05:00:00Z'
          FROM (VALUES ('20000000-0000-4000-8000-0000000000c7'::uuid), ('20000000-0000-4000-8000-0000000000c8'::uuid))
            t(task_id);" >/dev/null
    pg --command="INSERT INTO enrichedlab_agent.broker_project_tasks (tenant_id, project_id, task_id, action_key,
          parent_task_id)
        VALUES ('capture-site', '20000000-0000-4000-8000-0000000000a3', '20000000-0000-4000-8000-0000000000c7',
                'step-a3', NULL),
               ('capture-site', '20000000-0000-4000-8000-0000000000a4', '20000000-0000-4000-8000-0000000000c8',
                'step-a4', NULL);" >/dev/null
    pg --command="BEGIN; /* broker-two-projects */ SELECT 1 FROM enrichedlab_agent.broker_projects
        WHERE tenant_id = 'capture-site' AND project_id = '20000000-0000-4000-8000-0000000000a4' FOR UPDATE;
        SELECT pg_sleep(3);
        SELECT 1 FROM enrichedlab_agent.broker_projects
        WHERE tenant_id = 'capture-site' AND project_id = '20000000-0000-4000-8000-0000000000a3' FOR UPDATE;
        COMMIT;" >/dev/null 2>"$evidence_log.first" &
    broker=$!
    await_active 'pg_sleep(3)' 'the two-project broker transaction did not start'
    deferred_at_once 'the recorder did not defer a batch whose second project row was held'
    broker_status=0; wait "$broker" || broker_status=$?
    [ "$broker_status" -eq 0 ] || { cat "$evidence_log.first" >&2; printf '%s\n' 'the broker transaction failed' >&2; exit 1; }
    record >/dev/null
    pg --tuples-only --no-align --command="SELECT count(*) FROM evidence.records WHERE tenant_id = 'capture-site'
        AND event_type = 'broker.project_tasks' AND project_id IN ('20000000-0000-4000-8000-0000000000a3',
          '20000000-0000-4000-8000-0000000000a4')" | grep -qx 2 \
        || { printf '%s\n' 'the two projects were not recorded after the broker transaction' >&2; exit 1; }
    # 3c. A held stream defers the recorder within its bound instead of blocking it: a session holds lab A's lab stream
    # (as an evidence append does in an open transaction) while an event for that stream is logged.
    pg --command="BEGIN; SELECT 1 FROM evidence.streams WHERE tenant_id = 'capture-site' AND group_key = 'lab-a'
        AND scope_kind = 'lab' AND project_id IS NULL AND run_id IS NULL FOR UPDATE; SELECT pg_sleep(3); COMMIT;" \
        >/dev/null &
    holder=$!
    await_active 'pg_sleep(3)' 'the stream holder did not start'
    pg --command="$(event c1 18)" >/dev/null
    deferred_at_once 'the recorder did not defer a batch whose stream was held'
    wait "$holder"
    record >/dev/null
    pg --tuples-only --no-align --command="SELECT count(*) FROM evidence.records WHERE tenant_id = 'capture-site'
        AND event_type = 'broker.task_events' AND (payload::jsonb->'key'->>'sequence')::int = 18" | grep -qx 1 \
        || { printf '%s\n' 'the held stream'"'"'s event was not recorded after its release' >&2; exit 1; }
    # 3d. A held opt-in row (as a catch-up holds its tenant's) defers the recorder in the same way.
    pg --command="BEGIN; SELECT 1 FROM evidence.capture_tenants WHERE tenant_id = 'capture-site' FOR NO KEY UPDATE;
        SELECT pg_sleep(3); COMMIT;" >/dev/null &
    holder=$!
    await_active 'pg_sleep(3)' 'the opt-in row holder did not start'
    pg --command="$(event c1 19)" >/dev/null
    deferred_at_once 'the recorder did not defer a batch whose tenant row was held'
    wait "$holder"
    record >/dev/null
    # 3e. The append service's role cannot run the recorder inside a transaction it controls: the procedure commits first,
    # which a transaction block refuses before any lock is taken, so no caller can keep the recorder's locks.
    # Each statement is its own message, as a driver sends them (a multi-statement string is one implicit transaction,
    # where the procedure cannot commit either).
    if pg --command="SET SESSION AUTHORIZATION evidence_adapter_qual" --command="SET statement_timeout = '800ms'" \
        --command="BEGIN" --command="CALL evidence.capture_record_now(10, NULL)" --command="COMMIT" \
        >/dev/null 2>"$evidence_log.third"; then
        printf '%s\n' 'the recorder ran inside a transaction its caller opened' >&2; exit 1
    fi
    grep -q 'invalid transaction termination' "$evidence_log.third" \
        || { cat "$evidence_log.third" >&2; exit 1; }
    pg --tuples-only --no-align --command="SET SESSION AUTHORIZATION evidence_adapter_qual" \
        --command="SET statement_timeout = '800ms'" --command="CALL evidence.capture_record_now(10, NULL)" \
        | grep -q '"busy": false' \
        || { printf '%s\n' 'the append service'"'"'s role could not record through the procedure' >&2; exit 1; }
    # 4. One recorder at a time: while another holds the recorder's lock row, a call returns busy and takes nothing.
    pg --command="BEGIN; SELECT 1 FROM evidence.capture_recorder FOR NO KEY UPDATE; SELECT pg_sleep(4); COMMIT;" \
        >/dev/null &
    holder=$!
    await_active 'pg_sleep(4)' 'the recorder lock holder did not start'
    pg --tuples-only --no-align --command="SET statement_timeout = '800ms'; SELECT evidence.capture_record(10)->>'busy'" | grep -qx true \
        || { printf '%s\n' 'a second recorder ran beside the first' >&2; exit 1; }
    wait "$holder"
    # 4b. Advisory lock keys are open to every session, so the recorder uses none: a worker session holding the former
    # recorder and catch-up keys at session level neither holds the recorder off nor blocks it.
    pg --command="SET SESSION AUTHORIZATION broker_worker_qual;
        SELECT pg_advisory_lock(hashtextextended('evidence.capture-record', 0)),
          pg_advisory_lock(hashtextextended('evidence.capture-catchup:capture-site', 0));
        SELECT pg_sleep(6);" >/dev/null &
    holder=$!
    await_active 'pg_sleep(6)' 'the advisory key holder did not start'
    pg --command="$(event c1 17)" >/dev/null
    pg --tuples-only --no-align --command="SET statement_timeout = '800ms';
        SELECT evidence.capture_record(10000)->>'busy' || ':' || (evidence.capture_record(10000)->>'oldest_seconds')" \
        | grep -qx 'false:0' || { printf '%s\n' 'a worker holding advisory keys held the recorder off' >&2; exit 1; }
    wait "$holder"
    # 5. A writer whose snapshot predates the opt-in is still recorded: logging does not read the opt-in, and the writer
    # commits after it.
    pg --command="INSERT INTO enrichedlab_agent.broker_tenants (tenant_id, name) VALUES ('drain-late', 'Drain late');
        INSERT INTO enrichedlab_agent.broker_research_groups (tenant_id, group_key, name, max_active_agents,
          max_active_agents_per_principal, max_queued_tasks, retention_days)
          VALUES ('drain-late', 'drain-late-lab', 'Drain late lab', 4, 2, 16, 30);
        INSERT INTO enrichedlab_agent.broker_principals (tenant_id, principal_id, issuer, subject, email,
          principal_class)
          VALUES ('drain-late', '30000000-0000-4000-8000-0000000000d1', 'https://issuer.qualification.internal',
                  'drain-late', 'drain-late@qualification.internal', 'person');" >/dev/null
    pg --command="BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT count(*) FROM enrichedlab_agent.broker_tenants;
        SELECT pg_sleep(4); INSERT INTO enrichedlab_agent.broker_admin_audit (tenant_id, actor_id, kind, detail)
          VALUES ('drain-late', '30000000-0000-4000-8000-0000000000d1', 'drain_probe', '{}'); COMMIT;" \
        >/dev/null 2>"$evidence_log.first" &
    writer=$!
    await_active 'pg_sleep(4)' 'the early-snapshot writer did not start'
    pg --command="SELECT evidence.capture_enable('drain-late', 'drain-late-lab')" >/dev/null
    writer_status=0; wait "$writer" || writer_status=$?
    [ "$writer_status" -eq 0 ] || { cat "$evidence_log.first" >&2; exit 1; }
    record >/dev/null
    pg --tuples-only --no-align --command="SELECT count(*) FROM evidence.records WHERE tenant_id = 'drain-late'
        AND event_type = 'broker.admin_audit' AND payload::jsonb->>'origin' = 'live'" | grep -qx 1 \
        || { printf '%s\n' 'a writer whose snapshot predates the opt-in was not recorded' >&2; exit 1; }
    pg --tuples-only --no-align --command="SET statement_timeout = '800ms';
        SELECT evidence.capture_prepare_scopes('drain-late', 'drain-late-lab', 1000)->>'remaining'" | grep -qx 0 \
        || { printf '%s\n' 'the late tenant'"'"'s scopes were not prepared' >&2; exit 1; }
    pg --tuples-only --no-align --command="SELECT evidence.capture_catchup('drain-late')->>'appended'" | grep -qx 0 \
        || { printf '%s\n' 'catch-up recorded a row the recorder had recorded' >&2; exit 1; }
    pg --tuples-only --no-align --command="SELECT evidence.capture_missing('drain-late')->>'missing'" | grep -qx 0 \
        || { printf '%s\n' 'the early-snapshot writer left a row missing' >&2; exit 1; }
    rm -f "$evidence_log.first" "$evidence_log.second"
    # 4. A database migrated with an earlier list of gap reasons gains the current list when migration 081 runs again.
    # It also removes what earlier drafts of 081 installed: commit-time triggers, queue tables, fence functions and
    # columns.
    pg --command="ALTER TABLE evidence.capture_gaps DROP CONSTRAINT capture_gaps_reason_check;
        ALTER TABLE evidence.capture_gaps ADD CONSTRAINT capture_gaps_reason_check
          CHECK (reason IN ('scope_unresolved', 'lab_disabled', 'source_time_out_of_range', 'scope_unprepared')) NOT VALID;
        INSERT INTO evidence.capture_gaps (tenant_id, table_name, change, key_sha256, row_sha256, event_type,
            idempotency_key, reason, origin, caller_role)
          VALUES ('capture-site', 'broker_task_events', 'insert', repeat('a', 64), repeat('b', 64), 'broker.task_events',
            'earlier-draft-gap', 'scope_unprepared', 'live', 'broker_worker_qual');
        CREATE FUNCTION evidence.capture_fence(text) RETURNS timestamptz LANGUAGE sql AS 'SELECT now()';
        CREATE FUNCTION evidence.capture_flush() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NULL; END';
        CREATE TABLE evidence.capture_queue (n bigint);
        CREATE TABLE evidence.capture_flushed (n bigint);
        ALTER TABLE evidence.capture_tenants ADD COLUMN fenced_at timestamptz, ADD COLUMN enabled_xact xid8;
        CREATE CONSTRAINT TRIGGER evidence_capture_record AFTER INSERT ON enrichedlab_agent.broker_task_events
          DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION evidence.capture_flush();
        CREATE TRIGGER evidence_capture_statement AFTER INSERT ON enrichedlab_agent.broker_task_events
          FOR EACH STATEMENT EXECUTE FUNCTION evidence.capture_flush();
        CREATE TRIGGER evidence_capture_scope BEFORE INSERT ON enrichedlab_agent.broker_projects
          FOR EACH ROW EXECUTE FUNCTION evidence.capture_flush();" >/dev/null
    pg --file=/workspace/migrations/postgres/081_evidence_live_capture.sql >/dev/null 2>"$evidence_log" \
        || { printf '%s\n' 'migration 081 did not run over an earlier draft' >&2; cat "$evidence_log" >&2; exit 1; }
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --command="SELECT (SELECT count(*) FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
            WHERE c.relnamespace = 'enrichedlab_agent'::regnamespace AND t.tgname LIKE 'evidence\_capture%'
              AND t.tgname NOT IN ('evidence_capture', 'evidence_capture_update'))
          + (SELECT count(*) FROM pg_proc WHERE pronamespace = 'evidence'::regnamespace
              AND proname IN ('capture_fence', 'capture_flush'))
          + (SELECT count(*) FROM pg_class WHERE relnamespace = 'evidence'::regnamespace
              AND relname IN ('capture_queue', 'capture_flushed'))
          + (SELECT count(*) FROM pg_attribute WHERE attrelid = 'evidence.capture_tenants'::regclass
              AND attname IN ('fenced_at', 'enabled_xact') AND NOT attisdropped)" | grep -qx 0 \
        || { printf '%s\n' 'migration 081 left objects of an earlier draft' >&2; exit 1; }
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --command="SELECT pg_get_constraintdef(oid) LIKE '%tenant_disabled%' FROM pg_constraint
          WHERE conname = 'capture_gaps_reason_check'" | grep -qx t \
        || { printf '%s\n' 'migration 081 did not replace the gap reason list' >&2; exit 1; }
    # An unpublished layout is rebuilt only while it holds no receipts: with records under an older layout name, the
    # evidence migration refuses and changes nothing.
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
        --set=ON_ERROR_STOP=on --command="COMMENT ON SCHEMA evidence IS 'enrichedlab evidence layout 0'" >/dev/null
    if docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on \
        --file=/workspace/migrations/postgres/075_evidence_record.sql >"$evidence_log" 2>&1; then
        printf '%s\n' 'the evidence migration rebuilt a layout that holds records' >&2
        exit 1
    fi
    grep -q 'evidence:draft_layout_holds_records' "$evidence_log" || {
        cat "$evidence_log" >&2
        exit 1
    }
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
        --set=ON_ERROR_STOP=on --command="SELECT count(*) >= 27 FROM evidence.receipts" | grep -q t
    # A role that row security would filter cannot count every receipt, so it cannot rebuild either.
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on \
        --command="GRANT USAGE ON SCHEMA evidence TO evidence_rls_probe;
          GRANT SELECT, UPDATE ON evidence.receipts TO evidence_rls_probe" >/dev/null
    if docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on \
        --command="SET ROLE evidence_rls_probe" \
        --file=/workspace/migrations/postgres/075_evidence_record.sql >"$evidence_log" 2>&1; then
        printf '%s\n' 'a row-security-filtered role ran the evidence migration' >&2
        exit 1
    fi
    grep -q 'evidence:draft_layout_unverifiable' "$evidence_log" || {
        cat "$evidence_log" >&2
        exit 1
    }
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on \
        --command="REVOKE SELECT, UPDATE ON evidence.receipts FROM evidence_rls_probe;
          REVOKE USAGE ON SCHEMA evidence FROM evidence_rls_probe" >/dev/null
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
        --set=ON_ERROR_STOP=on --command="COMMENT ON SCHEMA evidence IS 'enrichedlab evidence layout 5'" >/dev/null
    # Verification while an append commits mid-call: the read log is locked so verification stops after reading the
    # head; an append then commits; verification continues and must still find a sound chain. Each step waits for the
    # lock state it needs instead of a fixed delay.
    reads_locks() {
        docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
            --command="SELECT count(*) FROM pg_locks WHERE relation = 'evidence.reads'::regclass AND granted = $1"
    }
    await_reads_lock() {
        tries=0
        until [ "$(reads_locks "$1")" -ge 1 ]; do
            tries=$((tries + 1))
            [ "$tries" -lt 100 ] || { printf '%s\n' "no $1 lock on the read log" >&2; exit 1; }
            sleep 0.1
        done
    }
    stream=$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --set=ON_ERROR_STOP=on --command="SELECT stream_id FROM evidence.streams
          WHERE run_id = '00000000-0000-4000-8000-00000000a0a1'")
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on \
        --command="BEGIN; LOCK TABLE evidence.reads IN SHARE MODE; SELECT pg_sleep(15); COMMIT;" >/dev/null &
    holder=$!
    await_reads_lock true
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --set=ON_ERROR_STOP=on --command="SET SESSION AUTHORIZATION broker_api_qual;
          SELECT evidence.verify_stream('evidence-site', '00000000-0000-4000-8000-000000000001', '$stream', 'audit')
            ->>'valid';" >"$evidence_log" 2>&1 &
    verifier=$!
    await_reads_lock false
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
        --set=ON_ERROR_STOP=on --command="SET SESSION AUTHORIZATION broker_api_qual; SELECT evidence.append(
          'evidence-site','broker/api','lab-a','project','00000000-0000-4000-8000-0000000000a1',
          '00000000-0000-4000-8000-00000000a0a1','step.started','during-verify-0001',clock_timestamp(),p,
          encode(sha256(convert_to(p,'UTF8')),'hex')) FROM (SELECT '{\"during\":true}'::text p) x;" >/dev/null
    wait "$holder"
    wait "$verifier"
    grep -qx 'true' "$evidence_log" || {
        printf '%s\n' 'verification failed while an append committed during it' >&2
        cat "$evidence_log" >&2
        exit 1
    }
    rm -f "$evidence_log"
    # Migration 078's history import: synthetic rows in eight append-guarded tables, imported in batches, then the
    # counts, scopes, times, digest-only model text, chains, a rerun, a changed row and the finish are checked.
    docker exec "$container" psql --username postgres --dbname "$database" --quiet \
        --file=/workspace/tests/fixtures/broker_evidence_import_qualification.sql | grep -q 'evidence import qualification passed'
    # Migration 079's object custody: claims bound to producer and scope, readiness only with both copies verified and
    # retained, binding inside append, model calls only through the inference gateway, missing copies, expiry,
    # immutability and acceptance.
    custody_log=$(mktemp "${TMPDIR:-/tmp}/enrichedlab-evidence-custody.XXXXXX")
    docker exec "$container" psql --username postgres --dbname "$database" --quiet \
        --file=/workspace/tests/fixtures/broker_evidence_custody_qualification.sql >"$custody_log" 2>&1 || true
    grep -q 'evidence custody qualification passed' "$custody_log" || { cat "$custody_log" >&2; exit 1; }
    rm -f "$custody_log"
    # An upgrade from layout 5: a database (migrations 001 to 078 applied by file, in the order
    # migrate applies them) with a receipt producer and a receipt, then 079, then receipts again.
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --command='CREATE DATABASE broker_upgrade' >/dev/null
    for migration in "$repo_root"/migrations/postgres/0[0-7][0-9]_*.sql; do
        name=$(basename "$migration")
        case "$name" in 079_*) continue ;; esac
        docker exec "$container" psql --username postgres --dbname broker_upgrade --quiet --set=ON_ERROR_STOP=on \
            --file="/workspace/migrations/postgres/$name" >/dev/null 2>&1 || {
            printf '%s\n' "the upgrade database could not apply $name" >&2
            exit 1
        }
    done
    docker exec "$container" psql --username postgres --dbname broker_upgrade --quiet --tuples-only \
        --file=/workspace/tests/fixtures/broker_evidence_upgrade_before.sql | grep -q '1' || {
        printf '%s\n' 'the layout 5 receipt was not appended' >&2
        exit 1
    }
    docker exec "$container" psql --username postgres --dbname broker_upgrade --quiet --set=ON_ERROR_STOP=on \
        --file=/workspace/migrations/postgres/079_evidence_custody.sql >/dev/null
    upgrade_log=$(mktemp "${TMPDIR:-/tmp}/enrichedlab-evidence-upgrade.XXXXXX")
    docker exec "$container" psql --username postgres --dbname broker_upgrade --quiet \
        --file=/workspace/tests/fixtures/broker_evidence_upgrade_after.sql >"$upgrade_log" 2>&1 || true
    grep -q 'evidence upgrade qualification passed' "$upgrade_log" || { cat "$upgrade_log" >&2; exit 1; }
    rm -f "$upgrade_log"
    # The same upgrade with every migration run as a non-superuser owner, as a deployment runs them. The producer and
    # grant tables force row security even for their owner, so 079 must reach the receipt producer's tenant through the
    # broker's tenant list; run as the superuser above, row security never applies, which would hide a missing
    # registration. The owner may set the tenant parameter.
    docker exec "$container" psql --username postgres --dbname "$database" --quiet \
        --command='CREATE DATABASE broker_owner_upgrade' >/dev/null
    docker exec "$container" psql --username postgres --dbname broker_owner_upgrade --quiet --set=ON_ERROR_STOP=on \
        --command="DO \$\$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'broker_owner_qual') THEN
            CREATE ROLE broker_owner_qual NOLOGIN; END IF; END \$\$;" \
        --command='GRANT CREATE ON DATABASE broker_owner_upgrade TO broker_owner_qual' \
        --command='GRANT SET ON PARAMETER enrichedlab.tenant_id TO broker_owner_qual' >/dev/null
    for migration in "$repo_root"/migrations/postgres/0[0-7][0-9]_*.sql; do
        name=$(basename "$migration")
        case "$name" in 079_*) continue ;; esac
        docker exec "$container" psql --username postgres --dbname broker_owner_upgrade --quiet --set=ON_ERROR_STOP=on \
            --command='SET ROLE broker_owner_qual' --file="/workspace/migrations/postgres/$name" >/dev/null 2>&1 || {
            printf '%s\n' "the owner upgrade database could not apply $name as its owner" >&2
            exit 1
        }
    done
    docker exec "$container" psql --username postgres --dbname broker_owner_upgrade --quiet --tuples-only \
        --file=/workspace/tests/fixtures/broker_evidence_upgrade_before.sql | grep -q '1' || {
        printf '%s\n' 'the owner upgrade database did not append its layout 5 receipt' >&2
        exit 1
    }
    docker exec "$container" psql --username postgres --dbname broker_owner_upgrade --quiet --set=ON_ERROR_STOP=on \
        --command='SET ROLE broker_owner_qual' --file=/workspace/migrations/postgres/079_evidence_custody.sql >/dev/null
    upgrade_log=$(mktemp "${TMPDIR:-/tmp}/enrichedlab-evidence-upgrade.XXXXXX")
    docker exec "$container" psql --username postgres --dbname broker_owner_upgrade --quiet \
        --file=/workspace/tests/fixtures/broker_evidence_upgrade_after.sql >"$upgrade_log" 2>&1 || true
    grep -q 'evidence upgrade qualification passed' "$upgrade_log" || {
        printf '%s\n' 'run as its owner, 079 did not register the receipt schema' >&2
        cat "$upgrade_log" >&2
        exit 1
    }
    rm -f "$upgrade_log"
    # A tenant whose receipt producer has no schema yet (as a deployment that registered its producer before 079 has) gets the reviewed receipt schema
    # when 079 runs; here a second tenant is made without one before the rerun.
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on --command="
      SELECT set_config('enrichedlab.tenant_id', 'evidence-site-two', false);
      INSERT INTO enrichedlab_agent.broker_tenants (tenant_id, name) VALUES ('evidence-site-two', 'Second site');
      INSERT INTO enrichedlab_agent.broker_research_groups (tenant_id, group_key, name, max_active_agents,
        max_active_agents_per_principal, max_queued_tasks, retention_days) VALUES ('evidence-site-two', 'lab-z', 'Lab Z', 1, 1, 1, 30);
      INSERT INTO evidence.producers (tenant_id, identity, kind) VALUES ('evidence-site-two', 'openshell/receipts-two', 'sandbox');
      INSERT INTO evidence.producer_grants (tenant_id, producer_id, group_key, scope_kind, event_types, actor_role)
        SELECT 'evidence-site-two', producer_id, 'lab-z', 'standalone', ARRAY['sandbox.receipt'], 'sandbox'
        FROM evidence.producers WHERE tenant_id = 'evidence-site-two';" >/dev/null
    # An earlier layout 6 (the first custody source) had narrower event checks under other names; put them back, as a
    # database migrated by that source would hold them (NOT VALID, since rows here already use the newer kinds).
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on --command="
      ALTER TABLE evidence.custody_events DROP CONSTRAINT custody_events_kind_allowed;
      ALTER TABLE evidence.custody_events DROP CONSTRAINT custody_events_object_required;
      ALTER TABLE evidence.custody_events ADD CONSTRAINT custody_events_kind_check CHECK (kind IN ('copy_missing',
        'retention_requested', 'retention_applied', 'hold_placed', 'hold_released', 'reference_released',
        'delete_intent', 'delete_started', 'copy_deleted', 'epoch_observed')) NOT VALID;
      ALTER TABLE evidence.custody_events ADD CONSTRAINT custody_events_check
        CHECK (kind = 'epoch_observed' OR object_id IS NOT NULL) NOT VALID;" >/dev/null
    # Every migrate runs every migration again: a second run keeps layout 6 and the custody rules.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    checks=$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --command="SELECT string_agg(conname || ':' || convalidated, ',' ORDER BY conname) FROM pg_constraint
          WHERE conrelid = 'evidence.custody_events'::regclass AND contype = 'c' AND pg_get_constraintdef(oid) ~ '\\mkind\\M'
            AND (conname <> 'custody_events_kind_allowed' OR pg_get_constraintdef(oid) ~ 'acceptance_completed')
            AND (conname <> 'custody_events_object_required' OR pg_get_constraintdef(oid) ~ 'acceptance_completed');" | tail -1)
    [ "$checks" = "custody_events_kind_allowed:true,custody_events_object_required:true" ] || {
        printf '%s\n' "the rerun left other custody event checks: $checks" >&2
        exit 1
    }
    registered=$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --command="SELECT set_config('enrichedlab.tenant_id', 'evidence-site-two', false) IS NOT NULL,
          (SELECT string_agg(version::text || ':' || schema_sha256, ',') FROM evidence.event_schemas
            WHERE tenant_id = 'evidence-site-two' AND event_type = 'sandbox.receipt');" | tail -1)
    registered=${registered#*|}
    file_digest=$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --no-align \
        --command="SELECT encode(sha256(convert_to(pg_read_file('/workspace/schemas/evidence/sandbox.receipt.v1.json')::jsonb::text, 'UTF8')), 'hex');" 2>/dev/null | tail -1)
    [ "$registered" = "1:$file_digest" ] || {
        printf '%s\n' "the receipt schema was not registered once for a tenant without one: $registered (file $file_digest)" >&2
        exit 1
    }
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only --set=ON_ERROR_STOP=on \
        --command="SELECT obj_description(to_regnamespace('evidence'), 'pg_namespace');" | grep -qx ' enrichedlab evidence layout 6' || {
        printf '%s\n' 'a second migrate changed the evidence layout' >&2
        exit 1
    }
    rerun=$(docker exec "$container" psql --username postgres --dbname "$database" --quiet --tuples-only \
        --command="SET SESSION AUTHORIZATION evidence_adapter_qual; SELECT evidence.append('evidence-site',
          'gateway/inference-qual','lab-a','project','00000000-0000-4000-8000-0000000000a1',
          '00000000-0000-4000-8000-00000000a0a4','model.call','rerun-key-0001','2026-10-24T12:00:00Z',p,
          encode(sha256(convert_to(p,'UTF8')),'hex')) FROM (SELECT '{\"lineage\":{\"version\":1,\"activity\":{\"key\":\"rerun\",\"kind\":\"model_call\"},\"entities\":[{\"key\":\"o\",\"kind\":\"other\",\"sha256\":\"2222222222222222222222222222222222222222222222222222222222222222\",\"disclosure\":\"project\"}],\"generated\":[{\"entity\":\"o\",\"role\":\"output\"}]}}'::text p) x;" 2>&1 || true)
    case "$rerun" in
        *evidence:custody_required*) ;;
        *) printf '%s\n' 'after a second migrate an unclaimed digest was not refused' "$rerun" >&2; exit 1 ;;
    esac
    printf '%s\n' 'Evidence record PostgreSQL check passed (24 concurrent appends serialised; lineage graph; layout guard; history import; custody; upgrade from layout 5; migrate rerun).'
}
if [ "$mode" = evidence ]; then
    run_evidence
    exit 0
fi

# The gRPC evidence append service end to end: the evidence fixtures, then the service over TLS in a test process
# against this database as the adapter role the fixtures bind to the qualification sandbox producer.
if [ "$mode" = evidence-service ]; then
    run_evidence
    # Custody runs against two local object stores holding lab A's bucket (its name derived as the record derives it).
    custody_bucket="ev-$(printf 'evidence-site\nlab-a' | shasum -a 256 | cut -c1-20)"
    custody_env="$temp_root/custody.env"
    sh "$script_dir/custody_stores_local.sh" start 1 "$custody_bucket" >"$custody_env"
    custody_rig=$(sed -n 's/^CUSTODY_RIG=//p' "$custody_env")
    # shellcheck disable=SC1090
    set -a; . "$custody_env"; set +a
    status=0
    EVIDENCE_QUAL_DSN="host=127.0.0.1 port=$port user=evidence_adapter_qual dbname=$database sslmode=disable" \
        EVIDENCE_QUAL_OWNER_DSN="$(cat "$dsn_file")" \
        cargo test --quiet --lib -- --ignored --test-threads=1 \
        broker::evidence_append::tests::service_end_to_end broker::evidence_append::tests::custody_end_to_end || status=$?
    sh "$script_dir/custody_stores_local.sh" stop "$custody_rig"
    [ "$status" -eq 0 ] || exit "$status"
    printf '%s\n' 'Evidence append service check passed (appends; custody through two object stores).'
    exit 0
fi

if [ "$mode" = custodian-role ]; then
    docker exec "$container" psql --username postgres --dbname "$database" \
        --file=/workspace/tests/fixtures/broker_custodian_role_qualification.sql
    printf '%s\n' 'Custodian runtime role PostgreSQL check passed.'
    exit 0
fi

if [ "$mode" = direct-readiness ] || [ "$mode" = all ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_direct_readiness -- --ignored
    if [ "$mode" = direct-readiness ]; then
        printf '%s\n' 'Direct worker readiness PostgreSQL checks passed.'
        exit 0
    fi
fi

# Migration 081: with QUALIFY_EVIDENCE_CAPTURE=1 every tenant is opted in as it is created (the seed's and each
# fixture's), with its first lab as the standalone lab, so every later check runs with its rows recorded live. This
# trigger exists only in the disposable qualification database.
if [ "${QUALIFY_EVIDENCE_CAPTURE:-}" = 1 ]; then
    docker exec "$container" psql --username postgres --dbname "$database" --quiet --set=ON_ERROR_STOP=on \
        --command="CREATE FUNCTION public.qualify_capture_tenant() RETURNS trigger LANGUAGE plpgsql AS \$\$ BEGIN
            IF TG_TABLE_NAME = 'broker_tenants' THEN
              INSERT INTO evidence.capture_tenants (tenant_id, standalone_group) VALUES (NEW.tenant_id, '-')
                ON CONFLICT DO NOTHING;
            ELSE
              UPDATE evidence.capture_tenants SET standalone_group = NEW.group_key
                WHERE tenant_id = NEW.tenant_id AND standalone_group = '-';
              IF FOUND THEN
                PERFORM set_config('enrichedlab.tenant_id', NEW.tenant_id, true);
                PERFORM evidence.capture_prepare(NEW.tenant_id, NEW.group_key, 'standalone', NULL, NULL);
              END IF;
            END IF;
            RETURN NULL; END \$\$;
          CREATE TRIGGER qualify_capture_tenant AFTER INSERT ON enrichedlab_agent.broker_tenants
            FOR EACH ROW EXECUTE FUNCTION public.qualify_capture_tenant();
          CREATE TRIGGER qualify_capture_tenant AFTER INSERT ON enrichedlab_agent.broker_research_groups
            FOR EACH ROW EXECUTE FUNCTION public.qualify_capture_tenant();" >/dev/null
fi
"$broker_bin" seed \
    --file "$repo_root/examples/broker-catalog.yaml" \
    --migration-dsn-file "$dsn_file"
if [ "$mode" = campaigns ]; then
    # Campaign migrations 066 and 067 must be idempotent before their function-only role and behavior are checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    cargo test --locked --lib access_auth_refuses_the_reserved_campaign_issuer
    cargo test --locked --lib service_account_auth_refuses_a_campaign_mapping
    cargo test --locked --test broker_campaigns -- --test-threads=1
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_campaigns -- --ignored --test-threads=1
    printf '%s\n' 'Campaign capability, planner custody, function-only director, isolation, budgets, leases, draining, lifecycle and project ownership PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = artifact-recovery ]; then
    docker exec "$container" psql --username postgres --dbname "$database" \
        --file=/workspace/tests/artifact_recovery.sql
    printf '%s\n' 'Operator artifact recovery PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = identity-enrollment ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_identity_enrollment -- --ignored
    printf '%s\n' 'Concurrent identity enrollment PostgreSQL check passed.'
    exit 0
fi
if [ "$mode" = review ]; then
    # Migration 014 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_research_review -- --ignored
    printf '%s\n' 'Review, collaboration, administration and package PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = generated-code ]; then
    # Migration 015 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_generated_code -- --ignored
    printf '%s\n' 'Generated-code admission, lease-fenced custody and scoped read PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = workspace-reads ]; then
    # Migration 054 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_workspace_reads -- --ignored
    printf '%s\n' 'Workspace reads: last look per person and forward only, analyses paged past 200 in the read item shape, history read forward, refusals and grants PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = project-list ]; then
    # Migration 020 must apply twice without change before the list is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_project_list_summary -- --ignored
    printf '%s\n' 'Project list summary PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = attempt-lineage ]; then
    # Migration 019 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_attempt_lineage -- --ignored
    printf '%s\n' 'Per-attempt lineage, lease fencing, immutability and grant PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = lineage-upstream ]; then
    # Migration 065 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_lineage_upstream -- --ignored
    printf '%s\n' 'Upstream lineage walk, sources, reader withholding and limits PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = result-digests ]; then
    # Migration 021 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_result_digests -- --ignored
    printf '%s\n' 'Result digest lease fencing, binding, immutability, operator backfill, coordinator read and reader authorization PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = plan-ledger ]; then
    # Migration 036 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_plan_ledger -- --ignored
    printf '%s\n' "Plan ledger: each writer's label, the replay matching live rows, unrecorded revisions, grants, immutability and reader PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = plan-pages ]; then
    # Migration 048 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_plan_pages -- --ignored
    printf '%s\n' "Plan pages: finished-only archive under the run's lease and revision, the ledger label, the claim's archive and methods, growth past one working plan, immutability and grants PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = methods-revision ]; then
    # Migration 049 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_methods_revision -- --ignored
    printf '%s\n' "Methods revision: a revise turn commits no analysis, keeps its proposal out of answer readers and returns the run to the queue; the results read gives the request and the count; the third revision in a run is refused PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = specialist-texts ]; then
    # Migration 050 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_specialist_texts -- --ignored
    printf '%s\n' "Specialist texts: a lab member reads the instructions, context and reply a turn names under literature_claims.specialist; a kept text no turn names stays unreadable PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = query-facts ]; then
    # Migration 051 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_query_facts -- --ignored
    printf '%s\n' "Query facts: a revision's facts reach the results read with its request, an empty list without them, and the coordinator refuses a schema without 051 PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = lab-methods ]; then
    # Migration 052 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_lab_methods -- --ignored
    printf '%s\n' "Lab methods: a person in two labs reads each lab's own enabled methods, never another lab's or a disabled one, and the API is not ready without 052 PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = inbox ]; then
    # Migration 037 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_inbox -- --ignored
    printf '%s\n' "What waits for each person: kinds and who may act, marking as seen, changed items, people only, project readers, grants and immutability PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = reference-sql ]; then
    # Migration 038 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_reference_sql -- --ignored
    printf '%s\n' "Guarded queries: source registration, the run's sources, admitted queries only, read-only execution against the named snapshot, package, digest, receipt, failure codes, readers, grants and immutability, and a live lab source's registration rules, its version kept with each receipt and the refusals of an unversioned receipt and an unreadable version, PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = figure-versions ]; then
    # Migration 040 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_figure_versions -- --ignored
    printf '%s\n' "Figure versions: the render catalog, a person's request and its refusals, the worker's route against a stand-in render service, package, receipts, a refused setting, readers, lineage, the run's parallel limit and immutability PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = answer-documents ]; then
    # Migration 072 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_answer_documents -- --ignored
    printf '%s\n' "Answer documents: a person's request and its refusals, the request-only method, the worker's route against a stand-in render service, the filled finding, package, receipts, readers and lineage passed."
    exit 0
fi
if [ "$mode" = template-analyses ]; then
    # Migration 058 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_template_analyses -- --ignored
    printf '%s\n' "Template analyses: readiness by role, the analysis catalog kept apart from the figure catalog, a person's request and its refusals, the method offered only through a request, the lease fence, the worker's route against a stand-in render service, a package with every row at full precision, receipts, refusals in the broker's words, an outdated service, readers, lineage, the project's contents and totals, a figure and an analysis of the results, and immutability PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = lab-datasets ]; then
    # Migration 059 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_lab_datasets -- --ignored
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::datasets::tests::
    printf '%s\n' "Lab datasets: readiness by role, the dataset method offered only through a request, the guard's decision recorded for every outcome, the upload route with its refusals before the guard sees a file, the intake that checks and profiles the guard's grid, a copy that does not match its digest, a preparation a person approves and the prepared table with its receipt, a refused preparation that names its rows, the lease fence, readers, lineage, the project's contents, a result digest without the table's cells, a template analysis of the prepared table, and immutability PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = change-feed ]; then
    # Migration 061 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_change_feed -- --ignored
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::changes::tests::a_change_stream_
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::projects::tests::a_project_starts_from_several_sets
    printf '%s\n' "Change feed: a row for each change a view shows and none for a lease renewal, a repeated look, a sign-in rewrite or a working copy observation; keys under each reader's access rule; access changes for the person and the lab; commit order behind a running transaction; one notification per transaction; behind for a pruned, unknown or overlong position; busy projects; each lab's dataset signal; the readers for the API role only, the stream through a real listener with resume and reset, reads that answer 304 when unchanged, and a project started from several sets with its refusals. PostgreSQL checks passed."
    exit 0
fi

if [ "$mode" = lead-analyses ]; then
    # Migration 060 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_lead_analyses -- --ignored
    printf '%s\n' "Lead analyses: the coordinator ready only with migration 060's catalog read, a run that may list the template-analysis method while figure and dataset methods stay request-only, the method and newest catalog under the run's lease, a person's request checks on the analysis the lead chose, every other shape refused, the request's origin and parent, the worker's run of it, and a lab dataset's tasks holding none of a run's places PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = lab-objects ]; then
    # Migration 083 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_lab_objects -- --ignored
    printf '%s\n' "Lab objects: the coordinator and worker ready only with migrations 083 and 084 and their grants, the run's objects methods read under its lease, every other step shape refused with nothing recorded, the admitted step committed with its request, the request read under the worker's own lease only, one receipt per attempt, a request-only route, and the lab tools list naming the kind."
    exit 0
fi
if [ "$mode" = configured-methods ]; then
    # Migration 022 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_configured_methods -- --ignored
    printf '%s\n' 'Configured-method render check at commit, refusals, readers and lineage class PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = person-review ]; then
    # Migration 023 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_person_review -- --ignored
    printf '%s\n' 'Person review capability, people-only rule, output binding, idempotency, immutability and reader PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = lab-library ]; then
    # Migration 028 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_lab_library -- --ignored
    printf '%s\n' 'Lab library membership, paper, label, removal, restoration, immutability and grant PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = knowledge-person ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::knowledge::tests::a_person_
    printf '%s\n' "Knowledge actions through the workspace: the enrolled person's sign-in forwarded with the workspace's token, the commons scope filter, the change header, refusals passed on, unconfirmed changes reported as unknown outcomes, the collections and a collection mark, an unenrolled person kept back, and a lab's proposed links with a person's acceptance and rejection PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = tool-groups ]; then
    # Migration 080 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_tool_groups -- --ignored
    printf '%s\n' "Tool group versions stored once, lab grants from direct grants and held versions, revocation, reruns, immutability, the API's readiness and the member-only tools read PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = lead-library ]; then
    # Library, reading and review migrations must apply twice before their behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_lead_library -- --ignored
    printf '%s\n' "The coordinator's library and study reads, checked and refused literature review reuse, guarded packages, readiness and member artifacts, delegated knowledge request, part receipts, membership, removal, acceptance, lab scope and grant PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = decision-contexts ]; then
    # Migrations 030 to 032 must apply twice without change before their behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_decision_contexts -- --ignored
    printf '%s\n' "Kept decision texts: lease, address, lab-member reads, inspection refusal, immutability and grant PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = working-copies ]; then
    # Migration 033 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_working_copies -- --ignored
    printf '%s\n' "Working copies: operator records, clearing, project reads, refusals and grant PostgreSQL checks passed."
    exit 0
fi
if [ "$mode" = lab-notes ]; then
    # Migration 027 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_lab_notes -- --ignored
    printf '%s\n' 'Lab note sharing rules, binding, fixed state rule, reader, coordinator order and immutability PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = lineage ]; then
    # Migration 016 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_task_lineage -- --ignored
    printf '%s\n' 'Task origin, lineage, grant and backfill PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = orientation ]; then
    # Migration 074 must apply twice without change before its behavior is checked.
    "$broker_bin" migrate --migration-dsn-file "$dsn_file"
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_orientation -- --ignored --test-threads=1
    printf '%s\n' 'Supervisor fault reads, fixed-reason pauses and orientation keep-once and lab-member reads PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = research-runs ]; then
    docker exec "$container" psql --username postgres --dbname "$database" --set=ON_ERROR_STOP=on \
        --command='CREATE ROLE broker_coordinator_qual LOGIN;'
    docker exec "$container" psql --username postgres --dbname "$database" --set=coordinator_role=broker_coordinator_qual --set=api_role=broker_api_qual --set=worker_role=broker_worker_qual \
        --file=/workspace/migrations/postgres/grant_broker_coordinator.psql
    # Both checks claim runs in one tenant, so they run one at a time.
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_research_runs -- --ignored --test-threads=1
    printf '%s\n' 'Research run lease, fair claim and atomic submission PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = projects ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --test broker_projects -- --ignored
    printf '%s\n' 'Project authorization and continuity PostgreSQL checks passed.'
    exit 0
fi
if [ "$mode" = table-reads ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::table_reads::tests::tables_page_and_summarize_
    printf '%s\n' 'Table reads: pages and summaries from verified package members and bare tables, refusals, byte ranges of members and stored files, changed bytes refused, and nothing for a person who may not read the task.'
    exit 0
fi
if [ "$mode" = figure-kinds ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::result_kinds::tests::a_figure_version_declares_
    printf '%s\n' 'Figure kinds: a completed figure version names the drawings of its own package as R figures, bound to path and digest; another digest, another package, a refused attempt and an unlisted file keep their checked kinds; the cached listing is unchanged; nothing for a person who may not read the version.'
    exit 0
fi
if [ "$mode" = figure-previews ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::figures::tests::a_person_previews_
    printf '%s\n' 'Figure previews: a drawing through the HTTP route checked against its digest, each kind of service error in the broker'"'"'s words and never the service'"'"'s, the size limit, refusals before drawing, catalog limits written any way, the per-person limits, a preview no worker draws, abandoned previews, access lost before a claim, after a drawing and during the wait, and the manuscript mark.'
    exit 0
fi
if [ "$mode" = live-output ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::live_output::tests::the_lease_holder_records_output_
    printf '%s\n' 'Live output: pieces recorded under the lease, a replayed batch stored once, a lost reply sent again with newer pieces and checked field by field, gaps, other leases, an expired lease, unsanitized text and direction controls refused, what the sanitizer and piece builder make accepted, a live reader, four streams per person, a closure checked against the stored text, immutability, a resumed reader ending with a checked close, a task that ended without a closure, and nothing for a person who may not read the task.'
    exit 0
fi
if [ "$mode" = result-kinds ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::result_kinds::tests::the_worker_records_kinds_
    printf '%s\n' 'Result kinds: the registry and its ETag, one recorder pass over stored artifacts, a custody fault left unrecorded, receipts that hash to their digest and name the media type, refused conflicts and edits, a media type change checked again, a receipt not in canonical form refused, member kinds in a listing, and nothing for a person who may not read the task.'
    exit 0
fi
if [ "$mode" = project-stream ]; then
    BROKER_QUALIFICATION_MIGRATION_DSN_FILE="$dsn_file" \
        cargo test --locked --lib -- --ignored api::projects::tests::a_project_stream_
    printf '%s\n' 'Project event stream: the state on connect, a change after a saved version, no research content in the payload, and a refusal for a person who may not read the project.'
    exit 0
fi
docker exec "$container" psql --username postgres --dbname "$database" \
    --file=/workspace/tests/fixtures/broker_runtime_events.sql
if [ "$mode" = runtime-events ]; then
    printf '%s\n' 'Runtime event PostgreSQL checks passed.'
    exit 0
fi
docker exec "$container" psql --username postgres --dbname "$database" \
    --file=/workspace/tests/fixtures/broker_database_qualification.sql

"$broker_bin" seed \
    --file "$repo_root/examples/broker-catalog.yaml" \
    --migration-dsn-file "$dsn_file"
docker exec "$container" psql --username postgres --dbname "$database" \
    --file=/workspace/tests/fixtures/broker_revocation_qualification.sql

printf '%s\n' 'Remote broker PostgreSQL qualification passed.'
