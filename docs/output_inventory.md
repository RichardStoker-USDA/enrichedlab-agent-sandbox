# Output inventory

`agent-sandbox-output-inventory` measures only the files already named in one private operator-owned request. The request reader requires a private descriptor root, exact filename, and expected writer UID. Its mounted root is absolute, held through no-follow directory descriptors, and every selected file is opened as a single-link regular descriptor.

The request uses `sandbox.enrichedlab.org/output-inventory-request/v1` and contains a fixed mounted root, selected `relativePath`, `visibility`, `role`, `label`, `mediaType`, and `maxFiles`/`maxBytes`. The helper rejects links, nonregular files, traversal, duplicate paths, budget overflow, cancellation, and file metadata changes during the streaming SHA-256 measurement. It emits at most 1 MiB of `sandbox.enrichedlab.org/output-inventory/v1` JSON containing only selected metadata, measured byte counts, bare SHA-256 values, and a canonical digest of the approved selection. It never emits file bytes.

The helper has no task, principal, Pod, lease, terminal-record, Kubernetes, network, or export authority. A later trusted observer must bind its private stdout to the exact completed run and decide whether the resulting inventory may become a terminal export record. The helper does not make a PVC quiescent or prevent a writer from changing output after measurement.

`OutputInventoryRequest::validate_inventory` lets that observer prove the inventory has the request's exact schema, canonical selection digest, ordered paths, metadata, byte limits, and bare lowercase SHA-256 values. It does not bind runtime identity or grant output-PVC, helper, lease, or export authority. The terminal-export caller must verify those boundaries before it encodes the existing terminal record.
