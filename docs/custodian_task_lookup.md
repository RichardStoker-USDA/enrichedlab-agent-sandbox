# Custodian task lookup

`Store::custody_task(task, worker, attempt, generation)` is a read-only recovery
lookup for the trusted input custodian. It invokes
`enrichedlab_agent.broker_custody_task` with the Store tenant and returns a
`ClaimedTask` only when the task belongs to that worker's exact current lease,
has the matching attempt and generation, is unexpired, and has not been
cancelled or finished. It neither claims, renews, starts, requeues, nor updates
a task.

The SQL function returns the persisted profile snapshot and broker identities
needed for the fixed custodian path. Its `objective` result is always an empty
string. The caller must continue to use the existing lease fence and source
validation; a successful lookup is not a lock or permission to create compute.

Migration `005_broker_custody_task.sql` revokes `PUBLIC` execute. A site that
wires this path must explicitly grant only its worker runtime role:

```sql
GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_custody_task(
  text, uuid, text, integer, bigint
) TO worker_runtime_role;
```

That grant, any credentials, and runtime wiring remain site-owned and are not
part of this reusable source change.

## Terminal custody lookup

`Store::terminal_custody_task(task, worker, attempt, generation)` is the
matching read-only lookup for trusted output custody after a runtime terminal
observation. It returns the persisted non-content task and profile metadata
only for that worker's exact, unexpired `running` lease with desired status
`run`. Its `objective` result is always empty and it cannot claim, renew,
start, requeue, finish, or otherwise update a task.

Migration `006_broker_terminal_custody_task.sql` also revokes `PUBLIC` execute.
A site must grant its selected terminal custodian role explicitly, without
adding that grant to reusable source:

```sql
GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_terminal_custody_task(
  text, uuid, text, integer, bigint
) TO terminal_custodian_role;
```
