# Platform support

This matrix separates OpenShell support from controller support. The controller does not claim an adapter because OpenShell supports the underlying compute driver.

| Host and driver | OpenShell status | Controller status | Notes |
|---|---|---|---|
| Apple Silicon macOS, OrbStack Docker | OpenShell Docker path | Tested by the project | Bind workspace, host forwarding, requests, peer relay, evidence, and cleanup were tested with OpenShell 0.0.116. OrbStack is used as a Docker engine, not as Kubernetes. |
| Apple Silicon macOS, direct VM | OpenShell VM driver | Tested by the project, with a limit | Sync push/pull, service forwarding, source-only peer relay, evidence, and cleanup passed with OpenShell 0.0.116. The tested guest kernel did not provide Landlock, so this path needed explicit `best-effort`; the VM boundary does not turn that into a hard-Landlock claim. |
| Kubernetes with a qualified Kata RuntimeClass | OpenShell Kubernetes driver | Tested on one reference K3s/Kata deployment; requalification required per cluster | Sync, Pod and Sandbox ownership proof, RuntimeClass proof, service forwarding, authenticated TLS peer transport, exact lease NetworkPolicies, negative source and capability checks, evidence, and cleanup passed. OpenShell's Kubernetes Helm path remains upstream experimental. |
| Kubernetes with gVisor RuntimeClass | Agent Sandbox and Kubernetes runtime path | Configurable, not live qualified in this release | Use for workloads whose syscall and device needs fit gVisor. Run the full cluster gate and record the observed `runsc` handler. Do not infer gVisor from a RuntimeClass name alone. |
| Linux amd64/arm64, Docker Engine | OpenShell Docker path | Source-qualified; live driver test needed per distro | Formatting, unit and repository tests, strict Clippy, shell parsing, and a locked release build cover this source. Live mounts, forwarding, and host networking still need a host release test per runtime. |
| macOS, Docker Desktop | OpenShell documented path | Not qualified in this release | Expected Docker adapter behavior, but no release evidence yet. |
| Podman | OpenShell compute driver | Config type only, not qualified | Bind configuration exists, but runtime identity and peer custody are not qualified. Do not point `--docker-bin` at Podman and call it supported. |
| macOS lightweight VM runtimes | Runtime-specific | Use only through a qualified OpenShell gateway | The controller talks to OpenShell, not Lima or Colima directly. Record the gateway driver and run the VM release gate. |
| WSL2 with Docker Desktop | Upstream OpenShell path is experimental | Not live qualified by this project | Run the Linux controller inside WSL2 only as a separate experiment and keep the unsupported status in its receipt. |
| Windows native | No native controller path | Not supported | Use no native-Windows install claim. |

OpenShell's current references are the [support matrix](https://docs.nvidia.com/openshell/reference/support-matrix),
[sandbox compute drivers](https://docs.nvidia.com/openshell/latest/reference/sandbox-compute-drivers), and the
[experimental Helm chart notice](https://github.com/NVIDIA/OpenShell/blob/main/deploy/helm/openshell/README.md).
Kubernetes SIG Apps publishes the current [Agent Sandbox API](https://agent-sandbox.sigs.k8s.io/docs/), including
Sandbox, claim, template, warm-pool, hibernation, and resume concepts. Those APIs do not make NVIDIA's OpenShell Helm
deployment production supported. The controller observes OpenShell-owned Sandbox and Pod objects; it does not create
SandboxWarmPools or claim warm capacity. Add that as a separately tested cluster adapter if allocation latency needs
it.

Named-agent continuity uses the selected OpenShell gateway on every qualified profile. The stable identity, state, and
workspace stay on the controller; each Docker container, VM, or Kubernetes Sandbox is a disposable generation. This
does not turn a locally stored logical-agent record into a shared multi-user control plane.

## Adapter boundary

Schema 2 uses named gateway profiles. Every sandbox record freezes the profile name, OpenShell gateway, driver, workspace transport, requested RuntimeClass, and SHA-256 driver-config digest. A later profile change that alters those fields fails closed for the existing record.

Docker uses bind mounts and records its container and isolated-network identity. VM and Kubernetes profiles reject bind transport. They copy validated trees through authenticated OpenShell upload/download, cap file count and bytes, reject escaping symlinks and unsafe targets, and write a sync receipt. A pull without `--apply` changes no host work. An applied pull stores the previous workspace in the private sync-backup directory first.

The Kubernetes observer is independent of the OpenShell gateway response. It runs a bounded local `kubectl` command or an argument-validated batch SSH command and accepts at most 4 MiB of JSON. It selects one Sandbox CR by OpenShell identity, then accepts one controller-labelled Pod only when that Pod's controller owner reference has the exact Sandbox UID. A passing proof also needs the requested RuntimeClass on that Pod and an observed runtime handler. The proof includes summarized NetworkPolicy objects but does not claim that the CNI enforced every packet.

Direct VM peer links use the OpenShell sandbox runtime ID, an exact temporary endpoint in the source policy, and a relay started inside that source. Kubernetes peers verify both OpenShell runtime IDs and both observed Pod identities, then use an exact source-to-target NetworkPolicy pair, TLS, a per-lease capability, and relays in both Pods. Docker peers also require matching isolated-network identity. Podman peers fail closed.

Research state stays on the host controller in this release. Bind profiles can reconcile model proposals and snapshot
files during a segment. VM and Kubernetes profiles pull and reconcile at the segment boundary. There is no
active-active state service or cross-cluster live stream.

## Kubernetes and Kata deployment

The controller does not install a cluster or runtime handler. The cluster owner must provide all of these before enabling the profile:

- OpenShell gateway and supervisor deployed with the Kubernetes compute driver;
- Agent Sandbox CRDs/controller required by that driver;
- a RuntimeClass whose handler is installed and qualified on selected nodes;
- namespace default-deny policy plus the exact DNS, gateway, package, and control-plane egress paths the deployment needs;
- a ClusterIP-only gateway or another operator-authenticated access path;
- a bounded observer identity that can read Sandbox CRs, Pods, RuntimeClasses, and NetworkPolicies;
- for peer links only, namespace-scoped permission to get, create, and delete NetworkPolicies plus `manage_peer_network_policies = true`. The controller accepts only its two exact lease-labelled policy names and verifies the stored specs after creation.

Keep the dashboard on operator loopback. For a remote cluster, use a kube-authenticated port-forward or another local tunnel to the OpenShell gateway. Do not expose the controller dashboard as an Ingress or NodePort.
