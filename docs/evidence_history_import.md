# Importing the broker's history into the research record

Migration 078 adds the functions an operator uses once, as the migration owner, to give the research record (migrations
075 to 077) the broker's history from before the record existed. Each row of the 55 append-guarded broker tables
becomes one record of event type `import.<table>` through `evidence.append`, so its chain, receipt and immutability
are those of every other record. The producer is `broker:history`; it can append only as the migration owner and is
disabled when the import is sealed.

## What a record holds

The row's primary key, the SHA-256 of the whole row as these functions render it (time zone UTC, bytea as hex), and the
row's structural columns: uuids, numbers, booleans and times, and text named as an identifier, enumeration, digest,
reference or version whose value is a single token of at most 256 bytes. A text column named as a time is copied only
when it is a valid RFC 3339 time. Every other column is recorded by SHA-256, size, encoding and class and named in
`normalised`; its bytes stay in the broker table. Model prompts, answers, reasoning, generated code and model SQL are
class `model-text` and never enter the record as text.

A record goes to the row's run, project or lab. Every scope identifier the row carries must resolve and agree; a row
whose identifiers do not is not imported and is reported as `unresolved`, with the digest of its key. A row with no
scope identifier goes standalone in a lab the operator names.

## Running it

1. `evidence.import_begin(tenant)` creates the producer.
2. `evidence.import_seal(tenant, lab)` does the import in one transaction. It takes share locks on the 55 tables and on
   the relations scopes and times are read from (broker writes to them wait until it ends), refuses while a research
   run is queued, running or waiting, imports every table, checks every record against its row as re-derived under
   those locks (`evidence.import_verify`: payload, lab, scope, run, source time), and only then removes the producer's
   grants and disables it. A failed check appends nothing and seals nothing.

`evidence.import_history(tenant, table, lab, offset, limit)` appends one batch of one table in key order; the seal uses
it, and so do rehearsals and the qualification. Repeating a batch appends nothing new.

The qualification fixture `tests/fixtures/broker_evidence_import_qualification.sql` runs in
`scripts/qualify_remote_broker.sh evidence`.
