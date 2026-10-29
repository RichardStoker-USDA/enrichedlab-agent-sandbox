# Native harness example

`native-events.jsonl` is a synthetic stream for a custom harness. It contains no host data or model endpoint. Ingest it
against an isolated controller config:

```bash
agent-sandbox provenance ingest example-run example-worker \
  --harness native \
  --input examples/provenance/native-events.jsonl \
  --provider local \
  --model synthetic-model \
  --engine custom-harness \
  --session example-session

agent-sandbox --output json provenance graph example-run
```

Use stable source IDs and parent IDs from the harness. The controller adds its own UUID, sequence, timestamp, tenant,
content digest, and hash-chain fields. Put data values, tool arguments, tool results, output, and provider-returned
reasoning in `restricted_content`. Keep bounded counts, digests, versions, and status in `metadata`.
