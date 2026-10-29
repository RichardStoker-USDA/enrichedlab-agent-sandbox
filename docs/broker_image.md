# Broker and custodian image

`images/broker.Containerfile` builds the reusable direct-Sandbox broker and its
fixed site helpers from one locked source tree. It preserves `images/Containerfile`,
which remains the OpenShell sandbox image and is not a broker runtime.

The recipe owns the digest-pinned Rust build toolchain; `Cargo.toml` owns the
minimum supported Rust version. The final image uses the pinned Debian 13
distroless C/C++ runtime, which supplies glibc, libgcc, OpenSSL and CA certificates.
It copies the pinned debug variant's BusyBox binary for the existing fixed `sh`,
`install`, `mkdir`, `cp`, `chmod` and `false` commands. It does not install packages
into the final runtime. The image runs as UID/GID 65532 with its working directory
at `/var/lib/enrichedlab`.
The image contains these binaries:

- `agent-sandbox-broker`
- `agent-sandbox-custodian`
- `agent-sandbox-custody-dispatcher`
- `agent-sandbox-exporter`
- `agent-sandbox-input-custodian`
- `agent-sandbox-output-custodian`
- `agent-sandbox-output-inventory`
- `agent-sandbox-plan-publisher`

The broker is the default entrypoint. Custodian, custody-dispatcher, inventory,
and exporter workloads override it with a fixed absolute binary path. The
`agent-sandbox-custody-dispatcher` target is supplied by the separately scoped
portable dispatcher source and is built only in the combined successor that contains
that source file. The direct-Sandbox worker does not
require the legacy `agent-sandbox` OpenShell CLI in this image. A legacy OpenShell
worker still needs its separately qualified runtime and must not use this as proof
that its CLI mode is runnable.

The plan publisher is an operator tool. It copies one already approved private
plan into the exact custodian plan root; it does not approve the plan, execute
it or bypass the custodian's current-task checks. Grant its write mount only
to the trusted publication job or catalog preparer, never to the worker or a researcher sandbox.
See [plan publication](plan_publisher.md) for its input and receipt contract.

Build from the repository root with the tracked `.dockerignore` context:

```sh
docker buildx build --platform linux/amd64 --file images/broker.Containerfile .
```

Before promotion, verify the pinned inputs and scan the resulting immutable image.
Retain the package inventory, scan database identity and findings with the release;
a historical scan of another runtime is not evidence for this image.

Run each fixed helper's `--help` entrypoint in the actual Linux image as UID 65532,
with networking disabled and a read-only root. Resolve any missing shared libraries
against that image's filesystem using the build or inspection environment. Do not
assume `ldd` or a package manager exists in the final image. When the native-TLS or
database boundary changes, exercise its real caller as part of the affected release.
A successful source check or build does not establish runtime or live acceptance.

## Coordinator readiness

For a site-owned coordinator Deployment, pass
`--ready-file /var/run/enrichedlab/coordinator/ready.json` to `agent-sandbox-broker coordinate`. Mount a pod-local
`emptyDir` at `/var/run/enrichedlab/coordinator`, writable by UID 65532 (for example with pod `fsGroup: 65532`),
including when the root filesystem is read-only. Do not share the marker directory between coordinator processes.
The parent must exist before the process starts. Startup clears stale readiness and publishes the private file only
after all start checks pass and the claim loop begins; shutdown clears it.

The image's `/usr/bin/sh` is the packaged BusyBox shell and has the `test` builtin. No standalone `/usr/bin/test`,
JSON utility or extra readiness subcommand is required. Set this probe on the coordinator container:

```yaml
readinessProbe:
  exec:
    command:
      - /usr/bin/sh
      - -c
      - test -f /var/run/enrichedlab/coordinator/ready.json
  periodSeconds: 2
  timeoutSeconds: 1
  failureThreshold: 1
  successThreshold: 1
```

Set the Deployment's `strategy.type: RollingUpdate`, `strategy.rollingUpdate.maxUnavailable: 0` and
`strategy.rollingUpdate.maxSurge: 1`. A replacement whose start checks fail then cannot displace the old ready Pod.
This probe checks the atomic marker's presence, not ongoing service health. The marker contains `ready_at` and
`version` for inspection. Qualify the probe and rolling update in the site's image and namespace before rollout;
source checks do not establish deployment readiness.
