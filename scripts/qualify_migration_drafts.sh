#!/bin/sh
# A migration that no published release carries may still be edited, and any committed draft of it may have run on a
# qualification or development database. Each such draft, upgraded by the current migrations, must reach the schema a
# fresh database reaches: the same relations, columns, constraints, indexes, functions (with their owner, arguments and
# defaults, properties, source and grants), triggers, views, types, privileges and row policies. Column order and the
# names PostgreSQL gives unnamed CHECK constraints are not compared. Both broker schemas, enrichedlab_agent and evidence,
# are compared. Each database gets the runtime grant files after its
# migrations, as a deployment does. The current migrations applied again must change nothing, before the grants are
# applied again: a migration that dropped and recreated a granted function on every run would take its grants away.
#
# ENRICHEDLAB_BROKER_FIRST_UNPUBLISHED names the first migration no published release carries (default 061: the
# newest published release carries 054 to 060). Raise it
# when a release that carries more migrations is published.
set -eu

first_unpublished=${ENRICHEDLAB_BROKER_FIRST_UNPUBLISHED:-061}
case "$first_unpublished" in
    [0-9][0-9][0-9]) ;;
    *) printf '%s\n' 'ENRICHEDLAB_BROKER_FIRST_UNPUBLISHED must be three digits' >&2; exit 2 ;;
esac

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)
postgres_image=${ENRICHEDLAB_BROKER_POSTGRES_IMAGE:-postgres@sha256:1c59e2c3c818eaa0f0628f695b36e7c9e362d6b219b36a54a32df645cbd7e1af}
container="enrichedlab-broker-pg-drafts-$$"
temp_root=$(mktemp -d "${TMPDIR:-/tmp}/enrichedlab-broker-drafts.XXXXXX")
started=false

cleanup() {
    if [ "$started" = true ]; then
        docker rm --force "$container" >/dev/null 2>&1 || true
    fi
    rm -rf "$temp_root"
}
trap cleanup EXIT HUP INT TERM

for command_name in docker git; do
    command -v "$command_name" >/dev/null 2>&1 || {
        printf '%s\n' "$command_name is required" >&2
        exit 1
    }
done

docker run --detach --name "$container" --tmpfs /var/lib/postgresql \
    --env POSTGRES_HOST_AUTH_METHOD=trust "$postgres_image" >/dev/null
started=true
ready=false
attempt=0
while [ "$attempt" -lt 60 ]; do
    if docker exec "$container" pg_isready --host 127.0.0.1 --username postgres >/dev/null 2>&1; then
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

cat >"$temp_root/fingerprint.sql" <<'SQL'
SELECT line FROM (
  SELECT format('relation %s.%s kind=%s', n.nspname, c.relname, c.relkind) AS line
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence') AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
  UNION ALL
  SELECT format('column %s.%s %s notnull=%s default=%s generated=%s identity=%s', c.relname, a.attname,
    format_type(a.atttypid, a.atttypmod), a.attnotnull, pg_get_expr(d.adbin, d.adrelid), a.attgenerated, a.attidentity)
  FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace
  LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence') AND a.attnum > 0 AND NOT a.attisdropped AND c.relkind IN ('r', 'p', 'v', 'm')
  UNION ALL
  SELECT format('constraint %s %s %s %s', c.relname, co.contype,
    CASE WHEN co.contype = 'c' AND co.conname ~ '_check[0-9]*$' THEN '(unnamed check)' ELSE co.conname END,
    pg_get_constraintdef(co.oid))
  FROM pg_constraint co JOIN pg_class c ON c.oid = co.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence')
  UNION ALL
  SELECT format('index %s', pg_get_indexdef(i.indexrelid))
  FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence')
  UNION ALL
  SELECT format('function %s.%s(%s) returns %s owner=%s kind=%s security_definer=%s volatility=%s strict=%s leakproof=%s '
    'parallel=%s config=%s source_md5=%s acl=%s', n.nspname, p.proname, pg_get_function_arguments(p.oid),
    pg_get_function_result(p.oid), pg_get_userbyid(p.proowner), p.prokind, p.prosecdef, p.provolatile, p.proisstrict,
    p.proleakproof, p.proparallel, p.proconfig, md5(p.prosrc), p.proacl)
  FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence')
  UNION ALL
  SELECT format('trigger %s', pg_get_triggerdef(t.oid))
  FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence') AND NOT t.tgisinternal
  UNION ALL
  SELECT format('view %s definition_md5=%s', c.relname, md5(pg_get_viewdef(c.oid)))
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence') AND c.relkind IN ('v', 'm')
  UNION ALL
  SELECT format('privileges %s acl=%s row_security=%s forced=%s', c.relname, c.relacl, c.relrowsecurity,
    c.relforcerowsecurity)
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence') AND c.relkind IN ('r', 'p', 'v', 'm', 'S')
  UNION ALL
  SELECT format('policy %s %s cmd=%s permissive=%s roles=%s using=%s check=%s', c.relname, p.polname, p.polcmd,
    p.polpermissive, p.polroles::regrole[], pg_get_expr(p.polqual, p.polrelid), pg_get_expr(p.polwithcheck, p.polrelid))
  FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence')
  UNION ALL
  SELECT format('type %s kind=%s', t.typname, t.typtype)
  FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence') AND t.typtype IN ('e', 'd', 'c')
    AND NOT EXISTS (SELECT 1 FROM pg_class c WHERE c.reltype = t.oid AND c.relkind <> 'c')
  UNION ALL
  SELECT format('enum %s %s %s', t.typname, e.enumsortorder, e.enumlabel)
  FROM pg_enum e JOIN pg_type t ON t.oid = e.enumtypid JOIN pg_namespace n ON n.oid = t.typnamespace
  WHERE n.nspname IN ('enrichedlab_agent', 'evidence')
) s ORDER BY line;
SQL
docker exec "$container" mkdir -p /drafts
docker cp "$temp_root/fingerprint.sql" "$container:/drafts/fingerprint.sql" >/dev/null

# Copies a migration directory into the container as /drafts/<name> with a psql script that applies its numbered
# migrations in order, as the broker's migrate does.
stage() {
    name=$1
    source_dir=$2
    {
        printf '%s\n' '\set ON_ERROR_STOP on'
        for file in "$source_dir"/[0-9][0-9][0-9]_*.sql; do
            printf '\\i /drafts/%s/%s\n' "$name" "$(basename "$file")"
        done
    } >"$source_dir/apply.psql"
    docker cp "$source_dir" "$container:/drafts/$name" >/dev/null
}

sql() {
    docker exec -i --env PGOPTIONS='-c client_min_messages=warning' "$container" \
        psql -X -q -v ON_ERROR_STOP=1 --username postgres "$@"
}

fresh_database() {
    sql --dbname postgres --command "CREATE DATABASE $1" >/dev/null
}

sql --dbname postgres --command \
    'CREATE ROLE drafts_api; CREATE ROLE drafts_worker; CREATE ROLE drafts_custodian; CREATE ROLE drafts_coordinator;' \
    >/dev/null
for grant_file in grant_broker_runtime.psql grant_broker_coordinator.psql; do
    docker cp "$repo_root/migrations/postgres/$grant_file" "$container:/drafts/$grant_file" >/dev/null
done

apply() {
    sql --dbname "$1" --file "/drafts/$2/apply.psql" >"$temp_root/$1.$2.log" 2>&1 || {
        printf 'FAIL %s: the %s migrations do not apply\n' "$1" "$2"
        grep -A3 'ERROR' "$temp_root/$1.$2.log" | sed -n '1,12p'
        return 1
    }
}

# The current runtime grant files with test roles, as qualify_remote_broker.sh applies them.
grants() {
    { sql --dbname "$1" --set=api_role=drafts_api --set=worker_role=drafts_worker \
        --set=custodian_role=drafts_custodian --file /drafts/grant_broker_runtime.psql \
      && sql --dbname "$1" --set=coordinator_role=drafts_coordinator --set=api_role=drafts_api \
        --set=worker_role=drafts_worker --file /drafts/grant_broker_coordinator.psql; } >"$temp_root/$1.grants.log" 2>&1 || {
        printf 'FAIL %s: the runtime grant files do not apply\n' "$1"
        grep -A3 -E 'ERROR|warning' "$temp_root/$1.grants.log" | sed -n '1,12p'
        return 1
    }
}

fingerprint() {
    sql --dbname "$1" --tuples-only --no-align --file /drafts/fingerprint.sql >"$temp_root/${2:-$1}.fingerprint"
}

mkdir -p "$temp_root/current"
cp "$repo_root"/migrations/postgres/[0-9][0-9][0-9]_*.sql "$temp_root/current/"
stage current "$temp_root/current"

fresh_database fresh
apply fresh current
grants fresh
fingerprint fresh
[ -s "$temp_root/fresh.fingerprint" ] || {
    printf '%s\n' 'The fresh schema fingerprint is empty' >&2
    exit 1
}

failures=0
fresh_database rerun
apply rerun current
grants rerun
fingerprint rerun rerun_granted
apply rerun current
fingerprint rerun
if cmp -s "$temp_root/rerun_granted.fingerprint" "$temp_root/rerun.fingerprint"; then
    printf '%s\n' 'PASS the current migrations applied again keep every grant'
else
    printf '%s\n' 'FAIL the current migrations applied again change the schema or its grants'
    diff -u "$temp_root/rerun_granted.fingerprint" "$temp_root/rerun.fingerprint" | sed -n '3,40p'
    failures=$((failures + 1))
fi
if ! cmp -s "$temp_root/fresh.fingerprint" "$temp_root/rerun.fingerprint"; then
    printf '%s\n' 'FAIL the current migrations applied twice differ from a fresh database'
    diff -u "$temp_root/fresh.fingerprint" "$temp_root/rerun.fingerprint" | sed -n '3,40p'
    failures=$((failures + 1))
fi

drafts=0
for commit in $(git -C "$repo_root" log --reverse --format=%H -- migrations/postgres); do
    touched=false
    for number in $(git -C "$repo_root" diff-tree --root --no-commit-id --name-only -r "$commit" -- migrations/postgres \
        | sed -n 's#^migrations/postgres/\([0-9][0-9][0-9]\)_.*\.sql$#\1#p'); do
        if [ "$number" -ge "$first_unpublished" ]; then
            touched=true
        fi
    done
    [ "$touched" = true ] || continue
    drafts=$((drafts + 1))
    short=$(git -C "$repo_root" rev-parse --short=7 "$commit")
    mkdir -p "$temp_root/archive_$short" "$temp_root/d$short"
    git -C "$repo_root" archive "$commit" migrations/postgres | tar -x -C "$temp_root/archive_$short"
    cp "$temp_root/archive_$short"/migrations/postgres/[0-9][0-9][0-9]_*.sql "$temp_root/d$short/"
    stage "d$short" "$temp_root/d$short"
    database="upgrade_$short"
    fresh_database "$database"
    if ! apply "$database" "d$short" || ! apply "$database" current || ! grants "$database"; then
        failures=$((failures + 1))
        continue
    fi
    fingerprint "$database"
    if cmp -s "$temp_root/fresh.fingerprint" "$temp_root/$database.fingerprint"; then
        printf 'PASS draft %s upgraded\n' "$short"
    else
        printf 'FAIL draft %s upgraded: its schema differs from a fresh one\n' "$short"
        diff -u "$temp_root/fresh.fingerprint" "$temp_root/$database.fingerprint" | sed -n '3,40p'
        failures=$((failures + 1))
    fi
done

[ "$drafts" -gt 0 ] || {
    printf 'No commit touches a migration from %s on; nothing was compared\n' "$first_unpublished" >&2
    exit 1
}
printf '%s drafts compared, %s failures\n' "$drafts" "$failures"
[ "$failures" -eq 0 ]
