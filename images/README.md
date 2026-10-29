# Sandbox image template

The Containerfile extends the same digest-pinned OpenShell community base used by the controller and installs a small development tool set. It contains no harness configuration, model route, credential, SSH key, or host detail.

Build it with a local tag:

```bash
docker build --file images/Containerfile --tag enrichedlab-agent-sandbox:local .
docker image inspect enrichedlab-agent-sandbox:local --format '{{.Id}} {{.Config.WorkingDir}}'
docker run --rm --entrypoint /bin/sh enrichedlab-agent-sandbox:local -lc 'id -u; python3 --version; rg --version | head -1'
```

The working directory must not be `/sandbox/work`; OpenShell reserves that target for the mounted project workspace. Scan the image, start a disposable OpenShell sandbox, verify the expected UID and tools, and use an immutable published digest in `agent-sandbox create --image`. Package versions from an APT repository can change between builds even though the base digest is pinned. For a reproducible release image, mirror and pin package artifacts or publish a scanned image digest from a controlled build.

Create separate images for development and data science when their dependency sets differ. Do not make one image with every tool. Keep templates editable through normal source review and do not put secrets in build arguments or layers.

`controller.Containerfile` builds the four Rust binaries and installs a checksum-pinned OpenShell CLI. It is the image
used by the remote broker API and its single controller worker. The image removes the package's gateway, systemd unit,
and local VM driver because the worker connects to a separately operated Kubernetes gateway and must not start either
component. Build each target architecture natively or with a reviewed `buildx` builder, scan the result, and replace
the example Kubernetes image with the published digest. The OpenShell package version is independent from the sandbox
image digest and must pass the cluster qualification gate before either one is promoted.

## Direct broker runtime

`broker.Containerfile` packages the API/direct worker and seven custody, inventory, export and plan helpers.
It uses a digest-pinned Distroless Debian 13 C/C++ runtime for glibc, libgcc, OpenSSL and CA certificates.
A separately pinned upstream utility image supplies static BusyBox. The image installs command links for
`sh`, `install`, `mkdir`, `cp`, `chmod` and `false`, which the existing private-file staging contracts require.
The BusyBox binary still contains its other applets; these command links are compatibility plumbing, not an
execution authorization boundary. Pod identity, read-only mounts, network policy and capability limits remain required.

The final image has no APT installation step and does not inherit the full Debian command-line distribution.
The Rust builder, runtime and utility inputs have separate build arguments so each can be updated by digest.
Only the builder installs a package: `libclang-dev`, which the query guard's parser needs to generate its bindings.
It comes from the Debian snapshot named by `DEBIAN_SNAPSHOT`, the same date the Rust image's own sources record, so
the builder's packages stay fixed. Move both together when the Rust image changes.
Verify the two Distroless signatures with the upstream keyless identity described in its
[verification procedure](https://github.com/GoogleContainerTools/distroless#how-do-i-verify-distroless-images).
Keep the base package metadata, upstream component notices, BusyBox license/source information, SBOM and build
provenance with a distributed image. This image recipe does not change the licenses of its upstream components.

Build for the deployment architecture and record the resulting immutable image digest. Before release, start all
eight binaries with `--help`, then exercise the actual init-container command strings as UID/GID 65532 with a
read-only root, read-only projected inputs and a writable temporary destination. Check copied bytes and private
file modes. Scan the assembled image with a current, pinned vulnerability database; an upstream signature or
successful startup does not establish that its dependencies are free of known vulnerabilities.

The direct broker image does not include the legacy OpenShell controller or the private site's Ruby renderer.
Those remain separate runtime images selected by their existing deployment contracts.
