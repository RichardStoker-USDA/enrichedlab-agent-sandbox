# Contract descriptors

`enrichedlab_evidence_v1.binpb` is the compiled descriptor set of the `enrichedlab.evidence.v1` Protocol Buffers
package, copied from the EnrichedLab contracts repository (`descriptors/evidence_v1.binpb`) at the commit that added
`EvidenceAppendService`. `build.rs` generates the gRPC server and client from it, so the build needs no `protoc`.
Replace it with a newer copy from that repository; never edit it here.
