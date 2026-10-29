# The upload guard: one Rust binary, which also runs as its own bounded parsing worker, with qpdf
# and poppler's pdftotext and pdftoppm as its PDF helpers, and ClamAV's daemon and signature updater for the scanner
# Deployment, which runs the same image. Image digests and the Debian snapshot were checked on 2026-10-06 and the Wolfi
# package lock regenerated on 2026-10-15, after Wolfi withdrew some of its builds. The default target is the release
# image; `runtime-check` and `runtime-lock` are described below.
ARG RUST_IMAGE=rust:1.98.1-trixie@sha256:a8a5f0a1e5fe7dfe1d352591e4a1c7dd2c08fd70475cae872cf3458ba0df0546
# The runtime is Wolfi: its glibc (2.44) is newer than the build stage's, and its qpdf, poppler and ClamAV are current
# and covered by Wolfi's own security advisories. Debian 13's libxml2, curl, libtiff, expat and ClamAV carried HIGH
# findings with no fixed version, which the registry's release policy refuses.
ARG RUNTIME_IMAGE=cgr.dev/chainguard/wolfi-base@sha256:00fd4e9cdd9c5576c336d979fa663f9976f160c456b6ae9fe87e19c4e9c759a2
# The test stage's packages come from one Debian snapshot so they do not move between builds. Plain HTTP is enough:
# apt checks the signed InRelease against the image's archive keyring.
ARG DEBIAN_SNAPSHOT=20261004T000000Z

# The tests run on the build machine's own architecture. The helpers' address-space limit cannot hold under an
# instruction emulator, which reserves address space of its own, so an emulated test run would fail on the limit
# rather than on the code; the release binary is built from the same locked sources for the target below.
FROM --platform=$BUILDPLATFORM ${RUST_IMAGE} AS test
ARG DEBIAN_SNAPSHOT
RUN printf '%s\n' \
      'Types: deb' \
      "URIs: http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}" \
      'Suites: trixie trixie-updates' \
      'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' \
      'Check-Valid-Until: no' \
      '' \
      'Types: deb' \
      "URIs: http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}" \
      'Suites: trixie-security' \
      'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' \
      'Check-Valid-Until: no' \
      >/etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends qpdf poppler-utils \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /source
COPY guard/Cargo.toml guard/Cargo.lock ./
COPY guard/src ./src
COPY guard/tests ./tests
COPY guard/rules ./rules
COPY guard/policy ./policy
# The service tests need qpdf, pdftotext and pdftoppm, which this stage has, and start the built binary as their worker.
RUN GUARD_REQUIRE_TOOLS=1 cargo test --locked --release && touch /source/tests-passed

FROM ${RUST_IMAGE} AS build
WORKDIR /source
# Waits for the test stage: the release binary is built only from sources whose tests passed.
COPY --from=test /source/tests-passed ./tests-passed
COPY guard/Cargo.toml guard/Cargo.lock ./
COPY guard/src ./src
COPY guard/rules ./rules
COPY guard/policy ./policy
RUN cargo build --locked --release --bin enrichedlab-guard

# The same tests compiled for the target, with the files they read at run time, for `runtime-check`.
FROM build AS runtime-tests
COPY guard/tests ./tests
RUN cargo test --locked --release --no-run --message-format=json >/tmp/build.json \
    && sed -n 's/.*"executable":"\([^"]*\/deps\/[^"]*\)".*/\1/p' /tmp/build.json >/tmp/executables \
    && test -s /tmp/executables \
    && mkdir -p /check/source/target/release/deps \
    && while read -r test; do \
           cp "$test" /check/source/target/release/deps/ && basename "$test" >>/check/source/tests.txt; \
       done </tmp/executables \
    && cp -R Cargo.toml policy rules tests /check/source/ \
    && ln -s /usr/local/bin/enrichedlab-guard /check/source/target/release/enrichedlab-guard

# Resolves the runtime's packages against Wolfi's current index. Build this target only to change a package or the
# base, and commit its output in place of images/guard-runtime-amd64.lock:
#   docker buildx build --platform linux/amd64 --target runtime-lock --output type=local,dest=images \
#     -f images/guard.Containerfile .
FROM ${RUNTIME_IMAGE} AS runtime-resolve
RUN --mount=type=bind,source=images/apk_installed.awk,target=/run/apk_installed.awk \
    apk add --no-cache qpdf poppler-utils clamav-1.5 clamav-1.5-daemon clamav-1.5-freshclam \
    && awk -f /run/apk_installed.awk /lib/apk/db/installed | sort >/guard-runtime-amd64.lock

FROM scratch AS runtime-lock
COPY --from=runtime-resolve /guard-runtime-amd64.lock /

FROM ${RUNTIME_IMAGE} AS runtime
# Installs exactly the package builds the lock names (every package in the image, as name, version and APK checksum,
# for linux/amd64) and fails if the installed set differs from it in any package, so a rebuild of the same commit
# cannot pick up a newer or different build. apk checks each package against Wolfi's signed index.
RUN --mount=type=bind,source=images/guard-runtime-amd64.lock,target=/run/guard-runtime.lock \
    --mount=type=bind,source=images/apk_installed.awk,target=/run/apk_installed.awk \
    apk add --no-cache $(awk '{ print $1 "=" $2 }' /run/guard-runtime.lock) \
    && awk -f /run/apk_installed.awk /lib/apk/db/installed | sort | diff -u /run/guard-runtime.lock - \
    && rm -rf /var/lib/clamav/* \
    && addgroup -S -g 10001 guard \
    && adduser -S -D -H -u 10001 -G guard -h /var/lib/guard -s /sbin/nologin guard
COPY --from=build /source/target/release/enrichedlab-guard /usr/local/bin/enrichedlab-guard
COPY guard/rules/ /usr/share/enrichedlab-guard/rules/
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/enrichedlab-guard"]
CMD ["serve", "--config", "/etc/enrichedlab-guard/guard.json"]

# The release image's layers with two more on top, the target-built tests and the script that runs them, run on a
# machine of the target's architecture before the image is promoted. The service tests start the image's own binary as
# their worker.
FROM runtime AS runtime-check
COPY --from=runtime-tests /check/source/ /source/
COPY images/guard-runtime-check.sh /source/run-tests
ENTRYPOINT ["/source/run-tests"]
CMD []

# The release image.
FROM runtime
