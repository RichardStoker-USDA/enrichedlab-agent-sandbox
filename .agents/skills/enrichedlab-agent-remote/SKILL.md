---
name: enrichedlab-agent-remote
description: Configure and use an EnrichedLab remote agent broker from a researcher workstation. Use when a person or host coding agent needs to authenticate through Cloudflare Access, submit durable cluster work, reconnect, cancel work, or download verified artifacts without Kubernetes or OpenShell credentials.
license: MIT
---

# EnrichedLab remote agent client

Use this skill on the researcher's computer. The site operator supplies the broker HTTPS origin and assigned research
group. The researcher never receives a kubeconfig, database credential, OpenShell identity, controller filesystem, or
direct cluster route.

Read [references/client_setup.md](references/client_setup.md) before configuring the client or a harness adapter.

## Operating boundary

- Use an interactive Cloudflare Access user session for human work. Do not reuse one login for several people.
- Treat the Access token as a secret. Do not print it, persist it in harness config, pass it through a model prompt, or
  add it to telemetry. Let `cloudflared` hold the session.
- Confirm identity, groups, profiles, and quotas with `agent-sandbox-remote me` before submitting work.
- Use an objective file and a stable idempotency key for a retryable submission.
- A disconnected terminal does not cancel an accepted task. Reconnect by task ID or list the current user's tasks.
- Download only through the client. It verifies the server custody label and local SHA-256 before keeping a file.
- Do not give a host agent raw `kubectl`, OpenShell, broker URL override, or arbitrary download paths when typed remote
  tools are enough.

## Harness use

Pi and OpenCode can load the reviewed `remote_agent_*` adapters. The launcher, not the model, selects the client binary
and owner-only output directory and enables the tools. Login stays outside the model session.

Report the verified broker origin, principal shown by `me`, assigned groups and profiles, submitted task IDs, terminal
states, downloaded artifact paths and hashes, and any server-side limit that stopped work. Never include an Access
token or private cluster coordinate in the report.
