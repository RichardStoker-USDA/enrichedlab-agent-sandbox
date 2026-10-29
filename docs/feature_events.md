# Broker feature events

The broker writes one compact JSON line to stderr when a feature attempt ends. This is the same container log stream
the platform uses for `enrichedlab.telemetry.ars115_attempt`; the cluster log collector sends it through the OTel
gateway. The event writer accepts typed IDs, counts, outcomes, and broker-owned error codes only. It has no field for a
query, prompt, model response, file name, person name, email, address, token, secret, or upstream error text.

The stable feature registry is:

| `feature.id` | `module` | Completion boundary |
|---|---|---|
| `broker.query` | `research-broker` | A guarded query worker attempt ends |
| `broker.analysis` | `research-broker` | An analysis worker attempt ends |
| `broker.figure` | `research-broker` | A figure worker attempt ends |
| `broker.dataset` | `research-broker` | A dataset worker attempt ends |
| `broker.literature` | `research-coordinator` | A literature read or policy refusal ends |
| `broker.package` | `research-broker` | A preview completes or a download stream succeeds, fails, or is dropped |
| `broker.campaign` | `research-coordinator` | One claimed campaign cycle ends |
| `coordinator.task.step` | `research-coordinator` | One claimed research-turn step ends |

Each event uses exactly the console field list: `event.name`, `event.outcome`, `error.code`, `duration_ms`, `attempt`,
`principal.kind`, `lab`, `module`, `feature.id`, `release`, `trace_id`, `span_id`, `items.returned`, and
`items.refused`. Every emitted event has `principal.pseudonym`; `agent.run_id` is present only for an agent event.
API package events continue a valid W3C `traceparent`; background work creates a local trace identifier.
These log events do not replace or mutate broker audit rows.
For package downloads, a dropped response uses outcome `cancelled` and code `cancelled`; an artifact that changes
between measurement and streaming uses outcome `failure` and code `package_changed_while_streaming`.

## Configuration

- `AGENT_SANDBOX_FEATURE_PSEUDONYM_KEY_FILE` names an absolute, regular, single-link file owned by the process user,
  mode 0600, holding at least 32 bytes. The HMAC-SHA256 key and caller ID are never logged. A Kubernetes projected
  Secret is a symlink and cannot be used at this path directly. Use the estate's init-container copy into a
  memory-backed `emptyDir`, with the copied file owned by the broker UID and mode 0600; mount only that copy read-only
  in the broker container. API, worker, coordinator, and campaign-director processes always emit feature events and
  refuse to start without this setting.
- `AGENT_SANDBOX_FEATURE_METRICS_BIND` enables the separate Prometheus listener, for example `0.0.0.0:32111`. It is
  off when unset and serves only `GET /metrics`.
- `AGENT_SANDBOX_RELEASE` must match the Pod's `app.kubernetes.io/version` label. It defaults to the crate version and
  accepts only a bounded version token.

The listener exports `enrichedlab_feature_events_total{module,feature,outcome,error_code,lab}` and
`enrichedlab_feature_duration_seconds{module,feature,outcome}`. Feature, module, outcome, and error labels come from
closed enums. Lab IDs must use the broker's lowercase identifier syntax; the process retains at most 64 distinct lab
labels and maps invalid or excess values to `other`.

Cluster deployment must create a Secret with one HMAC key, project it only into the copy init container, and copy it
without logging to `/var/run/secrets/enrichedlab-feature-telemetry/hmac.key` in a memory-backed `emptyDir`. Set
`AGENT_SANDBOX_FEATURE_PSEUDONYM_KEY_FILE` to that path, `AGENT_SANDBOX_FEATURE_METRICS_BIND` to
`0.0.0.0:32111`, and `AGENT_SANDBOX_RELEASE` from the Pod's `app.kubernetes.io/version` label. Add a named
`feature-metrics` TCP container port 32111 to the API, worker, coordinator, and campaign-director Pods and these Pod
annotations:

```yaml
prometheus.io/scrape: "true"
prometheus.io/port: "32111"
prometheus.io/path: /metrics
```

Keep `foundation.enrichedlab.org/telemetry: research-metadata`,
`foundation.enrichedlab.org/telemetry-payload: metadata-only`, and `app.kubernetes.io/version` on each Pod so the
node OTel agent accepts the JSON event lines and gives them the right release. Add ingress on TCP 32111 only from
the observability namespace's Pods labeled `app.kubernetes.io/name: vmagent`; do not expose this listener through
the public broker Service. The namespace's existing default-deny policy remains in place.
