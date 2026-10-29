# Researcher client setup

## Before changing the workstation

Get these non-secret values from the site operator:

- the broker HTTPS origin;
- the research-group key;
- the profile key or profiles assigned to the user;
- the intended local artifact directory.

The workstation needs a browser, `cloudflared`, and the `agent-sandbox-remote` binary built from the server's qualified
source release. Stop if the origin is not HTTPS or the operator asks for a kubeconfig, OpenShell bundle, database DSN,
or shared Access token.

## Configure and prove identity

```bash
agent-sandbox-remote configure --server https://agents.example.invalid
agent-sandbox-remote login
agent-sandbox-remote me
```

The login opens the site's identity-provider and MFA flow. The CLI sends the user token to the Cloudflare edge in the
`Cf-Access-Token` header. Cloudflare injects `Cf-Access-Jwt-Assertion` at the origin; the broker validates that assertion
against its exact issuer, audience, expiry, algorithm, and JWKS.

`me` must show the expected email, group quotas, and assigned profiles. An uninvited identity should receive a
broker denial even after Cloudflare authentication.

## Submit and reconnect

Create an owner-only objective file that describes one bounded task and its output checks:

```bash
agent-sandbox-remote submit \
  --group <group-key> \
  --profile <profile-key> \
  --objective-file /absolute/path/objective.txt \
  --idempotency-key <stable-request-key>

agent-sandbox-remote list
agent-sandbox-remote get <task-id>
agent-sandbox-remote events <task-id> --after 0
agent-sandbox-remote artifacts <task-id>
agent-sandbox-remote download <task-id> <artifact-id> --output /absolute/new/file
```

Reusing an idempotency key with the same request returns the same task. Reusing it with different work is rejected.
Cancellation is explicit: `agent-sandbox-remote cancel <task-id>`.

## Pi and OpenCode

The launcher creates a private output root, sets `ENRICHEDLAB_REMOTE_BROKER_ENABLED=1`, points
`ENRICHEDLAB_AGENT_REMOTE_BIN` to an absolute binary, and sets `ENRICHEDLAB_REMOTE_OUTPUT_ROOT`. Load
`scripts/harness/pi_omp_remote_broker.mjs` in Pi. For OpenCode, import
`createOpenCodeRemoteBrokerTools` from `scripts/harness/opencode_remote_broker.ts` in a local tool file and export the
returned tools.

Do not put login commands or token access in a model-callable tool. The adapter restricts downloads to
`<output-root>/<task-id>/<safe-name>` and refuses arbitrary paths.

## Acceptance checks

- intended identity and MFA policy pass;
- uninvited and wrong-audience identities fail;
- `me` shows only assigned groups and profiles;
- duplicate submission is idempotent;
- disconnect and reconnect keep the task running;
- one user cannot read, cancel, or download another user's task;
- a downloaded artifact matches its advertised size and SHA-256;
- no workstation process receives a kubeconfig, OpenShell bundle, or database credential.
