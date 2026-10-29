---
name: enrichedlab-agent-operations
description: Operate governed disposable subagents with EnrichedLab Agent Sandbox. Use for creating, running, stopping, restarting, deleting, mounting, syncing, exposing host or LAN services, linking peers, reviewing access requests, managing team branches, cataloging outputs, inspecting the dashboard, or collecting evidence through NVIDIA OpenShell.
license: MIT
---

# EnrichedLab agent operations

This skill needs an installed `agent-sandbox` controller and a configured NVIDIA OpenShell gateway profile.

The host agent is the operator. A sandboxed agent may request access, but it never approves its own request. Keep the
dashboard, controller state, PKI, Git integration, service decisions, and final research synthesis outside workers.

## Start

Run these read-only checks:

```bash
agent-sandbox doctor
agent-sandbox platform list
agent-sandbox list
agent-sandbox service list
```

For Docker, `docker info` must also pass. Read the selected profile before assuming bind or sync behavior. Use a named
private inference route, or `none`. Never place a provider credential in a route file, prompt, mount, image, or repo.

Create the smallest workspace and policy the task needs:

```bash
agent-sandbox create --name <sandbox> --route <route> --gateway-profile <profile>
agent-sandbox run <sandbox> --harness <open-code-or-pi> -- '<task, boundaries, output location, and acceptance check>'
```

The direct run returns a provenance run ID. Pi and OpenCode events, including provider-returned reasoning, are recorded
outside prompt state. Use `provenance ingest --harness native` for a custom scientist harness rather than parsing a
private transcript format.

When a named subagent must continue after compute is deleted, use
`../enrichedlab-agent-continuity/SKILL.md`. Do not keep an ordinary sandbox running only to preserve model memory.

Docker bind profiles may receive exact extra mounts. VM and Kubernetes profiles use bounded sync and reject host bind
mounts. Never mount home, `.git` from another checkout, agent config, credential folders, container sockets, or
Kubernetes identity.

## Access and services

Read [references/service_and_approval.md](references/service_and_approval.md) before opening a port, changing network
policy, approving a request, or linking peers. Host access is loopback first. LAN access is a separate short mTLS
lease. Use `openshell forward service` through `agent-sandbox service open`; do not use same-port shorthand.

## Teams

The host freezes a clean base and assigns literal non-overlapping paths. Workers have separate clones and no remote.
They commit their branch; the host collects, reviews, and merges. Workers never merge or push.

```bash
agent-sandbox team create <team> --repo /absolute/repo --shared-output
agent-sandbox team add <team> <worker> --owns <path> --task '<bounded task>' --route <route>
agent-sandbox team collect <team> <worker>
agent-sandbox team merge <team> <worker>
```

Use `team stop` for a resumable project and `team delete` after evidence capture. The project and outputs remain.

## Scientific research teams

Use `../enrichedlab-research-team/SKILL.md`. Research workers get sealed input snapshots, separate perspectives, and
organized output custody. Do not use ordinary shared-output teams as a substitute for blind independent analyses.

For scoped working state, context epochs, decision records, and provenance exports, use
`../enrichedlab-agent-state/SKILL.md`. Do not replace host access decisions or the audit log with agent state.

The dashboard's normal state, global search, Audit search, and Provenance replay are metadata-only. Restricted search
and reads need a short evidence lease for one run and purpose. Use the live Access count to review the bounded queue;
the browser marks a stopped state feed stale and offers `Reconnect`. Do not expose the dashboard outside loopback or
give a sandbox its evidence token.

When a Team Lead uses the reviewed Pi, OMP, or OpenCode adapter, it may list and reject requests for its own active
worker. Approval is allowed only for the launcher-configured low-risk kinds and exact read domains. Treat every
`human_required` result as blocked until the operator decides it in the dashboard or CLI. A synchronous Team Lead run
cannot service its own worker's request; use a separate supervisor while that run is active.

## Close

```bash
agent-sandbox service close <lease-id>
agent-sandbox audit snapshot <sandbox>
agent-sandbox audit verify
agent-sandbox delete <sandbox>
agent-sandbox service list
agent-sandbox list
```

Deleting keeps work unless the operator explicitly selects a controller-created workspace for trash. Confirm
temporary listeners, peer processes, policy entries, runtime objects, and controller records are absent. Preserve
failed evidence and recover through the recorded owner instead of starting a second cleanup path.
