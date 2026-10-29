# Kubernetes ServiceAccount submission authentication

`agent-sandbox-broker serve` uses Cloudflare Access by default. A site can opt in to
an exact Kubernetes ServiceAccount submitter as a bearer-only fallback, or choose
`service-account-only` for a private headless API. The adapter asks the configured
Kubernetes API server to TokenReview the presented bearer token with one exact
audience, then accepts only an exact namespace and ServiceAccount mapping in a
private configuration file. A request never supplies its tenant, namespace,
ServiceAccount, email, or catalog principal.

The mapping produces a distinct `kubernetes://serviceaccount` issuer and the fixed
subject `system:serviceaccount:<namespace>:<service-account>`. Its configured
`catalogEmail` must have a normal enabled invitation in the same tenant catalog.
The existing store then applies the unchanged group/profile grants and quotas.
This is not a Cloudflare identity and it does not bypass catalog authorization.

In the default Cloudflare mode, Cloudflare Access remains authoritative when a
request carries `Cf-Access-Jwt-Assertion`, including when that assertion is invalid.
A bearer-only request reaches the ServiceAccount adapter only when this private
configuration is mounted and `AGENT_SANDBOX_BROKER_SERVICE_ACCOUNT_AUTH_FILE` names
it.

For a private headless API, set
`AGENT_SANDBOX_BROKER_AUTH_MODE=service-account-only` and provide the private
ServiceAccount configuration. That mode does not construct a Cloudflare verifier or
JWKS client, rejects any `Cf-Access-Jwt-Assertion`, and rejects configured
Cloudflare issuer, audience, JWKS URL, or JWKS CA inputs. A broker URL is optional in
this mode: without one, accepted task responses use a relative
`/api/v1/tasks/<task-id>` Location; a supplied URL must be a real HTTPS origin. The
default Cloudflare and mixed fallback behavior is unchanged.

## Private configuration shape

The configuration contains references and catalog identity metadata, never token
values or database credentials. Use a short-lived projected caller token with the
same `audience` value. The broker asks TokenReview for that audience and requires
it in `status.audiences`; an authenticated review with an empty or mismatched
intersection is denied.

```json
{
  "schemaVersion": "enrichedlab.broker.service-account-auth/v1",
  "apiServer": "https://kubernetes.default.svc",
  "reviewerTokenRoot": "/var/run/enrichedlab/token-review-source",
  "reviewerTokenFile": "reviewer-token",
  "caFile": "/run/enrichedlab/token-review/ca.crt",
  "audience": "enrichedlab-broker-submit",
  "mappings": [
    {
      "namespace": "scheduled-agent-system",
      "serviceAccount": "task-submitter",
      "catalogEmail": "scheduled-submitter@example.invalid",
      "catalogDisplayName": "Scheduled task submitter"
    }
  ]
}
```

`apiServer` must be an HTTPS origin. The configuration and CA must be separate
mode-`0600`, single-link regular files in the same mode-`0700` private root,
owned by the API process. The broker holds no-follow descriptors from that root.
`reviewerTokenRoot` is a distinct root-owned, non-writable projected-volume root and
`reviewerTokenFile` is one plain filename. The broker holds that root and allows
only Kubernetes' exact `reviewer-token -> ..data/reviewer-token` rotation link;
it reads the current data child with no-follow semantics on every review.

## Private overlay inputs

The public workload remains tokenless and does not include the ClusterRole. A
private overlay that explicitly enables this adapter must add
[`service-account-auth-rbac.yaml`](../deploy/kubernetes/broker/workloads/service-account-auth-rbac.yaml), mount its
nonsecret JSON ConfigMap, and patch the API Pod with the configuration path and a
reviewer token projection. The only ClusterRole permission is
`authentication.k8s.io/tokenreviews` `create` for `agent-broker-api`.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: agent-broker-api
spec:
  template:
    spec:
      containers:
        - name: api
          env:
            - name: AGENT_SANDBOX_BROKER_SERVICE_ACCOUNT_AUTH_FILE
              value: /run/enrichedlab/token-review/service-account-auth.json
          volumeMounts:
            - name: token-review
              mountPath: /var/run/enrichedlab/token-review
              readOnly: true
            - name: token-review-source
              mountPath: /var/run/enrichedlab/token-review-source
              readOnly: true
      initContainers:
        - name: stage-service-account-auth
          image: <the same immutable broker image>
          command: ["/bin/sh", "-eu", "-c"]
          args:
            - >-
              install -d -o 65532 -g 65532 -m 0700 /private &&
              install -o 65532 -g 65532 -m 0600 /source/ca.crt /private/ca.crt &&
              install -o 65532 -g 65532 -m 0600 /source/service-account-auth.json
              /private/service-account-auth.json
          volumeMounts:
            - name: token-review-source
              mountPath: /source
              readOnly: true
            - name: token-review
              mountPath: /private
      volumes:
        - name: token-review
          emptyDir:
            sizeLimit: 1Mi
        - name: token-review-source
          projected:
            defaultMode: 0440
            sources:
              - serviceAccountToken:
                  path: reviewer-token
                  audience: https://kubernetes.default.svc
                  expirationSeconds: 600
              - configMap:
                  name: kube-root-ca.crt
                  items:
                    - key: ca.crt
                      path: ca.crt
              - configMap:
                  name: broker-service-account-auth
                  items:
                    - key: service-account-auth.json
                      path: service-account-auth.json
```

The overlay must also allow the API Pod's HTTPS route to its configured Kubernetes
API endpoint under the site CNI policy. The reusable base does not guess an API
address or CNI entity. Keep that egress limited to the API Pod and the configured
API endpoint. Do not add a public listener, wildcard mapping, or client-supplied
identity claim.

## Request boundary

A scheduled service sends its own projected token as `Authorization: Bearer ...`
to the existing broker endpoint and continues to provide the existing mutation
intent and idempotency headers. The endpoint validates the normal task request,
then the broker resolves the fixed mapped catalog identity before applying the
existing authorization and quota functions. The TokenReview adapter never creates
an invitation, a profile grant, or a task directly.

Disable the adapter by removing the private environment variable, config volume,
and optional ClusterRoleBinding. The next restart returns the API to Cloudflare
Access-only authentication.
