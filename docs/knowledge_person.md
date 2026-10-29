# Knowledge actions through the workspace

Some knowledge decisions belong to a person acting directly: marking a commons record baseline so every lab receives
it, reviewing a link the knowledge service proposed (such as a paper that names one of the lab's tools), and later
approving a grant. The knowledge service allows them only when the caller is
a person with no run behind them. The workspace lets a signed-in person take these actions: it checks the person's
Cloudflare Access sign-in as it does for every route, then forwards that same signed assertion to the knowledge
service beside a token of its own. The knowledge service verifies the assertion again, maps its subject to one of its
own person principals and acts as that person.

## Who can do what

| Caller | Result |
|---|---|
| A signed-in person the workspace knows | forwarded; the knowledge service decides and records the decision in that person's name |
| A service account, including the research lead's workloads | `knowledge_person_required`, never forwarded |
| A person the workspace does not know or has disabled | the workspace's own refusal, never forwarded |
| Any caller when the workspace has no knowledge configuration | `knowledge_not_configured` |

The workspace grants nothing here. It cannot name a person without that person's signed assertion, and the knowledge
service accepts a forwarded assertion only from a caller its own configuration names as a forwarder. The workspace
never mints, stores or logs an assertion, and it forwards nothing for a service account.

## Configuration

`--knowledge-person-config` (or `AGENT_SANDBOX_BROKER_KNOWLEDGE_PERSON_CONFIG`) names a private JSON file:

```json
{
  "schema": "enrichedlab.knowledge-person.v1",
  "service_url": "http://knowledge.research-knowledge.svc.cluster.local:8080/",
  "token_file": "/var/run/enrichedlab/knowledge-person/token",
  "commons_scope": "commons",
  "timeout_seconds": 8
}
```

`service_url` is https, or http only to a cluster Service name or loopback, with no user, query or fragment.
`token_file` is an absolute path to a projected ServiceAccount token for the knowledge service's audience, read each
time it is used; the knowledge service's auth configuration maps that ServiceAccount to the workspace forwarder
principal, and its person configuration names that principal as a forwarder (the knowledge service's
`docs/person_sign_in.md`).
`commons_scope` is the site's commons scope key, in the knowledge schema's form (`^[a-z0-9][a-z0-9._-]{0,62}$`). `timeout_seconds` is 1 to 30 (default 8). Without the file every
knowledge route answers `knowledge_not_configured`.

## Routes

| Method | Path | Use |
|---|---|---|
| GET | `/api/v1/knowledge/commons?kind=&after=` | one page (at most 500) of the commons records of one kind (`dataset` by default, or `paper`, `tool`, `object_collection`) that the person can see, each with its baseline mark, newest first; the service filters by the commons scope before its limit, so lab records never crowd commons records out. `next` continues the listing as `after`, and `complete` is true on the last page. Each paper carries `parts_waiting`: its active sections, figures and tables not yet marked while the paper is. The person's `role` in the commons (`member`, `curator`, `lead` or null) and `can_mark`, true only for a person acting directly who leads it |
| POST | `/api/v1/knowledge/resources/{id}/baseline` | `{"baseline": true}` marks the record baseline for every lab and `false` withdraws the mark; a paper's mark covers its sections, figures and tables as they stand. Answers the record as the service stored it with `parts_changed` |
| GET | `/api/v1/knowledge/commons/collections` | the commons collections by source prefix, such as `literature://papers/`: each kind's active and marked counts and the parts of marked papers that wait; `role` and `can_mark` as above |
| POST | `/api/v1/knowledge/commons/collections/baseline` | `{"prefix": "literature://papers/", "baseline": true}` marks every record of the collection, with each paper's parts, in the person's name; answers `records_changed` and `parts_changed` |
| GET | `/api/v1/knowledge/labs/{lab}/proposals` | the links proposed from the lab's records that the person can read, oldest first (at most 100): each link's kind, its two ends by kind and title, the names found and how many sections hold them; the person's `role` in the lab and `can_review`, true only for a person acting directly who curates or leads it |
| POST | `/api/v1/knowledge/relations/{id}/review` | `{"accept": true}` accepts a proposed link; `{"accept": false, "note": "…"}` rejects it with an optional note of at most 500 characters; answers `{"relation_id", "outcome"}` with `accepted` or `rejected` |

Each record carries only `resource_id`, `kind`, `scope`, `baseline`, `title`, `summary`, `data_class`, `rights`,
`lifecycle`, `source_uri` and `created_at`, each checked for its type, and a paper `parts_waiting`. The POST routes
need the workspace's `X-Agent-Sandbox-Intent: mutate` header, and the workspace sends the knowledge service's own change
header. `can_mark` only decides whether the workspace offers the mark; the knowledge service still decides each mark
when it is made.

A change that was sent but whose answer was lost, a server fault after it was sent, and a success answer out of shape
(about another record, with the other mark, or without its counts) all answer `knowledge_outcome_unknown`: the change
may have been made. The workspace then reloads what the service holds before offering the action again. Only a
connection that never opened answers `knowledge_unavailable`, since nothing was sent.

## Refusals

| Code | Status | Meaning |
|---|---|---|
| `knowledge_person_required` | 403 | the caller is not a signed-in person |
| `knowledge_not_configured` | 409 | the workspace has no knowledge configuration |
| `knowledge_kind_invalid` | 400 | the kind is not one a person can mark |
| `baseline_refused` | 403 | the person does not lead the commons, or the record is not in it |
| `baseline_not_active` | 409 | only an active record can be marked |
| `resource_not_found` | 404 | the person cannot see the record |
| `relation_not_found` | 404 | the link is not waiting, or the person does not curate or lead its lab |
| `relation_not_proposed` | 409 | someone already reviewed the link |
| `relation_review_refused` | 403 | the caller is not a person acting directly |
| `knowledge_note_invalid` | 400 | the note is longer than 500 characters or holds control characters |
| `person_not_enrolled` | 403 | the knowledge service has no principal for this sign-in |
| `person_assertion_invalid` | 403 | the knowledge service did not accept the sign-in |
| `person_keys_unavailable` | 503 | the knowledge service cannot read the sign-in keys now |
| `person_sign_in_stale` | 403 | a change needs a sign-in issued within the knowledge service's limit (an hour by default); sign out and in again |
| `knowledge_cursor_invalid` | 400 | `after` is not a `next` value the listing gave |
| `knowledge_collection_invalid`, `collection_invalid` | 400 | the prefix is not a scheme, `://`, an authority and a slash |
| `knowledge_unavailable` | 503 | the service could not be reached before anything was sent, or the workspace's own token could not be read |
| `knowledge_outcome_unknown` | 504 | a change was sent, but its answer was lost, was a server fault or was out of shape; it may have been made |
| `knowledge_invalid` | 502 | a read answered out of contract |
| `knowledge_unauthenticated`, `forwarder_not_allowed`, `forwarder_not_active`, `knowledge_refused` | 502 | the two services disagree about the workspace's own identity or role (including a forwarder principal the knowledge service holds disabled), or the service gave a code the workspace does not pass on |

A refusal from the service passes on only these fixed codes and the workspace's own messages, so an answer can never
place its own text in the workspace's response.

## Command line

```bash
agent-sandbox-remote knowledge commons --kind dataset
agent-sandbox-remote knowledge commons --kind paper --after <next>
agent-sandbox-remote knowledge baseline <resource-id>
agent-sandbox-remote knowledge baseline <resource-id> --withdraw
agent-sandbox-remote knowledge collections
agent-sandbox-remote knowledge mark-collection literature://papers/
agent-sandbox-remote knowledge mark-collection literature://papers/ --withdraw
agent-sandbox-remote knowledge links <lab>
agent-sandbox-remote knowledge review <link-id>
agent-sandbox-remote knowledge review <link-id> --reject --note "Describes sample handling, not this tool."
```

The client uses the person's own Access session, as for every other command.

## Checks

`api::knowledge` unit tests show that a service account, a missing change header, an unknown kind, a malformed cursor
or collection prefix, a workspace without the configuration and a person the workspace cannot look up never reach a
stand-in knowledge service, that answers keep only known codes and the listed fields, that a page with a lab record or a
malformed `next` is refused, and that a server fault after a change reads as an unknown outcome.
`scripts/qualify_remote_broker.sh knowledge-person` runs the positive path against isolated PostgreSQL: the enrolled
person's assertion and the workspace's token reach the stand-in, the commons scope filter, the change header, a refusal
passed on, a success out of shape and a server fault as unknown outcomes, the collections and a collection mark, and an
unenrolled person kept back. A second test in the same mode reads a lab's proposed links and records an acceptance, a
rejection, a refusal passed on and an acceptance the service did not show as reviewed (an unknown outcome).
`knowledge_person` unit tests cover the client's headers and configuration rules and show that a change whose answer is
lost has an unknown outcome while a read, or a connection that never opened, does not.
