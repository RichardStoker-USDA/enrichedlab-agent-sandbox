# Export client credential staging

`broker::export_client_staging::stage_export_client` writes only the CA and
combined client certificate/key bundle issued for one trusted export scope. It
derives the directory name from `IssuedExportCredentials` metadata and uses the
trusted `RemoteExportConfig.credential_root`; no task request supplies a path.

The process effective UID, configured writer UID, and held credential root must
agree. The root and scope directory are mode `0700`; `ca.pem` and `client.pem`
are regular, single-linked mode-`0600` files. Descriptor-held reads verify an
existing bundle byte-for-byte before an identical retry is accepted. Different,
missing, extra, or unsafe existing entries fail without replacement.

Creation happens in an owned temporary directory. Each file and directory is
synced before an atomic no-replace directory publication. A failure removes the
temporary directory. The returned receipt includes only scope, client leaf
fingerprint, and expiry. It has no certificate or key bytes.

`export_client_scope_exists` supports the terminal preparation wait. It opens the
same private root and exact task/attempt/lease child through held no-follow
descriptors. Only that child's ENOENT is pending. An existing scope must be a
private directory; the existing client constructor then validates every file and
certificate. The probe never reads or returns key bytes and does not create,
repair, adopt or replace a bundle.
