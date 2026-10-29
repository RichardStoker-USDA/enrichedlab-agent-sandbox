# Research campaigns

A research campaign is a durable program over one lab. A person pins its goal, constraints, starting points, permitted methods, budgets, and stop rules. The broker then creates private projects and starts ordinary research runs for bounded questions. Every run keeps the existing method review, statement checks, lineage, receipts, task custody, and project history.

A campaign is not a long model session, a new execution sandbox, or a way around project and run checks. Its director wakes for one generation-fenced cycle, records its changes, starts only work that fits the saved scope, then releases the lease. A model-backed planner works on those records, code-owned rules, API, and director loop.

## Who can use one

The site catalog grants `research.discovery.autonomous` to an exact lowercase verified email and lab:

```yaml
capabilities:
  - email: research-lead@example.invalid
    group: example-lab
    capability: research.discovery.autonomous
```

The seed enables entries present in the catalog and marks missing entries revoked without deleting them. Unknown capabilities, unknown labs, duplicate entries, and campaign-reserved email addresses are refused. A grant works only while the principal is an enabled person with an enabled membership in that lab. Service accounts and campaign principals never hold it.

Only a current capability holder can create, see, start, resume, pause, or stop a campaign. Any holder for the lab can pause or stop one; these controls are not limited to the person who started it.

## Campaign identity and projects

Creation makes one internal principal with issuer `urn:enrichedlab:campaign`, subject `campaign:<campaign-id>`, and a reserved `campaign.invalid` address. Access assertions and service-account authentication both reject this identity. It cannot be invited, granted the autonomous capability, or added to another lab.

The principal has the `researcher` lab role. That is the smallest existing membership role that can own a project; ownership supplies contribute and steer authority only for its own projects. Each campaign project is `private`, is owned by that principal, and grants the person who started the campaign only `read`. People pause, stop, and steer the program through capability-scoped campaign controls, never by replacing a campaign project's research state. It is never lab-visible. A campaign result does not enter lab notes, past research, a shared registry, or any publication path until a person promotes it through a later reviewed workflow.

Each step pins its accepted project checkpoint. The coordinator reads that revision, and only the campaign principal can author a later checkpoint through the broker's fenced research-run functions. The database rejects an API checkpoint for a campaign project even after a starter loses the autonomous capability. It also rejects any internal checkpoint whose goal, constraints, or workspace differs from the step's initial campaign snapshot.

## Scope and budgets

The scope names one or more starting points, the combined immutable workspace references, and up to sixteen method profiles. The API resolves the starting points from the site-owned workspace catalog. The database checks that every profile is an enabled run method granted to the lab when the campaign is created, started, resumed, and whenever a step starts.

Budgets are hard database limits:

- wall time, at most fourteen days;
- total runs, at most five hundred;
- concurrent runs, at most four;
- turns in each run, at most sixty-four.

A claim, renewal, apply, or step start after wall-time expiry moves the campaign to `stopping`. A spent total-run budget also moves it to `stopping`; it takes precedence if the total and concurrent-run limits are both reached. A concurrent-run limit is temporary, so it refuses the step while leaving the campaign `running`. Both capacity decisions are successful function calls with `started: false`, a fixed `code`, and the resulting campaign `state`; lease, authority, scope, and invalid-input failures are errors. Generation fencing prevents an expired director from applying a proposal or starting work.

## Records and branches

Branches form a tree. Their direction and rationale are marked model text, and each branch records its state, fixed reason class, outcome counters, and consecutive steps without evidence. Steps record their purpose, bounded question, profiles, project and run identifiers, state, and an outcome containing only answer state, action, and counts. Research values and finding text do not enter a step outcome or campaign event.

Campaign memory holds bounded records of these kinds:

- `question` and `hypothesis` for open lines of work;
- `attempt` for every finished step and its outcome class;
- `finding` and `prospect` for evidence still inside the campaign;
- `lesson` for an attributable planning lesson;
- `request` for work or judgment outside the current path.

Relations are `supports`, `contradicts`, `tested_by`, `retry_of`, `derived_from`, `cites_evidence`, and `answers`. References may name only a step, run, project, task, researcher-visible artifact, or record that the broker resolves inside the same campaign. Planner text cannot carry a number that does not already occur in the person's goal or constraints; result values stay behind references.

## Director rules

The director classifies each completed run in code:

```bash
agent-sandbox-broker campaign-director \
  --database-dsn-file /run/secrets/campaign_director_dsn \
  --model-config-file /run/secrets/model.json \
  --tenant example-site \
  --worker-id campaign-director-a \
  --planner-max-input-bytes 65536 \
  --max-parallel 4
```

`--once` claims and advances at most one campaign. The normal loop polls at a bounded interval and processes up to the
configured number of campaigns in parallel.

## Model planner

The director uses the same operator-owned model route and private `model.json` as the research coordinator. Its fixed planner instructions are separate and compact. The planner can propose bounded run questions, open and close branches, and add hypotheses, lessons, prospects, and open questions. It cannot start work itself. Code validates its proposal against the campaign snapshot, remaining budgets, free concurrency, branch state, duplicate-question history, references, and permitted methods before the database writes anything.

The context has a fixed order: goal and constraints; scoped methods with their names, short descriptions, and starting-point keys; budgets left; rule notices; branch tree and outcome counts; recent attempts; open records grouped by kind; and requests waiting on a person. The operator's `--planner-max-input-bytes` ceiling covers both the fixed instructions and serialized context and must not exceed `max_input_bytes` in `model.json`. The database reports records omitted by its bounded snapshot, and the compiler adds that count when it removes the oldest attempts and records for space. Rule notices are read in full and are never cut. It never cuts the goal, constraints, method scope, or rule notices. If the required sections do not fit, the proposal cycle is refused.

Planner text treats every supplied field as data, never as instructions. It may use only supplied identifiers and method keys. It must not repeat a failed question for the same reason, and a branch waiting for `change_approach` needs a changed question and method set. It should state assumptions and prefer controls, replicates, and an expected direction when the scope supports them. A number may occur in planner-written text only when the same number occurs in the person's goal or constraints. Result values remain in identifier references.

Replies are one strict JSON object with all required arrays and a summary. Unknown fields, wrappers, missing fields, unresolved references, and invalid proposal bounds are refused. One structurally unreadable reply gets one retry with a fixed correction note. A second unreadable reply counts as a refused planner cycle. A valid but empty proposal is `planner_no_progress` and counts toward the same refusal threshold.

Every call appends its start before network I/O, including the configured model, lease generation, input size, and SHA-256 address of the exact context. A linked immutable terminal row later adds the fixed outcome, output size, reported model, and, when the shared model client returned readable content, its reply digest. A pause or stop can fence the old generation from campaign actions without losing the in-flight start; that generation may only finish its already-recorded call. The addressed texts are immutable and tenant-scoped. Current campaign readers can page metadata at `GET /api/v1/campaigns/{campaign}/planner-calls` and read a digest named by one of those calls at `GET /api/v1/campaigns/{campaign}/planner-texts/{sha256}`. An in-flight call has null terminal fields. Losing the lab capability also loses these reads. Transport failures and replies the shared model client cannot parse finish with a fixed outcome but no reply digest because unvalidated response text is not admitted to text custody.

| Run result | Outcome |
|---|---|
| answered or partly answered with a finding | `evidence` |
| not answered | `no_answer` |
| review required | `needs_person` |
| paused, or no answer at the turn limit | `stalled` |
| failed, or refused to its limit | `failed` |
| cancelled | `cancelled` |

It writes one attempt record for every result, one identifier-bound finding record for each counted finding, and a request when a person is needed. A request never blocks other branches.

The configurable rules are evaluated in code; their counter and state changes are applied atomically at the
generation-fenced database boundary:

1. The same non-evidence class twice makes the branch wait for a changed question and profile set. Repeating the failed question or profile set is refused.
2. A branch with no evidence over its window, four steps by default, concludes as `no_new_evidence`.
3. A campaign with no evidence over its window, twelve steps by default, stops.
4. Any exhausted budget stops the campaign.
5. Three consecutive cycles in which every proposal is refused pause it as `planner_refused`.
6. `needs_person` creates a request and work continues elsewhere.

Every firing is an append-only `rule_fired` event with only its rule, bounded counts, threshold, and identifiers.
Concluded and abandoned branches accept no new step or later rule transition. Repeating a close for an already terminal branch is an event-recorded no-op, so a run that finishes after its branch closes can still be reconciled without reopening or changing the branch.

## Pause, stop, and kill switch

Pause and stop prevent new steps immediately. Existing research runs drain instead of being cancelled. Draining preserves their normal receipts, complete attempt history, and any evidence already in flight; cancellation would turn a policy stop into missing scientific custody. A paused research run is not a finished campaign step while any queued, leased, starting, running, or cancelling task remains in its project. Lifecycle finalization checks those tasks independently, including if an older director already marked the step finished. After the last run is reconciled and its tasks settle, pause becomes `paused`, while stop becomes `stopped` and disables the campaign principal and removes its membership.

The migration owner can stop every running or paused campaign in one tenant:

```sql
SELECT enrichedlab_agent.broker_stop_all_campaigns(<tenant>, <fixed_reason_code>);
```

No runtime role can execute that function. The director observes the fenced `stopping` state before it can start another step.

## Security boundary

The campaign director has a function-only PostgreSQL role. Provisioning and startup both refuse a login with role memberships, privileged role attributes, object ownership, direct relation, sequence, column, or extra schema authority, or any effective privilege over broker relations or functions outside the campaign contract. It has no Kubernetes, OpenShell, SSH, GitOps, registry, cluster, or authoritative-data credential. Its only network routes are the broker database and the operator-owned model route. Inputs remain read-only references; outputs remain in private campaign projects. Nothing is promoted, shared, sent, or published without a person.

## Planned

- Automatic re-execution and claim checks, with disagreements becoming recorded campaign work.
- Export of the structured memory to the lab knowledge graph, and the ranked digest, branch view, pending requests, budgets, and kill switch in the workspace.

Independent replication, campaign-written tools, novelty scoring, held-out checks, and broader rollout remain later work.
