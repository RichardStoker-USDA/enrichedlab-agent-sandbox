# Platform reviewed-tool adapter

The core broker may submit a reviewed compute job only through the platform job broker POST /v1/jobs. It does not import or seed the platform catalog, invoke run-reviewed-tool, or turn a direct Sandbox command into a tool claim.

A trusted server-side identity issuer supplies separate single-use platform operation JWS values for GET /v1/catalog and POST /v1/jobs. It uses the adapter's crate-private serialized request bytes when minting the submit proof, which binds those exact bytes and the server-issued request/conversation IDs. The adapter reads the projected ServiceAccount token from an operator-owned file for each request. Neither proof is accepted from an agent request or recorded in the core database.

Before submission, the adapter reads the authoritative platform catalog with its own operation proof. deployable:false, any release blocker, an unqualified image, a missing digest image reference, or a non-reviewed fixed runner yields platform_tool_catalog_unavailable. The current platform catalog remains unavailable, so this candidate does not wire worker execution.

After a 202 Accepted, broker_bind_platform_tool_job records the exact current task, attempt, lease generation, principal, request/conversation IDs, tool/catalog/input identities, and platform job ID. It allows only an identical retry. A lost lease writes no receipt; an ambiguous submit needs fresh server-issued operation proofs for the same request and cannot adopt a different job.

The binding is not scientific provenance. Start or terminal scientific events require a later retained output and validated platform result receipt.

This source batch does not wire a worker path. Activation still needs an identity issuer able to mint both operation proofs, a deployable platform catalog, and a durable pre-submit intent plus platform accepted-job recovery that fences the active lease across an uncertain network response. The adapter deliberately has no blind retry or job adoption while that boundary is absent.
