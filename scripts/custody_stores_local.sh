#!/bin/sh
# Starts two local S3 stores for evidence custody tests: a primary and a replica RustFS, each serving TLS from a
# throwaway CA, each with one bucket that is versioned, object-locked with a governance default, and replicated from
# the primary to the replica as a site's evidence buckets are. Prints shell assignments for the tests on stdout and
# keeps everything in one work directory; `custody_stores_local.sh stop <work dir>` removes the containers, network
# and directory.
#
# Usage: custody_stores_local.sh start [retention days, default 1] [bucket, default ev-custody-local]
#        custody_stores_local.sh stop <work dir>
set -eu

rustfs_image=${ENRICHEDLAB_RUSTFS_IMAGE:-rustfs/rustfs@sha256:9ca9559533b537d10482fe923288b676ad08f92a7069d9a17327fbae3cab688b}
rc_image=${ENRICHEDLAB_RC_IMAGE:-rustfs/rc@sha256:ab024bfebee49a750ce886b4c70963ccd9ddaa03f491704a90710641d7a26699}
region=local

stop() {
    work=$1
    tag=$(cat "$work/tag" 2>/dev/null || true)
    [ -n "$tag" ] || { printf '%s\n' "no rig in $work" >&2; exit 2; }
    docker rm --force "$tag-primary" "$tag-replica" >/dev/null 2>&1 || true
    docker network rm "$tag" >/dev/null 2>&1 || true
    rm -rf "$work"
}

case "${1:-}" in
    stop) stop "${2:?work dir}"; exit 0 ;;
    start) days=${2:-1}; bucket=${3:-ev-custody-local} ;;
    *) printf '%s\n' 'Usage: custody_stores_local.sh start [days] | stop <work dir>' >&2; exit 2 ;;
esac
case "$days" in ''|*[!0-9]*) printf '%s\n' 'days must be a whole number' >&2; exit 2 ;; esac
case "$bucket" in ''|*[!a-z0-9-]*) printf '%s\n' 'the bucket is a plain S3 bucket name' >&2; exit 2 ;; esac

for command_name in docker openssl; do
    command -v "$command_name" >/dev/null 2>&1 || { printf '%s\n' "$command_name is required" >&2; exit 1; }
done

work=$(mktemp -d "${TMPDIR:-/tmp}/enrichedlab-custody-stores.XXXXXX")
tag="enrichedlab-custody-$$"
printf '%s\n' "$tag" > "$work/tag"
trap 'status=$?; [ "$status" -eq 0 ] || stop "$work"' EXIT
umask 077

# A throwaway CA and one server certificate per store, named as the containers are reached.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 -subj "/CN=custody test CA" \
    -keyout "$work/ca.key" -out "$work/ca.pem" >/dev/null 2>&1
for role in primary replica; do
    mkdir -p "$work/$role/tls" "$work/$role/cred"
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=$tag-$role" \
        -keyout "$work/$role/tls/rustfs_key.pem" -out "$work/$role/csr.pem" >/dev/null 2>&1
    printf 'subjectAltName=DNS:%s,DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\nkeyUsage=digitalSignature\nbasicConstraints=CA:FALSE\n' "$tag-$role" > "$work/$role/ext.cnf"
    openssl x509 -req -in "$work/$role/csr.pem" -CA "$work/ca.pem" -CAkey "$work/ca.key" -CAcreateserial -days 2 \
        -extfile "$work/$role/ext.cnf" -out "$work/$role/tls/rustfs_cert.pem" >/dev/null 2>&1
    openssl rand -hex 10 > "$work/$role/cred/access_key"
    openssl rand -hex 20 > "$work/$role/cred/secret_key"
done
chmod -R a+rX "$work"

docker network create "$tag" >/dev/null
for role in primary replica; do
    docker run --detach --name "$tag-$role" --network "$tag" \
        --mount "type=bind,src=$work/$role/tls,dst=/tls,readonly" \
        --mount "type=bind,src=$work/$role/cred,dst=/secrets,readonly" \
        --publish 127.0.0.1::9000 \
        --env RUSTFS_VOLUMES=/data --env RUSTFS_ADDRESS=:9000 --env RUSTFS_CONSOLE_ENABLE=false \
        --env RUSTFS_TLS_PATH=/tls --env RUSTFS_REGION="$region" \
        --env RUSTFS_ACCESS_KEY_FILE=/secrets/access_key --env RUSTFS_SECRET_KEY_FILE=/secrets/secret_key \
        "$rustfs_image" >/dev/null
done

port() {
    docker port "$tag-$1" 9000/tcp | awk -F: 'NF { value=$NF } END { print value }'
}

# rc in a throwaway container on the rig's network, with both stores' root identities.
rc() {
    docker run --rm --network "$tag" --mount "type=bind,src=$work,dst=/w,readonly" --entrypoint sh "$rc_image" -c '
        set -e
        rc alias set primary "https://'"$tag"'-primary:9000" "$(cat /w/primary/cred/access_key)" \
          "$(cat /w/primary/cred/secret_key)" --region '"$region"' --bucket-lookup path --ca-bundle /w/ca.pem -q >/dev/null
        rc alias set replica "https://'"$tag"'-replica:9000" "$(cat /w/replica/cred/access_key)" \
          "$(cat /w/replica/cred/secret_key)" --region '"$region"' --bucket-lookup path --ca-bundle /w/ca.pem -q >/dev/null
        '"$1"
}

ready=false
attempt=0
while [ "$attempt" -lt 60 ]; do
    if rc 'rc bucket list primary >/dev/null 2>&1 && rc bucket list replica >/dev/null 2>&1' >/dev/null 2>&1; then
        ready=true
        break
    fi
    attempt=$((attempt + 1))
    sleep 1
done
[ "$ready" = true ] || { printf '%s\n' 'the stores did not answer within 60 seconds' >&2; exit 1; }

rc "for store in replica primary; do
      rc bucket create \"\$store/$bucket\" --with-versioning --with-lock --ignore-existing -q
      rc bucket lock set \"\$store/$bucket\" --mode governance --days $days -q
    done
    rc bucket replication add \"primary/$bucket\" --remote-bucket \"replica/$bucket\" \
      --replicate delete-marker,existing-objects --priority 1 --ca-cert /w/ca.pem -q" >/dev/null

trap - EXIT
printf 'CUSTODY_RIG=%s\n' "$work"
printf 'CUSTODY_CA=%s\n' "$work/ca.pem"
printf 'CUSTODY_BUCKET=%s\n' "$bucket"
printf 'CUSTODY_REGION=%s\n' "$region"
for role in primary replica; do
    upper=$(printf '%s' "$role" | tr '[:lower:]' '[:upper:]')
    printf 'CUSTODY_%s_URL=https://127.0.0.1:%s\n' "$upper" "$(port "$role")"
    printf 'CUSTODY_%s_ACCESS_KEY_FILE=%s\n' "$upper" "$work/$role/cred/access_key"
    printf 'CUSTODY_%s_SECRET_KEY_FILE=%s\n' "$upper" "$work/$role/cred/secret_key"
done
