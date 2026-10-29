#!/bin/sh
set -eu

# Run the real reader as the runtime UID against two views of one synthetic
# volume. Only the fixture writer gets the writable view; deployment does not.
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
staging_dir=$(mktemp -d /tmp/projected-token-XXXXXX)
fixture_name=projected-token-$(basename "$staging_dir" | tr '[:upper:]' '[:lower:]')
test_image=$fixture_name:contract
cleanup() {
    docker volume rm "$fixture_name" >/dev/null 2>&1 || true
    docker image rm "$test_image" >/dev/null 2>&1 || true
    rm -rf "$staging_dir"
}
trap cleanup EXIT HUP INT TERM

cat "$repo_root/images/broker.Containerfile" > "$staging_dir/auth.Containerfile"
cat >> "$staging_dir/auth.Containerfile" <<'DOCKERFILE'

FROM build AS projected-token-tests
COPY tests ./tests
RUN cargo test --locked --release --lib --no-run \
    && for test_binary in /source/target/release/deps/enrichedlab_agent_sandbox-*; do \
         if [ -f "$test_binary" ] && [ -x "$test_binary" ]; then \
           cp "$test_binary" /usr/local/bin/projected-token-tests; \
         fi; \
       done \
    && test -x /usr/local/bin/projected-token-tests
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/projected-token-tests"]
DOCKERFILE
cat "$repo_root/.dockerignore" > "$staging_dir/auth.Containerfile.dockerignore"
printf '\n!tests/\n!tests/**\n' >> "$staging_dir/auth.Containerfile.dockerignore"
docker buildx build --platform linux/amd64 --target projected-token-tests --load \
    --file "$staging_dir/auth.Containerfile" --tag "$test_image" "$repo_root"
docker volume create "$fixture_name" >/dev/null
docker run --rm --platform linux/amd64 --network none --user 0:0 \
    --read-only --cap-drop ALL --security-opt no-new-privileges \
    --mount "type=volume,src=$fixture_name,dst=/fixture" \
    --entrypoint /bin/sh "$test_image" -c 'chmod 3777 /fixture'
docker run --rm --platform linux/amd64 --network none --user 65532:65532 \
    --read-only --cap-drop ALL --security-opt no-new-privileges \
    --tmpfs /tmp:rw,nosuid,nodev,size=64m \
    --mount "type=volume,src=$fixture_name,dst=/fixture_writer" \
    --mount "type=volume,src=$fixture_name,dst=/fixture_reader,readonly" \
    --env PROJECTED_TOKEN_FIXTURE_WRITER=/fixture_writer \
    --env PROJECTED_TOKEN_FIXTURE_READER=/fixture_reader \
    "$test_image" broker::auth::tests:: --include-ignored --nocapture
