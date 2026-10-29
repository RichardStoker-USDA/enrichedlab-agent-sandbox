---
name: enrichedlab-agent-broker
description: Deploy, upgrade, or qualify the EnrichedLab multi-user agent broker on Kubernetes. Use for site-owned Cloudflare Access identity, research-group quotas, PostgreSQL task custody, Flux rollout, OpenShell worker placement, or cluster broker recovery. Do not use for a researcher's local client setup.
license: MIT
---

# EnrichedLab cluster broker

This is a site-operator workflow. Read [references/site_deployment.md](references/site_deployment.md) before any live
cluster, identity, database, image, network, or Flux change.

## Invariants

- Keep site values in a private GitOps overlay. The reusable repository contains no invitations, hostnames, audience
  tags, client bundles, credentials, storage coordinates, or model routes.
- Expose only the broker API through an authenticated Cloudflare Tunnel. Never expose OpenShell, Kubernetes,
  PostgreSQL, persistent volumes, the controller dashboard, or a kubeconfig to researchers.
- Validate `Cf-Access-Jwt-Assertion` at the broker with the exact issuer and audience. Do not trust an email header,
  client tenant string, or telemetry baggage as identity.
- Use distinct migration-owner, API, and worker database roles. Long-running Pods get function-only runtime roles and
  never receive the migration credential.
- Reconcile foundation, migration, grants, catalog seed, and workloads in that order. Publish and scan the immutable
  image before a Flux-owned workload refers to it.
- One RWO controller state tree has one worker leader. Scale concurrent agents with `max_parallel` and on-demand
  OpenShell sandboxes, not active-active workers over the same files.
- Keep untrusted sandboxes away from control-plane, storage, and inference nodes unless a qualified site policy says
  otherwise. Agents call approved inference and tool services through narrow policy routes.
- NVIDIA's OpenShell Kubernetes path remains experimental upstream. A site qualification applies only to the recorded
  versions, CNI, RuntimeClass handler, storage class, node pool, image digests, and policies.

## Completion receipt

Report the source commit and image digest, broker release, Kubernetes and OpenShell versions, runtime handler, database
roles and migration version without credentials, Access issuer host and audience fingerprint, group/profile catalog
digest, active worker concurrency, allowed ingress and egress paths, passed positive and negative tests, rollback
reference, and every unqualified claim. Do not call the deployment production-qualified unless the exact live release
matrix passed.
