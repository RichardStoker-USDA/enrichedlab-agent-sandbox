---
name: enrichedlab-agent-continuity
description: Create, resume, run, release, checkpoint, inspect, or retire a stable EnrichedLab logical agent whose scoped state and workspace persist across disposable OpenShell runtimes and fresh model sessions.
license: MIT
---

# EnrichedLab logical-agent continuity

Use a logical agent when the same named subagent needs to continue work after its container, Pod, microVM, or model
session has been destroyed. Read
[references/logical_agent_lifecycle.md](references/logical_agent_lifecycle.md) before changing lifecycle or retry
behavior.

The logical-agent ID, state run, and workspace are durable. The OpenShell sandbox name and provider session are not.
Do not pass a continuation or chat-session ID to imitate continuity. Start every provider session from the fixed
objective, the current scoped `.agent-state/view.json`, and the current task.

## Operate

Register a stable identity once. Pick the state profile from the job, not from the model name:

```bash
agent-sandbox agent create <agent-name> \
  --owner <stable-principal> \
  --domain <cluster-ops|scientific|software-engineering> \
  --state-profile <cluster-operator|scientific-worker|research-lead|software-engineer> \
  --objective '<durable objective>' \
  --role '<bounded role>' \
  --route <private-route> \
  --gateway-profile <qualified-profile>

agent-sandbox agent run <agent-name> -- '<bounded task and acceptance check>'
agent-sandbox agent status <agent-name>
```

`agent run` allocates compute when absent and releases it after the task. Use `--keep-runtime` only when the operator
accepts the idle capacity cost and needs lower latency for the next task. `agent start` allocates without running;
`agent release` reconciles state, checkpoints, captures evidence, and deletes compute. `agent retire` releases compute,
seals the state run, and retains the record, workspace, provenance, and artifacts.

Retries are off by default. `--max-attempts` explicitly allows fresh provider sessions to repeat a task. Do not enable
retry for a task with non-idempotent side effects unless its tool boundary has an idempotency key, fenced lease, or
reviewed recovery rule.

Each provider session defaults to a 3600-second deadline. Set `--attempt-timeout-seconds` between 30 and 86400 for the
task size. A timed-out attempt is evidence, not success, and automatic release still applies unless state or evidence
custody cannot be confirmed.

Run `agent-sandbox state verify` and `agent-sandbox audit verify` after a lifecycle failure. If a live sandbox exists
without its controller record, stop and recover custody. Do not create another runtime under the same logical agent.

## Shared control planes

The CLI record store is for one trusted local controller. A multi-user web app must put authenticated tenant and user
identity, capability checks, durable task leases, idempotency, and row-level data isolation in its server-side broker.
It may invoke the same lifecycle contract, but it must not expose the loopback dashboard, hand Kubernetes credentials
to a browser or model, or share this local JSON record directory between replicas.
