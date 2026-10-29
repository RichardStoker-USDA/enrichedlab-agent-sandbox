# Qualification matrix

| Gate | Docker | VM | Kubernetes | Source-only |
|---|---:|---:|---:|---:|
| Locked build, tests, Clippy, audit | required | required | required | required |
| Skill and disclosure validation | required | required | required | required |
| Gateway driver observation | required | required | required | no |
| Workspace write and host-root denial | required | required | required | no |
| Bind mount modes | required | no | no | no |
| Push/pull digest and pre-apply backup | no | required | required | no |
| Runtime object identity | Docker | VM gateway receipt | CR, Pod, RuntimeClass | no |
| Host/private-network sentinels | required | required | required where routable | no |
| Distinct local/target service ports | required | required | required | no |
| Peer cleanup | release change only | release change only | release change only | no |
| Evidence snapshot and audit chain | required | required | required | no |
| Automatic Pi and OpenCode event and output-version capture | required for state claim | required for state claim | required for state claim | fixture only |
| Fresh-session checkpoint and malformed-stream rejection | required for state claim | required for state claim | required for state claim | fixture only |
| Named-agent release and new-generation state restore | required for continuity claim | required for continuity claim | required for continuity claim | unit fixture only |
| Route-cap concurrency and explicit retry bounds | required for continuity claim | required for continuity claim | required for continuity claim | unit fixture only |
| Metadata-only state trace in requested browser | release change only | release change only | release change only | no |
| Provenance DAG, time controls, and leased restricted read | release change only | release change only | release change only | source fixture only |
| PostgreSQL tenant, append-only, lease, and restore gates | when PostgreSQL is selected | when PostgreSQL is selected | when PostgreSQL is selected | fixture only |
| Requested interactive browser | release change only | release change only | release change only | no |
| No remaining disposable resources | required | required | required | no |

## Receipt

```text
Controller: <version and commit>
Platform: <host, architecture, runtime, driver>
OpenShell: <version and gateway>
Source gates: <passed or exact blocked row>
Allowed path: <measured result>
Denied path: <measured result>
Service path: <target port, loopback port, result>
Evidence: <private receipt path and digest>
Cleanup: <sandbox, lease, policy, listener result>
Browser: <browser/version/result or blocked reason>
Limits: <claims not made>
```

Keep raw logs, prompts, credentials, research data, and private host coordinates out of a reusable release receipt.
