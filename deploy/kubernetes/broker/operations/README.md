# Broker database operations

These templates are inputs to a private GitOps overlay. They contain no credentials, identities, or deployable image
digest. Do not apply them from this public directory unchanged.

Run the operations in this order:

1. Publish and scan the controller image, then replace the zero digest in each private overlay.
2. Provision distinct migration, API, worker, and trusted-custodian database roles in the database platform.
3. Reconcile `migrate` as a one-shot Flux child and wait for `agent-broker-migrate-002` to complete.
4. Apply `migrations/postgres/grant_broker_runtime.psql` as the migration owner for the API, worker, and optional custodian roles.
5. Store the site catalog as an encrypted Secret named `agent-broker-catalog` with key `catalog.yaml`.
6. Reconcile `seed` as a one-shot Flux child and wait for `agent-broker-seed-001` to complete.
7. Remove the one-shot children from the active Flux graph before activating the API and worker.

Both Jobs read the migration DSN from the `database.dsn` key in `agent-broker-migration-database`. The running API,
worker, and custodian use different Secrets and database roles. Never mount the migration Secret into a long-running Pod.

Change a Job name when its immutable template changes. Flux dependencies should express the order. Do not put two
same-name Jobs or the source and successor operation in the graph at once.
