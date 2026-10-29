# Input-copy publication

The trusted custodian prepares an input-copy workload from a claimed task, its
server-owned binding, an approved source PVC UID, and a declared file manifest.
`PreparedInputPublication` uses the fixed renderer and rejects a declared manifest
whose digest differs from the approved input digest before creating any resource.

Publication writes the original request and its declared manifest into private
custody with exclusive atomic publication. It then creates, in order, the deny-all
NetworkPolicy, input PVC, artifact PVC, immutable request Secret, and copy Job.
Request content is server-side only. Public receipts and Debug output contain
identities and hashes, never declared paths, file bytes, or Secret bodies.

The same task, attempt, and lease generation reserve one private publication
journal. Each API call has a durable create intent before it and a UID receipt
after it. An interrupted API request, conflict, invalid receipt, or partial
publication requires recovery against those exact identities. Publication does
not overwrite, adopt, delete, or automatically replay a partial scope.

`InputSourceLeaseGuard` refreshes the broker lease and cancellation state, the
approved source's namespace, UID, run, tenant, principal, role, bound state, and
writer observation before each create and after the Job create. Input sources
must already be sealed. Source access approval and policy are site authority:
observation is not a lock. The site must prevent a new writer or replacement
throughout copying and limit this publisher to its approved namespace. A caller
must not use a permissive plugin callback as the publication guard.

`PublishedInputCopy` keeps the exact request and derives its completion expectation
from the trusted rendered Job plus its create UID. Its `observe_seal` method uses
the existing completion validator. The returned volume UIDs bind later admission
publication to the very volumes created here. This is input preparation, not a
successful research result or permission to start compute.

This source adds the publication boundary. Activation still needs the site's
custodian dispatcher, namespace policy and narrow create/read grants, retained
source approval, completion polling, seal promotion, and admission publication.
Worker admission waiting is opt-in; a source commit does not enable a live route.
