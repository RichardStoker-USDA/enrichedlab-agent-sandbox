ARG RUST_IMAGE=rust:1.93.1-bookworm@sha256:7c4ae649a84014c467d79319bbf17ce2632ae8b8be123ac2fb2ea5be46823f31
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

FROM ${RUST_IMAGE} AS build
WORKDIR /source
COPY Cargo.toml Cargo.lock build.rs ./
COPY kinds ./kinds
COPY proto/enrichedlab_evidence_v1.binpb ./proto/enrichedlab_evidence_v1.binpb
COPY src ./src
COPY examples ./examples
COPY migrations ./migrations
COPY policies ./policies
COPY profiles ./profiles
COPY runtime ./runtime
COPY schemas ./schemas
COPY web ./web
RUN cargo build --locked --release \
      --bin agent-sandbox \
      --bin agent-sandbox-broker \
      --bin agent-sandbox-relay \
      --bin agent-sandbox-remote \
      --bin agent-sandbox-exporter \
      --bin agent-sandbox-custodian
# A result kind registry the broker would refuse at start fails the build here.
RUN target/release/agent-sandbox-broker check-kinds

FROM ${RUNTIME_IMAGE}
ARG TARGETARCH
ARG OPENSHELL_VERSION=0.0.116
ARG OPENSHELL_AMD64_SHA256=883b5223399dd30a2b33b7f76817fd81295d610ce6f2d04a0266fc7cfbf87575
ARG OPENSHELL_ARM64_SHA256=d39e93477e8a160012fc199ae1d08ddea15a2e76ba700dcf7c485593a9bd6e1b

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
         ca-certificates curl git \
    && case "${TARGETARCH}" in \
         amd64) package_arch=amd64; package_sha="${OPENSHELL_AMD64_SHA256}" ;; \
         arm64) package_arch=arm64; package_sha="${OPENSHELL_ARM64_SHA256}" ;; \
         *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 2 ;; \
       esac \
    && package="openshell_${OPENSHELL_VERSION}-1_${package_arch}.deb" \
    && curl --fail --location --silent --show-error \
         "https://github.com/NVIDIA/OpenShell/releases/download/v${OPENSHELL_VERSION}/${package}" \
         --output "/tmp/${package}" \
    && printf '%s  %s\n' "${package_sha}" "/tmp/${package}" | sha256sum --check --strict \
    && apt-get install --yes --no-install-recommends "/tmp/${package}" \
    && rm -f /usr/bin/openshell-gateway \
         /usr/lib/systemd/user/openshell-gateway.service \
         /usr/libexec/openshell/openshell-driver-vm \
    && rm -f "/tmp/${package}" \
    && apt-get purge --yes --auto-remove curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 65532 agent \
    && useradd --uid 65532 --gid 65532 --home-dir /home/agent --create-home --shell /usr/sbin/nologin agent

COPY --from=build /source/target/release/agent-sandbox /usr/local/bin/agent-sandbox
COPY --from=build /source/target/release/agent-sandbox-broker /usr/local/bin/agent-sandbox-broker
COPY --from=build /source/target/release/agent-sandbox-relay /usr/local/bin/agent-sandbox-relay
COPY --from=build /source/target/release/agent-sandbox-remote /usr/local/bin/agent-sandbox-remote
COPY --from=build /source/target/release/agent-sandbox-exporter /usr/local/bin/agent-sandbox-exporter
COPY --from=build /source/target/release/agent-sandbox-custodian /usr/local/bin/agent-sandbox-custodian

USER 65532:65532
WORKDIR /var/lib/enrichedlab
ENTRYPOINT ["/usr/local/bin/agent-sandbox-broker"]
CMD ["--help"]
