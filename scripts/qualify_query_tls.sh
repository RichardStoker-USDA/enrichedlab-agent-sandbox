#!/bin/sh
# A lab reader's connection trusts its own certificate authority alone and never goes on without TLS.
# Starts two disposable PostgreSQL servers in one network namespace: one with TLS whose certificate a throwaway lab
# authority signed for `localhost`, one in plain text only. Then runs the reader connection tests in the broker
# image's toolchain stage, so the connection uses OpenSSL as the worker's does: the loader and TLS-mode unit tests, the
# server tests with the process's default trust, and again with the process trusting the lab authority
# (SSL_CERT_FILE), where a connection with another authority must still refuse the server.
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)
postgres_image=${ENRICHEDLAB_BROKER_POSTGRES_IMAGE:-postgres@sha256:1c59e2c3c818eaa0f0628f695b36e7c9e362d6b219b36a54a32df645cbd7e1af}
toolchain_image=enrichedlab-broker-toolchain:qualify-query-tls
# Named volumes keep the crate downloads and the test build between runs.
cargo_volume=enrichedlab-broker-qualify-cargo
target_volume=enrichedlab-broker-qualify-target
tls_container="enrichedlab-broker-pg-tls-$$"
plain_container="enrichedlab-broker-pg-plain-$$"
temp_root=$(mktemp -d "${TMPDIR:-/tmp}/enrichedlab-broker-tls.XXXXXX")
started=""

cleanup() {
    for container in $started; do
        docker rm --force --volumes "$container" >/dev/null 2>&1 || true
    done
    rm -rf "$temp_root"
}
trap cleanup EXIT HUP INT TERM

for command_name in docker openssl; do
    command -v "$command_name" >/dev/null 2>&1 || {
        printf '%s\n' "$command_name is required" >&2
        exit 1
    }
done

umask 077
cd "$temp_root"
for authority in lab other; do
    openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=qualification $authority authority" \
        -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
        -keyout "$authority-ca.key" -out "$authority-ca.crt" >/dev/null 2>&1
done
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout server.key -out server.csr >/dev/null 2>&1
printf 'subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\nkeyUsage=critical,digitalSignature,keyEncipherment\nbasicConstraints=CA:FALSE\n' >server.ext
openssl x509 -req -in server.csr -CA lab-ca.crt -CAkey lab-ca.key -CAcreateserial -days 1 -extfile server.ext \
    -out server.crt >/dev/null 2>&1
chmod 0644 server.crt server.key

docker build --quiet --target toolchain --tag "$toolchain_image" --file "$repo_root/images/broker.Containerfile" \
    "$repo_root" >/dev/null

docker run --detach --name "$tls_container" \
    --mount "type=bind,src=$temp_root,dst=/certs,readonly" \
    --env POSTGRES_HOST_AUTH_METHOD=trust \
    --entrypoint sh "$postgres_image" -c \
    'install -o postgres -m 0600 /certs/server.key /tmp/server.key && install -o postgres -m 0644 /certs/server.crt /tmp/server.crt && exec docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tmp/server.crt -c ssl_key_file=/tmp/server.key' \
    >/dev/null
started="$tls_container"
docker run --detach --name "$plain_container" --network "container:$tls_container" \
    --env POSTGRES_HOST_AUTH_METHOD=trust "$postgres_image" postgres -c port=5433 -c ssl=off >/dev/null
started="$plain_container $started"

wait_ready() {
    attempt=0
    while [ "$attempt" -lt 60 ]; do
        if docker exec "$1" pg_isready --host 127.0.0.1 --port "$2" --username postgres >/dev/null 2>&1; then
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 1
    done
    printf '%s\n' "PostgreSQL on port $2 did not become ready within 60 seconds" >&2
    exit 1
}
wait_ready "$tls_container" 5432
wait_ready "$plain_container" 5433

# The test container joins the servers' network namespace: both answer on its own loopback.
run_tests() {
    docker run --rm --network "container:$tls_container" --workdir /source \
        --mount "type=bind,src=$repo_root,dst=/source,readonly" \
        --mount "type=bind,src=$temp_root,dst=/certs,readonly" \
        --mount "type=volume,src=$cargo_volume,dst=/usr/local/cargo/registry" \
        --mount "type=volume,src=$target_volume,dst=/target" \
        --env CARGO_TARGET_DIR=/target --env BROKER_TLS_PORT=5432 --env BROKER_PLAIN_PORT=5433 \
        --env BROKER_TLS_CA=/certs/lab-ca.crt --env BROKER_TLS_OTHER_CA=/certs/other-ca.crt \
        "$@"
}
# Each run must pass exactly the tests it names, so a renamed test cannot pass by matching nothing.
expect_passed() {
    count=$1
    shift
    if ! run_tests "$@" >"$temp_root/tests.log" 2>&1 || ! grep -q "^test result: ok. $count passed;" "$temp_root/tests.log"; then
        cat "$temp_root/tests.log" >&2
        printf '%s\n' "expected exactly $count passing tests" >&2
        exit 1
    fi
}
expect_passed 4 "$toolchain_image" cargo test --locked --lib -- --include-ignored \
    a_readers_certificate_authority_sits_beside_its_connection_and_holds_certificates_only \
    a_lab_connection_requires_tls_and_others_keep_their_mode \
    a_lab_connection_trusts_its_own_authority_alone \
    a_lab_connection_refuses_a_server_without_tls
expect_passed 1 --env SSL_CERT_FILE=/certs/lab-ca.crt "$toolchain_image" cargo test --locked --lib -- --ignored \
    a_lab_connection_never_falls_back_to_the_process_trust
printf '%s\n' 'A lab reader connection trusts its own certificate authority alone and refuses plain text.'
