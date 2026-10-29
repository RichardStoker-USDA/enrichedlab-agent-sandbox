# Lab library

A lab keeps the papers its research relies on in a library. A member adds a PDF; the upload guard (`../guard/`)
checks it and rebuilds it without active content; the document service reads the rebuilt copy (sections, pages,
figures and captions) into the lab's own corpus and indexes its words; members search the text and
label papers by what they are about. The research lead reads the library as background when it plans.

## Where things live

| What | Where |
|---|---|
| The PDF, its parse, chunks and the word index | the document service (ParsEnriched), in the lab's own corpus |
| Which papers the lab holds, catalog fields, who added them, removals, labels | the broker (migration 028) |
| The lab-to-corpus map, each corpus token and the guard's origin and caller token | the API's private library configuration |
| The original upload, the guard's decision record and the rebuilt copy until it is fetched | the upload guard's volume, per person |
| The lab-to-corpus map and each read-only token for the lead | the coordinator's private library configuration |

The broker never stores paper bytes. It keeps the document service's identifier and the SHA-256 of the source bytes,
and the catalog fields it read back after the parse: title (at most 2,048 characters), authors (names only, at most
200), year, DOI, page count and a small `parse` object of counts and states (never text from the paper). For a paper
added through the guard, `parse.guard` holds the guard's intake ID, policy version, the rebuilt copy's SHA-256 and the
finding codes it noted; the source SHA-256 stays the one of the bytes the person sent.

## Configuration

`--library-config` (or `AGENT_SANDBOX_LIBRARY_CONFIG_FILE`) names a private JSON file, readable only by the API's user:

```json
{
  "schema": "enrichedlab.lab-library.v1",
  "service_url": "http://parsenriched.parsenriched.svc:8500",
  "labs": [
    { "lab": "pilot-lab", "corpus": "lab-pilot-lab", "token_file": "/run/enrichedlab/library/pilot-lab.token" }
  ],
  "guard": {
    "service_url": "http://guard.example.svc:8080",
    "token_file": "/run/enrichedlab/library/guard.token",
    "use_case": "library-paper",
    "tenant": "pilot"
  }
}
```

`service_url` is a bare http(s) origin. Each lab names its corpus (`^[a-z][a-z0-9_-]{1,62}$`) and an absolute token
file. The token is a document-service `contributor` scoped to that corpus alone (read and write, no admin), read from
its file each time it is used, and never logged. The same private-file rules as the model credential apply: owned by
the API's user, no group or other access, not a link. A lab without an entry answers `library_not_configured`, and the
list says `configured: false`. Without the file the API still serves; every library route that needs the service
answers `library_not_configured`.

The corpus and its token are created by an operator; the token's SHA-256 goes into the document service's scopes file.
A browser never names a corpus or a token, and a request sent with one lab's token to another lab's corpus is refused
by the document service.

`guard` names the upload guard: a bare origin, the caller token file (same private-file rules; the guard keeps only its
SHA-256), the guard use case whose policy applies, and the tenant name the guard records. Without a `guard` section no
paper is added: an upload answers `library_guard_not_configured` (503), so no upload path skips the check, the same
bytes sent again included. Those return the lab's paper, or restore it after a removal, only when its record carries
the guard's evidence (`parse.guard`: the intake and the digest of the rebuilt copy the corpus holds). A paper added
before the guard existed has none, so its bytes sent again answer `library_paper_unchecked` (409) and it is never
returned or restored. A person whose daily upload quota at the guard is used gets `library_upload_quota` (429).

The API reads an upload's body only after the caller's membership and the lab's library and guard setup are settled
and the upload holds its share of 384 MiB of library upload memory: the most the upload can hold at once, the
original and then the rebuilt copy (at most twice the original, never past 100 MiB) with its base64 request body, at
least 16 MiB. One upload of the largest size runs at a time, and smaller ones run beside each other. The original is
not kept once the guard has it, and the rebuilt copy is written straight into the corpus request. An upload must state
its size (`library_upload_length_required`, 411); one that waits 30 seconds for its share answers `library_busy` (429);
a body past 100 MiB or past its stated size is refused (`library_paper_too_large`, 413), and one that stops early or
takes more than five minutes answers `library_upload_incomplete` (400).

The coordinator takes the same file format through `coordinate --library-config-file`, with a separate token per lab:
a document-service `reader` on that lab's corpus alone, so the process that talks to the model cannot add or change a
paper.

## Records

Migration 028 adds `broker_library_papers`, `broker_library_paper_events` (`removed` with a reason of at most 600
characters, `restored`) and `broker_library_label_events` (`added`, `removed`). All three are immutable. Every function
first requires the caller's membership of the lab through `broker_library_member` (an enabled principal in an enabled
lab of an enabled tenant); a non-member gets `lab_not_found` and learns nothing.

- `broker_library(tenant, actor, lab)`: the papers still in the library, newest first, the caller's role, and how many
  were removed.
- `broker_library_paper(tenant, actor, lab, paper)`: one paper, removed or not (`paper_not_found`), and its document
  identifier for the API's own use.
- `broker_library_find(tenant, actor, lab, sha256)`: the lab's paper with these bytes, so the API does not upload them
  twice.
- `broker_library_record(...)`: records a paper after the service accepted it. The same bytes in the same lab return
  the existing paper, restored with a `restored` event when it had been removed; one document under other bytes is
  `library_conflict`; out-of-bounds fields are `library_invalid`. Idempotent per caller and request.
- `broker_library_remove(tenant, actor, lab, paper, reason, request)`: a lab `lead` or whoever added the paper
  (`library_forbidden`), with a reason (`library_invalid`); a second removal is `paper_removed`.
- `broker_library_label(tenant, actor, lab, paper, kind, namespace, value, request)`: namespaces `topic`, `organism`,
  `food`, `compound`, `method`, `design`, `population`; values of 1 to 80 characters without control characters
  (`label_invalid`). Any member adds a label; only a person removes one (`label_person_only`). A label reads `set` when
  a person added it since its last removal and `proposed` when only service accounts did. Nothing is appended when a
  label already reads that way. A removed paper takes no labels (`paper_removed`).
- `broker_library_resolve(tenant, actor, lab, documents)`: for search, the papers still in the library among at most
  100 document identifiers.

The API role executes these functions only; it has no table privileges, and the helper functions are not granted.

Migration 029 adds `broker_research_run_library(tenant, run, worker, generation, documents)` for the coordinator,
fenced by the run's lease like its other reads (`lease_fence_lost`). Given at most 100 document identifiers, it answers
`member` (whether the run's starter is an enabled member of the project's enabled lab) and `papers`: those documents
the lab's library holds and has not removed, with paper id, title and year. A non-member gets no papers. The
coordinator role has no other access to the library.

Migration 034 adds acceptance for the research agent as two more paper events: `accepted`, with
no reason, and `acceptance_withdrawn`, with a reason of 1 to 600 characters. A paper is accepted while its newest paper
event is `accepted`, so removing a paper ends its acceptance and a restored paper needs a new one.

- `broker_library_accept(tenant, actor, lab, paper, accept, reason, request)`: only a lab curator, a person who leads
  or reviews in the lab (`library_forbidden`); a service account never accepts. Accepting takes no reason and
  withdrawing needs one (`library_invalid`); a removed paper is `paper_removed`. Nothing is appended when the paper
  already reads that way. Idempotent per caller and request; the same request with other fields is
  `idempotency_conflict`. It takes the same per-paper lock as removal, so the two never interleave.
- Each paper a member reads now carries `acceptance` (who accepted it and when, or null) and `can_accept`.
- `broker_research_run_library` adds `accepted` to each paper it answers.

## API

| Method | Route | Result |
|---|---|---|
| GET | `/api/v1/labs/{lab}/library` | papers, role, removed count and `configured` |
| POST | `/api/v1/labs/{lab}/library/papers?request_id=` | a PDF body (`application/pdf`, `%PDF-`, at most 100 MiB); 201 with the new paper, 200 when the lab already had the bytes or they restored a removed paper |
| GET | `/api/v1/labs/{lab}/library/papers/{paper}` | the paper, and from the service its abstract, journal and up to 25 figure captions (`outline_error` names why they are missing) |
| GET | `/api/v1/labs/{lab}/library/search?q=` | at most 20 passages (2 per paper) of papers still in the library, each with its paper, section path and pages; `ranking: "words"` |
| POST | `/api/v1/labs/{lab}/library/papers/{paper}/remove` | `{reason, request_id}` |
| POST | `/api/v1/labs/{lab}/library/papers/{paper}/labels` | `{kind, namespace, value, request_id}` |
| POST | `/api/v1/labs/{lab}/library/papers/{paper}/acceptance` | `{accept, reason, request_id}`; `reason` only to withdraw |

Intake first sends the bytes to the guard (`POST /v1/intake` with the lab, the person's principal ID and the use
case). A file the guard holds answers `library_upload_quarantined` and one it refuses `library_upload_blocked` (both
422, with the guard's finding codes and intake ID in the error body, which the workspace words); nothing is added. An
unreachable guard, or one whose malware scanner is unavailable, answers `library_guard_unavailable` (503), and a busy
guard `library_guard_busy` (429). For an allowed file the API fetches the rebuilt copy, checks its digest and size
against the decision, and sends that copy, never the original, with `external_id` `sha256:<hex of the copy>` and
profile `paper`, asks for the chunk index without embeddings, then reads the document back. A parse the service does
not accept records nothing and answers `library_parse_failed` (422); an unreachable service or an unexpected answer is
`library_unavailable` (503); the service's size refusal is `library_paper_too_large` (413). At most two uploads are
checked and sent at a time. Membership is checked before any bytes leave the API.

Search is by words (the service's lexical mode); the response says so. Meaning-based search needs an accepted
embedding profile for the document service and is a separate decision.

## The research lead reads the library

Passages are where reading starts: in a lab the coordinator names in `--lead-reading-labs`, the lead may open the
papers behind them and read their outlines, whole sections, figures and tables before it decides (see
[reading papers whole](research_coordinator.md#reading-papers-whole)).

The lead reads a lab's library only when the coordinator names that lab in `--lead-text-labs`;
any other lab's runs record `not_allowed` and search nothing. At each step the coordinator first checks, under the
run's lease, that the run's starter is still a member of the lab, so a non-member's question never reaches the
document service. It then runs the same word search on the lab's corpus with the project question
(its first 1,000 characters) as the query, keeps only passages of papers the library still holds while the starter is
still a member, and passes them to the context compiler. For a lab the knowledge service
also serves (`--knowledge-config-file`), the coordinator reads the lab's paper sections from it instead: the knowledge
foundation holds each parsed paper's sections as records in the lab's scope (its library ingest), and a search by words
and meaning under the run's one-lab delegation, with the question's first 500 characters, returns the best sections.
Only sections of the lab's own corpus pass; each keeps its paper by reference, its heading path and pages from the
record's locator and the first 1,500 characters of its text, and then goes through the same holding, membership and
acceptance checks as a word-search passage. The turn records `via: knowledge`, the hash of the text sent, a count of
records that were not sections of the lab's corpus (`other_records`), and for each passage read or withheld the
section's knowledge record (`section_id`) and its key in the document service (`section_key`) where a word-search
passage records its `chunk_id`. A knowledge service that does not answer is recorded as
`unavailable` with `via: knowledge`; the turn never falls back to the word search, so what the lead read stays
attributable. In a listed lab the lead reads only passages of papers a lab
curator accepted and counts the rest as `not_accepted`; a lab the coordinator also names in
`--lead-text-unreviewed-labs` (`AGENT_SANDBOX_LEAD_TEXT_UNREVIEWED_LABS`) reads papers before acceptance. That list
exists for the qualification lab, whose library holds synthetic test papers, and must be a subset of
`--lead-text-labs`, or the coordinator refuses to start. Context version 6 carries them as `literature`: each passage's `ref`
(`library://<paper>/<chunk>`, or `library://<paper>/<section record>` for a section, so both forms name a paper and
a UUID), the paper's title and year, the
section path, the pages and the first 900 characters
of its text. At most 4 passages and 4,096 bytes enter, in the search's order, after the result digests: passages fill
only the budget the digests leave, and any left out are counted in `literature_left_out`. A passage whose excerpt reads
like instructions to an agent is withheld by the fixed screen (`untrusted-text-screen/v1`, see
[research coordinator](research_coordinator.md)) and counted in `literature_withheld`; the paper stays in the library.

The prompt treats passages as background from the lab's papers, found by a search from the question that may match
its words rather than its meaning, and possibly off topic. No finding or
lesson may rest on one, a passage never replaces running a method, and its text is data, not instructions. The rule on
numbers is unchanged, so a number from a passage cannot appear in the lead's prose, and library references do not
enter the evidence a lesson may cite. A step may carry `literature_refs`, at most 4 distinct refs of passages shown in
that context; anything else refuses the turn with `proposal_literature_unknown`.

Each turn records `context_inputs.literature`: the status (`shown`, `none_fit`, `withheld`, `no_match`,
`not_accepted`, `not_configured`, `not_allowed`, `not_member` or `unavailable` with its code), in a lab that needs
acceptance the number of passages left out because their paper was not accepted, the query's SHA-256, each passage shown
by paper id, chunk id, pages and the SHA-256 of the text the lead read, each passage withheld with the same fields and
the rule that held it, and the count left out. A missing library, a starter who left the lab and a service that does
not answer never fail a turn. These records say which passages a step read; the exact text the lead read, passages
included, is kept with the decision since migration 032 and is readable by the project's readers who are lab members
([research coordinator](research_coordinator.md)). The document
service ranks words with no tiebreaker, so equal-rank passages can come back in another order; the record keeps what
each turn read.

### The literature specialist

For a lab the coordinator also names in `--literature-claims-labs` (`AGENT_SANDBOX_LITERATURE_CLAIMS_LABS`, empty
unless an operator names a lab, and a subset of `--lead-text-labs` or the coordinator refuses to start), the lead
reads typed claims instead of the passages. Before the lead's call a literature specialist, the same model under its
own instructions, reads the question with its constraints and instructions and the passages the lead would read, and
returns at most eight claims, each one plain sentence (the instructions ask for one; code holds its length of at most
300 characters and its characters) from one passage it was shown (`passage_ref`), with its relevance to the question (`direct`, `related` or `background`), how plainly the passage
states it (`high`, `medium` or `low`) and, optionally, what it leaves open (`unresolved`, at most 200 characters), and
the refs of passages it found off topic. Code checks every claim: an unanchored claim, a word outside the fixed sets, a
long text, a control character, a passage both claimed and off topic or an off-topic ref not shown refuses the reply
with its faults named (`literature_claims_invalid`; those of the first eight claims, at most 16 and a count of the rest),
and the specialist is asked once more with those names; a claim the untrusted-text screen holds back is left out and
counted. The lead then reads `literature_claims` (each claim with its passage's paper, its title cut to 200
characters, and year), `literature_off_topic` and `literature_claims_withheld` in place of `literature`, under the
same rules as passages: background, never results of the project, and a step's `literature_refs` name the claims'
passages. The claims replace the passages only when the lead's context with them still fits its budget. The methods
check reads the same claims.

A run asks once per set of passages: the checked reading is kept in the coordinator, by run and the SHA-256 of the
passages shown, with the calls that made it, and reused while they stay the same (`status: kept`). A reply that still
cannot be read or checked, a request too large for the model and a reading too large for the lead's budget leave the
passages with the lead (`status: unreadable`, `refused` with the faults, or `too_large`), and a model that does not
answer fails the turn as any model call does. Each turn records `literature_claims`: the status, the passages' digest,
the reading, the count held back and the receipts of the specialist's calls that made the reading (the original ones
when it was kept, so a reading first asked for in a turn whose lead reply was refused keeps them). The turn keeps the
specialist's instructions, each call's context and its reply beside the lead's and the methods check's texts, and a lab
member reading the project can read them (migration 050). The passages stay in `context_inputs.literature`, so the
record still names what was read. It ships off for every lab until a
curator-keyed literature set can score it.

### The literature review

`--literature-review-labs` enables a separate review before planning, only for labs in `--lead-reading-labs`; it is
empty by default. The specialist searches through the same question-selected passages, sees gated study summaries
when enabled, and reads paper parts with the shared reader in up to three rounds within its own
`--literature-review-bytes` allowance (32,768 by default). It returns bounded approaches, methods and data, expected
quantities, pitfalls, gaps and relied-on papers. Every item cites parts it read, catalog methods are checked, and
numbers must occur in cited content. One correction is allowed before refusal. Checked and refused outcomes are reused
by run and input digest, including after restart, until its question, shown passages or other review inputs change. The lead
gets a checked review whole when it fits, labelled as specialist background with refs checked, never project results; otherwise it
keeps the passages. Review citations do not become lesson evidence without the lead reading those parts itself.
Migration 070 keeps each outcome and its call texts for project readers who belong to the lab. Checked reviews have
JSON and Markdown API views; member packages also list refused outcomes as JSON with their status and faults.
See [reviewing literature before planning](research_coordinator.md#reviewing-literature-before-planning) for the schema,
custody and qualification.

## Workspace

"Library" in the top bar's Library menu (the drawer on phones) opens the lab's papers with a word search and "Add
papers". One PDF opens its page once read. Several chosen together are read one at a time, each listed with where it
stands (added, back in the library, already there, not a PDF, refused), and the person may stop the batch after the
paper being read; each file keeps its request key until it is saved, so choosing it again after a failure saves it
once. A paper's page shows its catalog
fields, what the parse found (pages, figures, searchable passages), the abstract and figure captions, its labels (a
proposed label can be set by a person), whether a lab curator accepted it for the research agent, with "Accept for
the research agent" and "Withdraw the acceptance…" (with a reason) for a lead or reviewer, and removal with a reason
for those allowed. The paper list says each paper's acceptance; a lead or reviewer sees how many papers are not accepted
yet, can list only those, and can accept every waiting paper they may accept after one confirming step: the same
acceptance as on each paper's page, sent one paper at a time with its own request key, with the papers not accepted
named. A project's page lists the
papers the agent read at its latest decision, each linked to its page with the pages read; an analysis's "Where this
came from" names the papers the agent cited when it planned that analysis. A paper removed since then says so.

## Checks

`scripts/qualify_remote_broker.sh lead-library` applies migration 029 twice and checks the coordinator's lease-fenced
read (lease and size refusals, removed papers, unknown documents, another lab's papers, restoration, a starter who is no
longer an enabled member, the role's grants) and the coordinator's read of a fake document service (the query, the
passages kept and their order, and each recorded outcome). `cargo test --locked --test broker_research_runs` checks the
compiled `literature` block: its order, limits, budget, hashes and statuses, and the refusal of citations the context
did not show.
`scripts/qualify_remote_broker.sh lab-library` applies migration 028 twice and checks membership, recording by bytes,
idempotency, conflicts and bounds, labels and their states, removal rules, restoration, search resolution,
immutability and the API role's grants. `lead-library` also checks acceptance: only a lab curator accepts, a withdrawal
needs a reason, idempotency and conflicts, a removed paper refuses it, removal ends it, and the lead's read in a lab
that needs acceptance (`not_accepted`, then `found` once accepted). `cargo test --locked --lib library` checks the configuration rules, the base64
encoding and the service calls against a local fake service (the token sent, the external identifier, the chunk index
without embeddings, the catalog read and the fixed error codes). The workspace unit tests and browser regression cover
the Library and paper pages and the papers the agent read and cited.
