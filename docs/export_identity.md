# Ephemeral export identity

`broker::export_identity::issue_export_credentials` creates a fresh CA and one
server/client mTLS pair for a trusted `ExportScope`. It derives the server DNS
name from the scope service name, trusted namespace, and `RemoteExportConfig`
suffix. Callers cannot choose a separate certificate host.

The requested task and export windows must both be positive and their combined
lifetime must not exceed 24 hours. The returned public metadata contains only
the scope, derived server DNS name, client-leaf SHA-256 fingerprint, and expiry.

Private CA, server, and client bytes remain in the non-serializable
`IssuedExportCredentials` value. Only crate-private borrowed server and client
file views are available to the trusted Kubernetes publisher and broker client
stager. The issuer does not write files, contact Kubernetes, retain a CA key,
or authorize an exporter workload.
