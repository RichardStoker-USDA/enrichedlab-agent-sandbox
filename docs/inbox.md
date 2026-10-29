# What waits for each person

Migration 037 adds one reader and one record. `broker_inbox` computes, for the caller, the items
across their labs that ask for a person, each only for someone who may act on it:

| Kind | Item | Who sees it | Version |
|---|---|---|---|
| `review_request` | a project's newest run stopped for review, with the recorded reason: the methods check's words when it asked, else the lead's summary | people and service accounts who may steer or review the project | the run |
| `step_check` | a step of the current plan whose analysis the system could not confirm (`uncertain`) | who may steer | the plan-ledger revision that marked it |
| `analysis_review` | a finished analysis whose newest review by a person does not cover its current result files; a figure version is a drawing, not an analysis, and never waits (migration 040); a template analysis is an analysis and waits like any other (migration 058) | people who may review | the digest of its current result files |
| `paper_acceptance` | papers in a lab's library that no curator accepted, one item per lab with the count | the lab's leads and reviewers | the set of waiting papers |
| `note_attention` | a shared note the lead no longer reads because its review-by date passed or its evidence changed | the lab's leads and reviewers | its state and dates |

A person marks an item as seen with `broker_inbox_acknowledge`; the row in `broker_inbox_acknowledgements` names the
person, the item and its version, and cannot change. The same request again returns the same row, a second request for
the same item keeps the first time, and an item that is not the caller's now, or an older version, is refused
(`inbox_item_not_found`). A changed item has a new version and reads as unseen again. Service accounts read their items
and are refused a mark (`inbox_person_only`). `broker_project_acknowledgements` tells anyone who may read a project who
saw each of its review requests, by display name only, as reviews do.

Routes: `GET /api/v1/inbox`, `POST /api/v1/inbox/acknowledgements` (`kind`, `ref`, `version`, `request_id`) and
`GET /api/v1/projects/{project}/acknowledgements`. The API stays out of service until migration 037 and those grants
exist; no other role may call the readers or touch the table.

The workspace's "Waiting for you" place lists the items by kind in plain sentences (`clients/workspace/src/inbox.ts`),
unseen first, with the agent's own words labelled apart, a link to where each can be dealt with and "Mark as seen". The
top bar (the drawer on phones) counts unseen items; the home page's Waiting for you section counts the projects they
belong to and says so. A project's summary says who saw the agent's current request for review.

Nothing is sent outside the application. Notifications by mail or chat need a channel the site chooses and would carry no
research content.

PostgreSQL checks: `scripts/qualify_remote_broker.sh inbox`.
