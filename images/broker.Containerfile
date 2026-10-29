# Keep the Rust toolchain independent from the runtime and staging utilities.
# Runtime index digests and upstream signatures were checked on 2026-10-15 (distroless: Debian 13.7, keyless
# signatures verified for keyless@distroless.iam.gserviceaccount.com); the Rust and Node digests on 2026-09-10.
ARG RUST_IMAGE=rust:1.98.1-bookworm@sha256:9a73a5088750b4c95158ab26629c854c3d6fc4b173cb7bc8079ad252d8ed7bfa
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2
ARG UTILITY_IMAGE=gcr.io/distroless/cc-debian13:debug-nonroot@sha256:984d31d4bd6e71bb9469353d93e73c68f7b3c17142b2aec45f2a960489095b22
ARG NODE_IMAGE=node:24-bookworm-slim@sha256:0e0ff40c39bc087845bfb27465a0df4ea419520094bc35842ff83dd8cbe6f9b6

FROM ${NODE_IMAGE} AS workspace
WORKDIR /workspace
COPY clients/workspace/package.json clients/workspace/package-lock.json ./
RUN npm ci --ignore-scripts
COPY clients/workspace/ ./
RUN npm run build

FROM ${UTILITY_IMAGE} AS utilities

FROM ${RUST_IMAGE} AS toolchain

# The query guard's parser (pg_query) generates its bindings with bindgen, which loads libclang. Its build script
# also regenerates its protobuf bindings whenever starting `protoc` does not fail, and under an emulated amd64 builder
# starting a missing program reports exit 127 instead of failing, so the builder carries a real protoc. Both come from
# the Debian snapshot the Rust image names in its own sources, so the builder's packages do not move between builds.
# Plain HTTP is enough: apt checks the signed InRelease against the image's archive keyring.
ARG DEBIAN_SNAPSHOT=20260824T000000Z
RUN printf '%s\n' \
      'Types: deb' \
      "URIs: http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}" \
      'Suites: bookworm bookworm-updates' \
      'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' \
      'Check-Valid-Until: no' \
      '' \
      'Types: deb' \
      "URIs: http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}" \
      'Suites: bookworm-security' \
      'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' \
      'Check-Valid-Until: no' \
      >/etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends libclang-dev protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

# scripts/qualify_query_tls.sh runs the reader connection tests in this stage, with OpenSSL as the worker.
FROM toolchain AS build
WORKDIR /source
# The release build names the source revision it builds; the answer-document writer compiles it in and records it in
# each document's receipt (writer.<n>+<revision>). Left empty, a document names the writer's version alone.
ARG ENRICHEDLAB_SOURCE_REVISION=

COPY Cargo.toml Cargo.lock build.rs ./
COPY kinds ./kinds
# build.rs generates the evidence gRPC service from the contracts descriptor set.
COPY proto/enrichedlab_evidence_v1.binpb ./proto/enrichedlab_evidence_v1.binpb
COPY examples ./examples
COPY migrations ./migrations
COPY policies ./policies
COPY profiles ./profiles
COPY runtime ./runtime
COPY schemas ./schemas
COPY src ./src
COPY web ./web

RUN cargo build --locked --release \
      --bin agent-sandbox-broker \
      --bin agent-sandbox-custodian \
      --bin agent-sandbox-custody-dispatcher \
      --bin agent-sandbox-exporter \
      --bin agent-sandbox-input-custodian \
      --bin agent-sandbox-output-custodian \
      --bin agent-sandbox-output-inventory \
      --bin agent-sandbox-plan-publisher
# A result kind registry the broker would refuse at start fails the build here.
RUN target/release/agent-sandbox-broker check-kinds

RUN mkdir -p /runtime-bin /runtime-work \
    && for tool in sh install mkdir cp chmod false; do \
         ln -s /usr/local/libexec/busybox /runtime-bin/"$tool"; \
       done

FROM ${RUNTIME_IMAGE}

# Preserve the fixed init-container commands without the full Debian userland.
# Distroless cc supplies glibc, libgcc, OpenSSL and CA certificates. Debian 13's
# /bin link resolves the generated workloads' /bin/sh and /bin/false paths.
COPY --from=utilities /busybox/busybox /usr/local/libexec/busybox
COPY --from=build /runtime-bin/ /usr/bin/
COPY --chown=65532:65532 --from=build /runtime-work/ /var/lib/enrichedlab/
COPY --from=workspace /workspace/dist/ /usr/local/share/enrichedlab/workspace/

COPY --from=build /source/target/release/agent-sandbox-broker /usr/local/bin/agent-sandbox-broker
COPY --from=build /source/target/release/agent-sandbox-custodian /usr/local/bin/agent-sandbox-custodian
COPY --from=build /source/target/release/agent-sandbox-custody-dispatcher /usr/local/bin/agent-sandbox-custody-dispatcher
COPY --from=build /source/target/release/agent-sandbox-exporter /usr/local/bin/agent-sandbox-exporter
COPY --from=build /source/target/release/agent-sandbox-input-custodian /usr/local/bin/agent-sandbox-input-custodian
COPY --from=build /source/target/release/agent-sandbox-output-custodian /usr/local/bin/agent-sandbox-output-custodian
COPY --from=build /source/target/release/agent-sandbox-output-inventory /usr/local/bin/agent-sandbox-output-inventory
COPY --from=build /source/target/release/agent-sandbox-plan-publisher /usr/local/bin/agent-sandbox-plan-publisher

USER 65532:65532
WORKDIR /var/lib/enrichedlab
ENTRYPOINT ["/usr/local/bin/agent-sandbox-broker"]
CMD ["--help"]
