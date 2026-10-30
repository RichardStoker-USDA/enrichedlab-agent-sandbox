# Durable research coordination

The Rust broker's `coordinate` process advances project work independently of a browser session or a tool sandbox.
It uses a separate PostgreSQL login limited to the functions `migrations/postgres/grant_broker_coordinator.psql`
grants. It has no shell tool, Kubernetes client, input-data mount or permission to grant itself a project. For a lab
named in `--lead-text-labs` (`AGENT_SANDBOX_LEAD_TEXT_LABS`, empty unless the operator names a lab) it reads the lab's
shared notes and, with a library configured, the lab's papers through a read-only document-service token
(`--library-config-file`, [lab library](lab_library.md)); any other lab's runs read neither. Of a
listed lab's papers the lead reads only those a person in the lab accepted, unless the lab is also named in
`--lead-text-unreviewed-labs`. A researcher starts a run through the authenticated API and chooses
an allowed profile subset, maximum turns and concurrent tasks. The database rechecks that person's current project
and catalog permissions on claim, lease renewal and commit. This is service execution under a recorded human
delegation, not a separate autonomous identity with its own wider authority.

The accepted flow is implemented as follows; site deployment and model behavior require their own live receipts:

1. Claim one eligible run with a timed, generation-fenced lease. Other coordinators skip the locked row. Since
   migration 046 the claim is fair: it takes a run from the research group with the fewest runs holding a live
   lease, then from the person with the fewest, then the oldest, so one lab or one person starting many runs
   cannot hold every slot while others wait; a lab alone still gets every slot. Each two minutes a run has been
   ready counts as one fewer lease for its lab, so a run the fair order keeps passing over (a person with leases
   in other labs, say) is still claimed within bounded time. Claims in a tenant run one at a time under a
   transaction advisory lock, so each counts the lease the previous one took; a claim that finds the lock taken
   claims nothing, and the coordinator asks again on its next pass instead of waiting. With 2,960 ready runs and
   40 leases the ordering query takes about 9 ms; checking every active run's delegation, which each claim
   already did before 046, takes most of the claim's time (about 200 ms at that size).
2. Reconcile plan statuses and evidence references from broker task records. A successful task without accessible
   retained evidence stays uncertain. Store changed statuses without rewriting the scientist's question or lessons.
3. If tools are still running, release the lead's lease and wait. Tool custody continues independently. No model call
   or permanent per-project container is needed while waiting.
   Once they settle, uncertain execution yields `review_required` before another model call. A worker lease expiry
   marks an active attempt uncertain because compute may have finished without retained evidence. This state survives
   restart. An authorized checkpoint owner must reconcile custody, record the outcome and mark that attempt failed
   before requesting a later retry; pausing or starting another run alone does not acknowledge it. Existing explicitly
   reconciled failed history remains available to the lead. Scientific tool failures with definite outcomes can still
   inform a revised method. This rule does not claim automated recovery of expired tool leases.
4. Compile a fresh bounded context (schema 6, 031, 034 and 038) in the scientist's words: `question`, `constraints`,
   `scientist_instructions` (constraints the workspace saved as instructions), `starting_point` (the chosen data;
   `chosen_from_catalog` with no invented preparation record, or `prepared` with its reference; when the scientist
   chose several sets, `sets` lists each set's name, description and data), `methods` (the run's
   allowed profiles and descriptions), `analyses` (each plan step joined with its task record and, once finished, the
   digests of its result packages, each table with its rows, columns, `missing` (how many rows lack each column, since
   with the query, the cohort a later step builds on) and its cells when small), `earlier_findings`,
   `latest_answer` and `earlier_readings`. `earlier_findings` are
   the findings the lead stated most recently in the project, with the answer it gave then; `latest_answer` is the
   answer of its newest decision and how many findings it stated, even none, so an older answer never reads as current.
   Since migration 045 the context also carries `answerability`, the run's newest
   answerability ledger with its turn and answer (each part's words, state and reason, without the finding numbers of
   the reply that gave it; within 4,096 bytes by default, then without reasons, then only counted), and `attempts`, the run's
   refused lead replies newest first, at most 12, each with its turn and fixed reason code; a model service fault is not
   one of them. The lead starts its parts from the ledger and is told not to repeat its refused replies. The workspace
   shows the newest ledger under the answer, as the agent's reading. Since migration 047 a query
   method's source may carry a schema card (`reference_sql.md`); the context then has `data_cards`, placed after the
   results so a card never takes the room of a table the lead could state: for each source its keys, joins, the sizes of
   its large relations and the relations too costly to read, small value lists, missing values (a sampled relation's as
   estimates) and the lookup entries the question's words name first, within 6,144 bytes by default and what the budget
   still leaves, its key, brackets and commas included. The methods list never carries the whole card, and the turn's
   `context_inputs.data_cards` records the stored card's digest and the SHA-256 of what the lead read, or `"shown":
   "none"` for a card the coordinator could not read. A source without a card leaves the context unchanged. A card
   measured at an earlier version of a lab's data than the source's newest completed query read carries
   `card_behind_data`, a fixed note (never the version rows), in room kept before the card is fitted; its record marks
   `card_behind_data: true` and its `shown_sha256` covers the note (migration 082, `reference_sql.md`). Since
   migration 062 a source may list up to 128 relations, and a query method's relations take at most
   12,288 bytes by default and a quarter of the lead-context budget. Both the input ceiling and the optional section
   cap described below must be raised for the quarter to expose more. Before the whole-or-partial decision, relations are rebuilt from
   `name`, `about` and `columns`; columns retain only `name`, `type` and `description`. A column registered with
   `about` but no `description` exposes that text as `description`, under the same screening and size accounting. When
   both are present, `description` wins and `about` is dropped. Non-string descriptions are omitted with reason
   `non_text`; extra keys are dropped, with their names recorded once per relation as `fields_dropped`.
   A dropped name that fails the screen is recorded only by ordinal and SHA-256, never by its text.
   Every retained string passes the same fixed screen as lab notes. A match is replaced by
   `{"withheld":true}` before measuring or ordering, so neither presentation uses its text. The turn's
   `context_inputs.query_relations_withheld` records its source, relation, column when applicable, field, reason, field
   SHA-256 and screen version, never the text. Non-text fields hash their serialized JSON bytes. The methods specialist
   and model retries inherit this screened view. The complete query-source view then passes a recursive boundary:
   every string value and object key, including title, description, release and functions, passes `screen::screen`
   before ranking or sizing. Failed values become `{"withheld":true}` (`"withheld"` in scalar arrays); failed keys
   become `withheld_<n>`, counting from 1 within each object and skipping existing clean keys. Other values are
   unchanged. Boundary records use a screened source key, a `location` array of safe keys and indexes, `kind` (`key`
   or `value`), reason, field SHA-256 and screen version. The same boundary covers cards, facts and their audit records;
   registrations and stored cards stay unchanged for the guard, bindings and computation.
   A source whose screened relations fit is shown whole. A larger one, such
   as a lab's own database, shows under `relations` first the relations this project's queries read (newest query
   first), then those the question names, then those its constraints and instructions name, then the rest in the
   source's order, each with its columns and description while they fit; `other_relations` names the rest in the
   source's order and `relations_note` says so. A word of the question weighs 3 in a relation's name, 2 in a column's
   name and 1 in a screened description, times the number of relations over the number that hold it. The guard admits
   a query of any listed relation, so the lead reads a listed relation's columns with
   `SELECT * … ORDER BY 1 LIMIT 5`, and later calls show
   that relation first. Such a source's card keeps the keys, missing values, joins and value lists of the relations
   shown, a join when either side is one; `context_inputs.query_relations` records each cut source's shown relations
   and how many it listed, and the card's record names the relations it was cut to. Since
   migration 048 the context carries `tool_card`: each method the run may use, in key order,
   with how many of this project's analyses used it by their steps' status (the working plan's and the archive's
   together) and the newest analysis in the working plan with its record's status and fixed error code, within 4,096
   bytes; the claim names each analysis's method in its record for it. Once the project has archived steps it also
   carries `plan_pages`: the pages, how many steps they hold and by status, then the newest with their titles within
   2,048 bytes, and `steps_not_listed` for the rest. Saved
   next-step suggestions and lessons stay under `earlier_readings`, explicitly unverified. Catalog tool references
   remain in the checkpoint but not in working context. The checkpoint view takes up to two thirds of the input budget;
   digests are added newest step first while the rest allows, each charged with the key or comma it adds. A template
   analysis's digest (its step runs the method that carries the analysis catalog) that does not fit whole enters with
   its tables' cells withheld one table at a time, the table whose cells take the most bytes first, each marked
   `cells_omitted: context_budget` (`coordinator/result_fit.rs`): the lead keeps the table's path, rows and
   columns, can give it whole as an answer table, and binds findings only to cells it was shown. Any other digest
   enters whole or not at all: a query's one table is already bounded by the digest, and
   the lead can query again for the rows it needs. Digests that do not fit are named in `results_omitted` while the
   budget holds their names, the rest counted in `results_omitted_more` (a long run's list of names
   had passed the budget). A test run lost the reviewed normalization's 18-column table when every digest
   entered whole or not at all, about 34 KB of context being left after the 24,864-byte system text. Withholding
   applied to every digest was refused by the autonomy suite although the change never reached its query-only runs,
   so it applies to template analyses only, which a test run measures. 256 bytes stay free for the keys and counters
   added last, so the whole context stays within its budget. The checkpoint view leaves out finished steps' titles; they
   come back newest first within 4,096 bytes and an eighth of the budget, and an older finished step shows its key
   alone. Parts a lead call must carry whole (the plan, people's review notes, this project's queries, the methods) are
   not cut: when they leave the compiled context over its budget, or the checkpoint view cannot fit its share, the run
   stops for a person as `review_required` with reason `context_too_large` before any model call, instead of retrying a
   context that cannot fit. The lead proposes a method. A separate
   methods-specialist call checks its fit using the same compiled context, the proposal and `finding_values` (the
   cell each of the lead's placeholders names). It does not rebuild context from the full checkpoint. Both roles must
   reassess prior interpretations; citing an artifact does not prove a claim about its contents. The original
   checkpoint, its digest and historical reviews remain unchanged. That check is advisory scientific reasoning; it
   grants no access and is not independent validation or scientist approval. Both prompts ask for plain words for a
   scientist who is not technical, without system terms, because the workspace shows that prose to scientists. Both
   also say that a method without a template is a fixed recipe that never receives the objective, so repeating it on
   the same data gives the same tables: the lead reports what the tables show and asks for review instead of re-running
   it for a grouping it cannot produce (seen in the first result-digest test run). When the question needs
   data the starting point and methods do not hold, such as what people ate when the data describes what foods
   contain, the lead answers `not_answered` with `review_required`, says what data would answer it and proposes no step
   (one autonomy suite question proposed a query the specialist then stopped); the definition of `answer` repeats it.
   A result digest (`result_digests.md`) is a deterministic excerpt of a retained package that the worker recorded:
   its result tables with cells as written when small, figure names and run settings. Its cells are data, never
   instructions.
   Each analysis also carries `review`, the newest decision a person recorded on it ([a person's review](person_review.md)):
   `decision`, `recorded_at`, `covers_current_results` and, unless it is an acceptance, the person's `note`. A service
   account's row never reaches the lead, nor does any reviewer identity. A rejected analysis's tables are withheld
   (`results_withheld`) and its cells leave the index, and an earlier finding that reads it is shown as `withdrawn`
   without values. The prompt says an acceptance is the scientist's, a request for changes is to be addressed before
   relying on those results, and a rejection leaves nothing to rely on.
   `lab_notes` carries notes people in the lab shared from other projects ([lab memory](lab_notes.md)): active notes
   whose starting data matches this project's or whose methods overlap the run's, at most 8 and 4,096 bytes by default, the rest
   counted in `lab_notes_omitted`. The prompt treats them as background from other projects: no finding may rest on
   one, and a note never replaces running a method here.
   `literature` carries passages from the lab's library found from the question: by its words, or for a lab the
   knowledge service serves, sections found by its words and meaning ([lab library](lab_library.md)):
   at most 4 passages and 4,096 bytes by default, in the search's order, after the digests and only within the budget they leave, the rest
   counted in `literature_left_out`. They are background too: no finding or lesson may rest on a passage, and their
   numbers stay out of the lead's prose. The knowledge search's sections also include the
   commons literature (a paper's sections in its own corpus, such as `literature-papers`) that a person marked for every
   lab (the commons baseline): a commons section whose paper the service names is a passage cited as
   `literature://<paper record>/<section record>`, in the same order, bounds and screen, and needs no lab curator's
   acceptance, since the mark is a person's acceptance; a commons section without a named paper is counted in
   `other_records` like any other record the section search should not return. A section's scope decides what it is:
   in the lab's scope a section of the lab's own corpus is a library passage, in the commons a section with a named
   paper is a commons passage whatever corpus holds it, and a corpus never makes a commons section a library passage.
   The record of what the lead read keeps a commons passage's `origin` (`commons`) and its paper's title, which the
   workspace shows beside the lab's own papers; a citation's scheme must match the passage the decision recorded.
   Notes and passages come only for a lab on the `--lead-text-labs` list, and passages only of papers a lab curator
   accepted unless the lab is on `--lead-text-unreviewed-labs`. Each note statement and every field of a passage the
   lead would read (its paper's title, each section heading, its pages label, which the library's word search takes
   from free text, and the excerpt) also passes a fixed screen
   (`src/broker/screen.rs`, `untrusted-text-screen/v1`): text that reads like instructions to
   an agent (overriding instructions, chat roles and template tokens, text addressed to the agent, prompt language, the
   lead's own reply fields as JSON, directions to send data somewhere, hidden directives, and bidirectional or tag
   characters) is withheld, and the context only counts it (`lab_notes_withheld`, `literature_withheld`). The screen
   narrows one route for injected text; it is not a complete defense, and the permitted-action validators, the methods
   check and a person's review stay in place. For measurement, `agent-sandbox-broker screen-texts` applies the same
   screen to texts on standard input (`id` and `text` per line) and prints each id with the rule that would withhold
   it, never the text.
   The coordinator orders every list itself (methods by key, results newest step first and then newest first within a
   step, shared notes by their fixed order), so the same records in any order compile to the same bytes. Each turn
   record keeps `context_inputs`: the saved version and its SHA-256, each result shown with its digest SHA-256 and each
   left out, the analyses withheld after a rejection, each person's review read by id, the earlier findings' run and
   turn, the newest answer's run and turn, each shared note read with its statement SHA-256 and each withheld with its
   reason, the lab-text policy and screen version, and the library search's status (including `not_allowed`,
   `not_accepted` and `withheld`) with the count of passages whose paper was not accepted, query SHA-256, each passage read by paper, chunk, pages and text SHA-256, and each passage withheld with
   its reason, the field that tripped the screen (`title`, `section`, `pages` or `text`), the SHA-256 of its pages
   label and excerpt and, for any other field, that field's SHA-256; a withheld record keeps hashes only, since the
   screen stops at the first field it holds back and the fields after it were never checked. With the lead call's `context_sha256` it names exactly which records a step's context came from.
   The context carries `context_schema_version`: 5 added shared notes, 6 the library's passages, 7
   the newest answer and the withheld counts, with instructions that say how to read them, 8 the sources a query
   method reads, 9 `past_research` (below), 10 `tool_card` and `plan_pages`, 11 the methods check's findings, and 12 a large query source's relations shown
   in part.
   `past_research` carries the lab's earlier analyses, figure versions and methods from the knowledge foundation's
   search (`--knowledge-config-file`, `AGENT_SANDBOX_KNOWLEDGE_CONFIG_FILE`; schema
   `enrichedlab.knowledge-search.v1`: the service URL, the coordinator's projected ServiceAccount token file for the
   service's audience, the labs whose leads search, and a timeout). For a run in a listed lab, each turn delegates the
   run to that lab's scope alone, renewing the delegation, then searches with the question's first 500 characters
   under it, so the service ranks only that lab's records and the commons baseline. Records of another scope, which the
   service never returns, and records of this project's own analyses, which `analyses` already shows, are dropped and
   counted, and so is any record that is not the lab's earlier work (data class `prior_output` or `method`), such as a
   library paper's figure, which is also a `figure` record (`not_earlier_work`). The lab's library sections come from the
   same service as `literature` passages ([lab library](lab_library.md)). At most 6 records and 3,072 bytes enter by default, after the passages and within the budget they leave, each with
   its kind, title and the first 400 characters of its summary under a `knowledge://` reference; the rest are counted
   in `past_research_left_out`, and a record that reads like instructions is withheld and counted in
   `past_research_withheld` by the same screen as notes and passages. They are background: no finding or lesson may
   rest on one. `context_inputs.past_research` keeps the status (`shown`, `no_match`, `unavailable` with its fixed
   code, `not_configured`), the query's SHA-256, the embedding profile that ranked by meaning, each record read by id
   and text SHA-256, and each withheld. A service that does not answer is recorded, never a failed turn. An analysis
   record may also carry `query`, the database query that earlier analysis ran (migration 042): the coordinator sends
   the found records' broker tasks to `broker_research_run_prior_queries` under the run's lease, and the database
   returns the admitted text only for a query analysis in another lab-visible project of the run's lab that the run's
   person may read, finished with a completed receipt, for a method the run may use, and not rejected by a person. At
   most 2 queries enter, in at most 3,072 bytes by default and 1,500 characters each, with the data they read and their row count,
   never a row; a query that reads like instructions is withheld by the same screen, and one that returned no rows is
   not a working query, so it is counted and not shown. The lead may adapt one by writing the whole adapted query in
   the step's `sql`, and it is admitted again like any other; `context_inputs.past_research` keeps each shown query's
   SHA-256 and the counts found, shown, empty and withheld. A test run showed the need: the lead wrote a fruit query
   that returned no rows without seeing the vegetable query that had worked; shown both that empty query and the
   working one, its first query came back empty again and its second returned 20 rows. Each of this project's own
   analyses also carries `query`, the query it ran (migration 043): the coordinator sends the plan's newest 64 tasks
   to `broker_research_run_own_queries` under the run's lease, and the database returns the admitted text, digest and
   the newest receipt's status, row count and error code of each query analysis in the run's own project, never a
   row. The analyses show them newest first in at most 4,096 bytes by default and 1,500 characters each, with `status`
   `completed`, `refused`, `failed` or `not_run`; `context_inputs.own_queries` keeps each shown query's digest and how
   many were left out. A proposed query whose admitted text is one an analysis already ran to completion is refused
   with `proposal_query_repeated`, naming the step's key, since the pinned source would return the same table. In
   test runs six of eight runs spent their second turn proposing the query their first had run, because an
   analysis showed its table but not the query that made it.
5. Validate the typed proposal and references against the actual bounded context, including visible lessons, result
   references and the preparation reference. Pruned lesson references do not enter that call's evidence vocabulary.
   The proposal also carries `answer` (`answered`, `partly_answered`, `not_answered` or `not_yet`) and at most eight
   `findings`. A finding is a plain sentence whose result values are placeholders, short lowercase names in braces
   such as `{a}` or `{oj_mean}`; each is bound to one cell (result reference, table path, 1-based row, column) of a
   table shown in that call's context, or to a cell an earlier finding shown in that call binds (its value is in that
   finding's `values`), or the proposal is refused with `proposal_finding_unbound`, which names each
   placeholder at fault for the next lead call with what is wrong: `no binding`, `no placeholder`, or the part of its
   binding that names no shown cell (`ref`, `no cells` for a table shown without its cells, `table`, `column` or
   `row`), and for a table, column or row the choices it had, as in `a: column (its table's columns: supp, mean, lower,
   upper)` or `b: row (its table's rows: 1 to 2)`: the result's tables with cells (at most 4 named), the table's columns
   (at most 16) or its numbered rows; for `no cells` why the digest withheld them, in words; for a `ref` the call names
   (an analysis's `result_refs` or `results_omitted`) without showing it, that the budget left it out and what may be
   used instead. In a test run the budget showed one table per call, and three of five refused
   replies restated earlier findings whose results it had left out: the call showed those values, but the index held
   only the shown tables' cells. Each name is at most 300
   bytes. Test runs paused after three unbound findings each while the names alone were given; in three
   more the lead bound findings to a table whose cells the digest had withheld because one food name was longer than
   64 characters, which the refusal called a wrong `ref`. The prompt says a table shown with `cells_omitted` has no
   values to state, and the digest now keeps cells up to 160 characters (`result_digests.md`). A finding is at most 600 characters with at most 8 placeholders, and the
   lead writes at most 8 findings; several rows of one table go in a table statement (below), and a decision holds at
   most 16 findings with the composed ones.
   It also asks the lead to keep one unit per measure (a test run ranked vitamin A across international units and
   micrograms, which the specialist caught) and, for a question about which items have the most or least of
   something, to write the query to return only the rows the answer needs, at most 12, with each item's name in its
   first column, and then state every row of the table it answers from with one table statement whose rows are `all`
   (earlier it wrote three rows per finding with each name through a placeholder; a test run stated one
   of ten rows, and a suite run stated part of a 10- and a 20-row table, which the specialist sent back).
   A reply may also hold at most four `table_statements`. Each names a shown table by `ref` and
   `table` as a binding does, its `rows` (the `#` numbers of one to twelve distinct rows, or `all` for a table of one
   to twelve rows), one to four distinct `columns` in the order to state them, and `lead_in`, the plain words that open
   each sentence (at most 300 characters, spaces included, and no brace; the number rule below applies, named
   `table_statements.lead_in`). The coordinator writes the findings itself, after the lead's own: each opens with the
   lead's words (a later one adds `, continued`), states as many rows as eight values allow (four with two columns,
   two with four), fewer when the sentence would pass 600 characters, and binds every value to its cell under a
   placeholder it names (`t1_r3_c2` is
   statement 1, row 3, column 2). When the first of several columns holds a name, not a number, in every stated row,
   the name opens its row (`{t1_r1_c1}: mean {t1_r1_c2}`); otherwise a row reads `mean {t1_r2_c1}, upper {t1_r2_c2}`,
   with a column name's underscores and hyphens read as spaces and its braces as parentheses, so code never writes a
   placeholder it did not bind. A composed finding carries `composed_from`, the
   statement's number, which a reply cannot set, and passes the same finding checks. With the lead's own findings a
   decision holds at most 16: an earlier limit of 8 in all lost, in its first suite run, three goals' turns to six
   too-many-findings refusals. Size is checked apart, because the source review found sixteen findings with long names
   could pass the input ceiling: a proposal whose methods review (this context, the proposal and every
   finding's values) would pass the model's `max_input_bytes` is refused as `proposal_too_large`, naming by how many
   bytes. The check keeps 256 bytes free for the note a methods call carries when it asks again after an unreadable
   reply; a methods call over the ceiling had failed a suite run's turn as `coordinator_internal`,
   and a call's own budget refusal is now logged as `context_budget_too_small`. The next call's `earlier_findings` stop
   in their order at 16,384 bytes or at half of what the context budget leaves after the other fixed parts, whichever
   is less, with `findings_omitted` counting the rest, so the whole context stays within its budget and results keep
   room. The prompt asks the lead to name only
   the columns the question needs, to write few findings of its own beside a long table, and to give every finding
   it writes a value through a placeholder, a sentence without one belonging in the summary: on the same 80 replayed
   calls the sixteen-finding wording alone drew 9 refusals for findings without a value, and with these sentences none
   (65 of 80 accepted, against 60 of 79 for the earlier prompt). A shape
   the rules do not allow is `proposal_table_statement_invalid` and a part that names nothing shown
   `proposal_table_statement_unbound`; both name each statement and part at fault for the next lead call, the latter
   with the choices it had, as in `statement 1: column vitamin_c (its table's columns: ...)`. More than 8 written
   findings or 16 in all is `proposal_findings_too_many`, naming how many were written and how many the statements
   made. The
   specialist's
   prompt says the lead chose a composed finding's table, rows and columns and wrote only its opening words. In the
   suite, 17% of refusals were bindings the lead wrote; a composed finding has none. A record without
   table statements keeps its earlier shape; a replay pool that checks a stored proposal again drops its composed
   findings first, since their bindings are the coordinator's. Answer tables give whole result tables as
   part of the answer when it is a list longer than table statements can state: one suite goal spent all twelve
   turns paging through a list of vegetables, and another stated the first food groups only, in both attempts. A
   reply lists at most four in `tables`, each naming a table of a result its call was shown (with or without its
   cells, a lab table included) or one its earlier answer gave, one to six of its columns and opening words held to a
   table statement's rules. A template analysis's table may appear in several answer tables with different columns,
   such as its values per 100 g and per portion (after test runs were refused for it); any
   other result's table appears once, since its query or code chose the columns. `check_tables` refuses
   the same table with the same columns twice, or a table with no rows, and writes each table's
   row count from the shown digest; the lead never writes it (a reply that does is `proposal_schema_invalid`). A shape
   fault is `proposal_table_invalid` and a part that names nothing shown `proposal_table_unbound`, as in
   `table 1: column median (its table's columns: ...)`. A part may name the tables that state it, an answer may rest
   on tables alone, and their opening words follow the number rule. The prompt asks the lead to state, beside a table
   that answers by its rows, the numbers that define it (a cutoff a step computed, how many rows meet the conditions),
   each bound to a query that returns it; for a table too large to show that means a count, since the row count the
   context shows is not bindable. With answer tables alone the lead gave the vegetable list's table with at most one
   finding, as its prompt then allowed, in both attempts. The prompt's example answer table no longer repeats a suite
   goal. Later attempts showed why: the list's one query returned the answer key's 104 rows, but a digest then kept cells for at most 50
   rows, so the workspace, the answer and the suite saw no value, and the lead's count queries were refused as
   unordered (`reference_sql.md`). A digest keeps up to 200 rows within its byte limits; a lead call is
   shown the first 50 (`LEAD_ROWS`), with `rows_shown` when the table holds more, and binds only those rows. A reply
   with `paused` and an answered question is `proposal_paused_answered`: an answer with every part supported ends for
   review, and paused stays for a run that waits on a person (two suite goals each paused once with a full
   answer). The workspace shows every row of the named columns from the retained result, so no value passes through
   the lead. Migration 064 returns the newest decision's
   tables with its findings in `earlier_findings`, which the next call shows as words, table, columns and row count
   (a table from a rejected analysis withdrawn), and counts them in `latest_answer`; the coordinator is not ready
   without 064. Archival keeps the step a table comes from, and 064 returns the person's review of that analysis even
   after its step left the working plan (an older coordinator may have moved it), so a rejection still withdraws the
   table and a reply that gives it again is `proposal_finding_rejected`. The methods check may name `table N`. An average, sum or count is computed over
   every row that matches the question, never over rows limited first, and a difference the question asks for is
   computed in the query so one cell holds it. Each broken limit has its own code for the next lead call: `proposal_findings_too_many`,
   `proposal_finding_too_long`, `proposal_finding_no_values`, `proposal_finding_too_many_values`, and
   `proposal_finding_placeholder_invalid` for a brace or placeholder name the rules do not allow (an empty statement is
   `proposal_finding_invalid`). A finding bound to a rejected analysis is `proposal_finding_rejected`, and
   a lesson citing one `proposal_evidence_rejected`; an answer of `answered` or `partly_answered` needs a finding
   (`proposal_answer_needs_finding`). A reply carries `parts`, the answerability ledger, in
   every reply but a delegation that answers `not_yet`: at most 6 parts of the question in its own words (at most 300
   characters), each `supported` (naming the lead's own findings or table statements that state it, by number; a part
   that names none is not refused, as both ledger refusals in one suite were such parts and each paused its run, but a supported or partial part needs at least one finding or table statement in the reply, whatever its
   answer), `partial` (stated for some of the items it asks about, naming what states it and with a reason saying
   what the data does not hold, 049's conditionally absent), `unsupported` (with a reason saying why the data cannot
   hold it) or
   `open` (not settled yet, only in a reply that proposes a step) or `awaiting` (a choice only a person can make,
   named by its reason; a reply may stop on it); a reason is at most 600 characters (400 at first: every
   reason the suites refused at 400 held 402 to 492). The answer must
   follow from it: `answered` when every part is supported, `partly_answered` when at least one is supported or partial
   but not every one is supported, `not_answered` when every part is unsupported (`proposal_parts_invalid`, naming each
   part at fault with what is wrong, such as `part 2: reason missing` or `part 1: words 301 characters, at most 300`,
   or the count in each state). The check runs after the finding checks, so their faults come first; its words follow
   the number rule, and the methods review checks that the parts cover the question. The lead answers `not_answered`
   and proposes no step only when no part can be answered from the data; a part that needs data the source does not
   hold is marked unsupported while steps work on the others, and the lead stops before them only for a part awaiting
   a person's choice that the question, constraints and instructions do not settle (one suite goal asks
   three parts, the database holds two, and the lead stopped at its first call). Finding and ledger limits count characters,
   not bytes. A step may name in `literature_refs` at most 4 distinct library or commons passages its
   context showed, or parts of papers the lead read in the turn ([reading papers whole](#reading-papers-whole))
   (`proposal_literature_unknown` otherwise). No number may appear in the summary, step titles, rationales, lesson statements,
   findings or table statement openings unless the question, constraints, method descriptions or shown column names hold it
   (`proposal_prose_number`). Such a number in the summary is written as `(a number)` instead of
   refusing the turn (in the suite, 40% of refusals were numbers in prose), so every sentence and caveat
   stays: the turn record counts the masked numbers as `summary_numbers_masked`, and the kept reply still holds the
   original. A summary the mask would take past its 2,048-byte limit stays whole and is refused, and so does a number
   inside a name (a letter touches it, or a hyphen in any Unicode form, a soft hyphen or a zero-width joiner joins it
   to one, as in B12 or IL-6), which masking would garble. A whole number after an identifier label (chromosome or
   chr, exon, intron, figure or fig, table, section, equation, experiment, appendix, chapter; also through a list such
   as `chromosomes 3, 5 and 12`) stands when a paper passage, a read or cited part, or the scientist's question,
   constraints or instructions in the same call write that label kind with those digits. Data cards, method
   descriptions, the starting point and result cells add none. The label is a whole word (`not_table 9` labels
   nothing), and a measurement after a label stays refused, joined or after a space (`12%`, `12.5`, `12 mg`, `12 %`,
   `12 percent`, `12 (mg)`, `12"`): after a space only a plain word that is no unit, a bracket opening another
   reference (`(Fig. 2)`) or a spaced `&` or en dash in a list may follow, on the source's side as on the reply's (a lab test run's lead copied paper chromosome numbers into findings in 5 of 11 refused replies).
   Titles, rationales, findings, composed openings and lesson statements keep the refusal: cutting a rationale's
   sentence could drop a condition the methods review must see, and a masked title could lose what it names (the
   source review found both). The methods check's reason has the same number rule (`method_review_prose_number`), and
   its prompt names it, down to a row count or a limit, since a test run lost a turn to one. The specialist's prompt
   also says what repeated work is: a step that repeats an analysis of this project. Running here a query another
   project's
   analysis ran is how this project gets its own results, so it is not stopped for that alone; in test runs
   the specialist had stopped one such step and let three identical ones run. Because a query this project already
   ran to completion is refused before the specialist is asked, its context lists each query step as `query_steps`
   with `finished_in_this_project: false`, a fact the coordinator checked; a suite question was
   stopped as already run in this project when only another project had run it. The lead
   never writes a result value: the workspace fills each placeholder from the stored digest. Before committing, keep
   the exact texts both model calls were given, the fixed instructions and the compiled context, and since migration
   044 each reply that fits one kept text (262,144 bytes; a reply padded past that stays named by its hash in the
   receipt, unkept, and the turn still commits), by SHA-256 under the run's lease (migration 032); the stored addresses
   must equal the receipts' own or the turn is refused (`context_kept_mismatch`). Commit the decision, delegated tasks and next checkpoint in one
   transaction. A changed project revision or lost lease rejects the whole commit. Since migration 036 the same
   transaction records the plan steps the checkpoint changed as the lead's turn, and a reconciliation's checkpoint
   records its status changes as the analysis record's ([plan ledger](plan_ledger.md)). The same transaction records each
   task's origin as this run and turn, which the task's lineage reports as who asked for it. The existing task
   queue, profile limits and disposable-runtime worker then own execution.
6. Stop at a declared turn limit, an explicit pause, a review handoff, revoked delegation or three failed model calls.
   A new run can continue the same project's checkpoint. A failure never produces a canned successful result.

The lead and specialist currently use the same configured inference role in separate calls. Their agreement is
correlated evidence. The runtime records the role, server-reported model, prompt/template and response hashes, the
SHA-256 of the exact request body with the output-token limit, token usage when returned, the checked proposal,
specialist assessment, submitted task IDs and checkpoint revision. Since migration 032 it
also keeps the exact texts each call of a committed decision was given, so the request body can be rebuilt byte for
byte from the record (`coordinator::request_body`, or `coordinator::request_body_with` with the receipt's `settings`
when it names them) after the method catalog, notes or library change. The context can
carry passages from the lab's papers and its shared notes, so the API serves a kept text
(`GET /api/v1/research-runs/{run}/contexts/{sha256}`) only to a project reader who is a member of the project's lab,
and only when a decision of that run names it; an administrator inspecting the project reads its decisions but not
these texts. The workspace shows them under each decision and offers no download; packages never include them. It
leads with a plain summary read from the same text by fixed code (`clients/workspace/src/context_summary.ts`): the
question, constraints and instructions, the data, the permitted methods, the analyses by status and review, the latest
answer, the parts of the question by state and the refused replies, and the notes and passages given or held back by
the screen, as counts and names only. The summary never
restates a result value; the full text opens on request. Since migration 044 each reply is kept as
well, by the `content_sha256` its receipt names, and a readable lead reply the checks refused is kept with the context
it answered: the deferral's run event names both (`lead_context_sha256`, `lead_reply_sha256`), so a replay of that
call can be compared with what the lead said, and the same reader rule serves them. Keeping a refused reply is best
effort; the refusal stands without its addresses when keeping fails. Since migration 061 each call's
reasoning text (`reasoning` or `reasoning_content`) is kept beside its reply, without NUL characters (PostgreSQL text
cannot hold one): the receipt names `reasoning_sha256` (of the kept text) and `reasoning_bytes` (its full size), a text
over 262,144 bytes is cut at a character boundary and marked `reasoning_truncated`, a refused reply's deferral names
`lead_reasoning_sha256`, and the same reader rule serves it. An unreadable reply (no answer, cut
at its length limit, or text outside one JSON object) keeps only its fixed code and its sizes in the log. A model call answered with a server error (HTTP 5xx) is sent again once with the same body after 3 seconds, and
its receipt names the status as `server_error_before`; a second server error, a client error, a transport failure or
a timeout ends the call as `model_unavailable`, so a call never waits twice its timeout. Rejected turns
log fixed reason codes for malformed proposals, repeated actions, unsupported evidence and invalid method reviews;
provider, parser and database error bodies never enter that diagnostic field. Since migration 014 each deferral is
kept with its code in an append-only run event, so the record of rejected replies survives the failure counter
reset. Later capture of those classes needs a restricted artifact path; this implementation does not claim to preserve
them.

## Reviewing literature before planning

`--literature-review-labs` is an empty-by-default comma list and must be a subset of
`--lead-reading-labs`. Each listed lab gets a literature review before the lead plans. The same model uses its own
fixed instructions, the question, constraints, scientist instructions, shown search passages and method catalog.
When study reading is on it also sees the papers' gated study summaries. It uses the existing paper reader with a
separate window: `--literature-review-bytes` defaults to 32,768 (16,384 to 196,608), with at most three read rounds.
The coordinator checks that this allowance fits the declared model window and refuses to start without migration
070 and the coordinator grant.

A strict review has six lists: `approaches`, `methods_and_data`, `expected`, `pitfalls`, `gaps` and `papers`, at most
eight items each. An item has `text` of at most 300 characters and one to four distinct `refs` naming parts still in
the specialist's own reading window. Shown passages and study summaries alone do not qualify. Each method item also
names exactly one catalog `method_key` or a `source_relation` with `source` and `relation` from a catalog query.
Every number must appear in a cited part's content under the study pass's NFKC number rule; identifiers, page numbers
and omission counts do not support a quantity. The lab-text screen applies. A failed reply gets one correction with
fixed fault names; another failure proceeds without a review and records `status: refused` and `faults`.

A checked review or refused outcome is kept by run and an input digest covering the question, constraints, scientist instructions,
shown passages, catalog, study summaries, field gate, budget and specialist instructions. It is reused while these
stay unchanged, including across coordinator restarts or a refused lead reply. A reused refusal keeps `status: refused`,
its fault codes, receipts and reading ledger, makes no specialist call and never enters the lead context. Changed passages, including newly
accepted papers found by search, or an edited question trigger a new review. Results and prior planning calls do not.
The lead receives `literature_review`, labelled as written by the review specialist from parts it read, with refs
checked and not results. It fits whole within the lead's budget or is left out (`status: too_large`); the passages
remain. Actions may cite its item refs in `literature_refs`. A review item is not evidence for a lesson; the lead must
read the source part itself for that.

The turn's `literature_review` holds status (`read`, `kept`, `too_large`, `no_match` or `refused`), the input digest,
call receipts and the reading ledger, without the review text. Migration 070 stores checked and refused outcomes as immutable run
records and makes their instructions, call contexts, replies and reasoning readable through the kept-text API.
A project reader who is a current lab member can open
`GET /api/v1/research-runs/{run}/literature-reviews/{inputs_sha256}` as JSON, or add `?format=markdown` for a short
rendering; this route serves checked reviews only. Member evidence packages include each checked review under `runs/{run}/literature_reviews/` as JSON and
Markdown, and refused outcomes as JSON with status and faults. Non-member and inspection packages omit them. `scripts/qualify_remote_broker.sh lead-library` checks the
outline, study and section read sequence, lead context, durable reuse of both outcomes, artifact custody and membership
boundary. The writer refuses malformed reading rounds; package reads also guard against malformed older records.

## Reading papers whole

Search shows the lead short passages; reading lets it study the papers behind them before it
decides. The operator turns reading on per lab with `--lead-reading-labs` (`AGENT_SANDBOX_LEAD_READING_LABS`) and sets
the bytes one turn's reading holds with `--lead-reading-bytes` (`AGENT_SANDBOX_LEAD_READING_BYTES`, 16,384 to 196,608;
0, the default, turns it off). A reading lab must also be on `--lead-text-labs`, have a library and not be on
`--literature-claims-labs`, and the bytes must fit beside the model's `max_input_bytes` within `MAX_INPUT_BYTES` and the
declared context window, or the coordinator refuses to start. A coordinator with a reading lab serves only once
migration 069 is applied.

`--lead-study-fields` is a comma list of study fields whose audit passed; its empty default disables
study reading. With fields enabled, an outline adds the study record's `model-extracted, quotes checked` label,
counts for those fields, up to twelve catalog links and `library://<paper>/study`. That part reads only the allowed
facts in record order, with each fact's source section ref and no quotes: quotes are paper text, available by reading
the section next. Values pass the same lab-text screen, and facts that do not fit are cut from the end and counted in
`facts_omitted`. Links retain the service's assertion and review label; only `reviewed` links have been accepted by a
lab curator, and every other assertion is a machine or text-derived link no person has accepted. The service excludes
rejected and retired links. Target titles are bounded; a study part includes up to 24 links, with any removed for room
counted in `links_omitted`. The source lookup uses the knowledge service under the run's delegation after the paper's
membership and acceptance checks. No record adds no study summary; a service failure leaves the outline readable
with `study_status`. A read study part follows the existing citation, lesson and methods-review rules.

When the context shows at least one passage from the lab's own library, each such passage gains `paper_ref` and the
context gains `reading_offer` (how to ask, rounds left, bytes). A reply may then be
`{"action":"read","summary":…,"reads":[…]}`: one to four distinct refs, a summary of at most 1,000 characters and
nothing else (`read_request_invalid` otherwise, shown to the lead in the next call).

| Ref | Reads |
|---|---|
| `library://<paper>` | the outline: title, year, journal, first 12 authors, abstract, and every section (heading, role, pages, characters), figure and table with its ref |
| `library://<paper>/study` | the outline's study record: audited fields, source section refs and catalog links with their assertions |
| `library://<paper>/role/<role>` | every section whose parser label is `<role>` (ParsEnriched `parse/sections.py`) |
| a passage's ref | the passage's whole section |
| `library://<paper>/section/<key>` | a section an outline listed; `…@<n>` continues it from paragraph `n` |
| `library://<paper>/figure/<n>`, `…/table/<n>` | the outline's n-th figure (label, caption, page, and a model's description marked `description_source: model`) or table (caption and grid) |

The lead may name only a paper a passage showed, and a section, figure, table or continuation listed by a part an
earlier call held: a part's refs become readable when the round that read it ends with the part still held
(`read_ref_not_shown`, `read_ref_unknown`, `read_ref_invalid`). Each round checks the run's library again under its
lease for exactly the papers named (`read_not_member`, `read_not_in_library`, and `read_not_accepted` where the lab
needs acceptance) and reads each paper's parse once per turn from the lab's corpus: blocks, figures and tables, 25 a
page, at most 3,000 blocks and 200 figures and tables, with `complete: false` on an outline read in part. Blocks are
grouped into sections exactly as the knowledge service groups them, so a section the knowledge search found has the
same key here. Every field is screened as untrusted lab text: a paragraph, heading, caption or cell that reads like
instructions shows as `[withheld]`, and the round records its reason and digest. A section longer than half the reading
bytes comes in parts with a `next` ref. ParsEnriched's document API keeps 2,048 characters of a block; a section counts
paragraphs that long in `paragraphs_cut_by_parser_api`.

What the lead read appears in `reading.parts`, oldest first. A newer part that needs the room drops the oldest: one an
earlier call held is `evicted`, one read in the same round is `dropped` (no call held it, so it unlocks nothing), and a
part read again replaces its copy. A turn allows three read rounds; the offer then says no
reads remain, and a fourth read reply defers the turn with `read_rounds_spent`. A read call's input ceiling is
`max_input_bytes` plus the reading bytes and 4,096 bytes for the offer; the deciding call's checks, the methods review
and the commit are unchanged. An action may cite a part in `reading.parts` in `literature_refs`, and a lesson may rest
on one in `evidence_refs`; a passage the lead was only shown stays outside a lesson's evidence. The methods review
never reads the reading: it reads each cited part's ref, paper, heading or label and first 1,200 characters, in the
room its query facts leave (`reading_cited`), and a finding may rest on one of those parts as on a shown result. A
check shown no part names an unshown ref as before ("is not a shown result"). Both rules came from a test run that paused when the methods check cited a read part twice.

A lesson keeps only the evidence refs the call was shown as results or read as parts, and an observation left with
none is dropped. Every other check runs first on the reply as written, so the lesson bound, a blank
or constraint lesson and a ref to a rejected analysis are refused as before. The turn record's `lessons_trimmed` names
the refs left out that the context itself holds (a listed part, a shown passage, its paper; at most twelve), the total
left out and the lessons dropped; a ref the reply wrote that the context does not hold is only counted, because it may
be any text. A test run showed why: told each ref at fault, its lead fixed those and cited others it had
not read on the next attempt, three attempts in a row, and the run paused.

A refusal for an action's `literature_refs` (`proposal_literature_unknown`) names, for the next lead call alone, every
ref at fault from the first faulty action on, at most twelve, each with what to do instead: a part a read part lists
(an outline's sections, figures and tables, a cut section's continuation) was listed but not read, so read it first; a
shown passage's paper was not opened. Without a reading offer the names do not say to read. `Proposal::validate` still
names a lesson's unknown evidence the same way (`proposal_evidence_unknown`), which the lead check now only meets if
trimming left a ref behind. A test run paused after three such refusals that named nothing: its lead cited
listed sections and a shown passage as lessons' evidence and repeated them on each try.

The turn records `reading`: the bytes, the ceiling, and per round the read call's receipt, request ref receipts, each
part the next call held (ref, part, bytes and the part's SHA-256), and what was withheld, refused, evicted or dropped; `held`
lists the parts the deciding call read. The version 2 ledger keeps each request ref's SHA-256 and byte length; it includes
the ref itself only when the broker showed that ref or read its part. Refusals carry fixed codes. This also applies to
withheld, dropped and evicted refs and to fresh or reused literature reviews, including refused reviews. A malformed
request keeps only its reply's digest, byte length and fixed refusal code. Unresolved request text remains in the next
model context, never in public turn or package ledgers. Each read call's context, reply and reasoning are kept like
every other call's. Migration 069 lets a lab member read them (`broker_research_context`), adds their reasoning to a package that carries reasoning (role
`lead_reading`) and counts every paper a part was read from among the project's touched papers.
`scripts/qualify_remote_broker.sh lead-library` runs whole turns against a fake document service and model: two read
rounds with a refused ref, the kept and readable texts, the package, a paper removed between search and read, and a lead
stopped after three rounds. Its review drill checks that invalid refs from both readers stay out of non-member turns
and member and inspection packages, for fresh and reused checked or refused reviews, while members can read the
original requests and corrections in kept texts.
It also checks an outline, its study record and a listed source section in three rounds, including the delegated
knowledge request and each part's size, digest and kept call context.

## Model-written code in a reviewed template

A profile with a row in `broker_profile_templates` runs a reviewed template with one replaceable R block. The claim
adds that template's reference and contract (dataset columns and the block's return shape) to the profile description
the lead sees; it never sees data values. For that profile only, a proposed action carries `code`, the block's source;
code for any other profile is refused (`proposal_code_unexpected`), and a template action without it is refused
(`proposal_code_missing`).

Before the methods review spends a call, `codegen::admit` checks each block on tokens. It admits positively: every
name the block refers to must be one it binds itself, a column of the staged dataset (from the profile's contract) or a
name on `codegen::ALLOWED` (base R, stats, graphics, grDevices and utils functions an analysis needs, and a few from
the recommended packages). No call may be made on a computed value (`f()()`, `(x[[1]])(...)`, `obj$f(...)`), and the
names that assemble or evaluate code (`as.call`, `as.name`, `do.call`, `match.fun`, `get`, `eval` and the rest) are
not on the list. Numbers written in the code may set
parameters of a computation on the data (`round(x, 2)`, `quantile(x, c(0.25, 0.75))`), but may not become result
data: none inside a data constructor such as `data.frame` or `list` except shape arguments like `nrow`, and no vector
of them assigned to a name. It also checks size and characters, the entry point `analyze <- function(data)`, `:::`,
dot-names, superassignment, paths, URLs and secret-like strings. A refusal is recorded as a run event with a fixed
code such as `code_name_not_allowed`, `code_dynamic_call` or `code_literal_data`; the block itself is discarded with
the proposal. The coordinator keeps that code for the run in memory and gives it to the next lead call as
`prior_attempt`, because at its default temperature of 0 the lead would otherwise repeat the refused proposal; a restart
forgets it. For refused code it also keeps the names the block called that admission refused (at most 12 plain R
names of at most 48 characters, for example `replicate` or `do.call`) as `prior_attempt.names`. The names go only to
the next lead call and the methods check that reads the same context; logs, run events and recorded reports carry the
fixed code alone. A reply that does not fit the proposal's fields is refused with `proposal_schema_invalid` and names
each field that broke the shape, from the shape's own vocabulary: `missing:summary`, `invalid:answer` (the wrong kind
of value) or `unexpected:actions.notes` (a field the shape does not have; a name that is not at most 40 lowercase
letters, digits and underscores reads `other`), at most 12. A test run showed why: without them the
lead repeated one refused reply three times and the run paused. These are field names only, so the deferred turn's log
line carries them too; run events still carry the fixed code alone. The names then showed that the lead left out
`lessons` in five of six replies, so an absent `actions` or `lessons` list now reads as empty; a delegation still has to
name its actions. A number refusal (`proposal_prose_number`, `method_review_prose_number`) names each field and number
at fault, such as `summary: 10` or `reason: 20`, at most 8, and a query step without its query, or with one it may not
have, is named by its key. A number in prose may be a result value, so those names reach only the next call's
`prior_attempt`, never a log line or run event. Test runs lost retries to both refusals without knowing what
to change. Since r38 a number refusal also says where each number stood, as `where <field>: …<words> <<n>> <words>…`
(up to 24 characters of the lead's own words on each side, taken from the text and span that refused it, whitespace
collapsed, control and direction characters dropped), and `prior_attempt.fix` states what the lead may do instead:
bind the value to a result cell through a placeholder, write a name exactly as shown, or leave it out. The methods
specialist's immediate retry gets the same fix. A schema refusal names the fields its object allows, as
`allowed:<object>: <fields>`, after its faults (at most 4, never displacing a fault). These guidance names are left out
of the recorded fault count for the reasons that write them, so counts stay comparable with earlier runs. After any
refusal of a reply whose template-analysis steps chose analyses, the next call shows those analyses' contracts first
(`analysis_focus`); the choices travel beside the refusal's names, never among them. Before r38 only
`proposal_analysis_contract_not_shown` did this, so a step refused for its table lost its analysis's contract and the
next reply was refused for not having seen it. The third call after two refused replies in a row is narrowed instead of repeated
where the narrowing cannot forbid the repair: its context carries `redirect` (`kind` and the refusal it follows),
chosen in code from the last refusal's code. `one_step` (after a step, query or code fault of the lead's own) allows at
most one step; `fewer_findings` (after too many findings or a proposal too large to review) allows at most 8 findings
in all; `shape` (after an unreadable or misshapen reply of the lead's) asks for the object alone with exactly the
described fields. A finding refusal gets no redirect, since its repair may need a new query, and neither does a fault
the lead's reply may not have caused (a configured recipe, source or settings catalog, the methods specialist's
unreadable reply, the model service). The checks hold the reply to it (`proposal_redirect_ignored`, naming what it held
instead), the kept context and the committed turn's record carry it, and the coordinator logs `research turn
redirected`. Only a deferral the fenced yield recorded is kept for the next call, with the lease generation that
recorded it, the run's turn and its failure count after it; an older generation never replaces a newer one, and the
next claim uses it only while the run is still at that turn with that count, so a discarded or delayed lease, a
deferral whose response is still on its way, or a refusal from an earlier turn whose count recurs after a commit,
cannot steer `prior_attempt` or the redirect. A refused redirected call is the run's third, so the run then waits for a person as before. A restart
forgets the last refusal and with it the redirect. Whatever their source, `prior_attempt.names` stop at 2,048 bytes, each cut to 240 characters, with
`names_omitted` counting the rest (a query calling 123 functions was refused with 8,192 bytes of
names, more than the 6,144 bytes the lead call keeps free). The prose check runs before the methods specialist sees a proposal, so the specialist's prompt says a
number that the question, the constraints or a method description holds is allowed in a finding, even after a number
refusal: in a suite run the specialist held five findings for the question's own portion size. The query check appends `LIMIT` and the row cap to a query without one, which reads the same whether the query returns rows or one row of averages, so the specialist's prompt says a final `LIMIT` limits only the rows returned: in four suite runs the specialist held one question's correct one-row average as limited before averaging. A reply that cannot be read as one JSON object is refused with `model_content_missing` (no answer after the
reasoning), `model_content_not_json` or `model_response_incomplete` (the reply reached its length limit), and names
whose reply it was and what was wrong, such as `lead: text_after_object` or `methods_specialist: no_answer`, so the
lead keeps a proposal that only the specialist's reply lost. The reasoner's `response_format` JSON constraint never
reaches its answer: vLLM applies it only after the reasoning parser reports the end of reasoning, and the
`muse_glimmer` parser reports that only at a tool call (a synthetic request with the constraint set was
answered in plain prose). The coordinator therefore reads the shape itself. It removes one surrounding code fence and
names it in the call's receipt (`normalized: code_fence_removed`); anything else outside the object is refused. These
names are fixed words, so the deferred turn's log line carries them, and a `model reply unreadable` line records the
reply's shape: role, fault, finish reason, content and reasoning sizes in bytes, and token counts, never its text.
The reasoner's decoding differs between calls even at temperature 0: replays of two requests whose live reply had no
answer returned a readable one in 13 of 14 calls. So an unreadable reply (`model_content_missing` or
`model_content_not_json`) is asked for again, up to three calls in all, before it costs the turn one of its three
refusals, and the receipt of the reply that was read names the faults before it (`unreadable_before`). From the
second call on, the context carries `previous_reply_unreadable` (the last fault and the attempt number), since the same
text came back with no answer three times at near-identical sizes in a test run, as it had for the specialist in three
others; the call keeps the text it sent, so the kept context is the one the accepted reply answered, and the
specialist's context is built from the lead's context without the note. A reply at its
length limit, and every refusal of what a reply said, still refuses the turn at once. A query step refused for lacking
its query also logs its shape (`query step without its query`: its field names, whether `sql` was absent, null or
blank, and whether its title, objective or rationale reads like a query), since test runs lost five of
thirteen readable lead replies to that refusal. A reply refused for unbound findings logs `finding bindings unbound`
with counts only: how many placeholders had each fault, the number of findings, and how many faults were near misses
(a ref of the analysis-result form, one naming a shown analysis, a column differing only in case, a row numbered from
0); never a placeholder name, statement or cell, since a name may carry a value. An
accepted block, its SHA-256 and the admission report (with the allowlist's digest) are committed with
the task in the same transaction; the task's request digest also binds the block's digest. The template repeats the
check on the parsed expressions inside the sandbox, evaluates the block in a separate R process where the block's own
code can name only the allowlisted functions, and reruns it on a changed copy of the input to refuse tables that do
not change. These checks narrow what a block can do; the isolation boundary is the Kata sandbox with no network and a
read-only root and input. Code can still compute a value that does not come from the data, so admission is not
scientific validation; results stay labelled unvalidated until a person reviews them.

## Configured methods

A configured method (`configured_methods.md`) is a template profile whose contract carries a
`configuration`: a block template with `{{slot}}` markers and named settings, each a closed list of choices. The lead
sees its settings and choices by label, never the block, and proposes `settings` (one choice key for every setting) and
no `code`. The coordinator renders the block, admits it like any other block and submits it with the configuration
record. `proposal_settings_missing` and `proposal_settings_invalid` refuse a proposal the lead can correct;
`configured_block_rejected` means the recipe itself failed admission and `configured_method_invalid` that its configuration does not load, both catalog defects. Migration 022 renders the block
again at commit from the template row and the settings and refuses any difference, stores the configuration with the
labels read from the template row, and classes the task `configured` in lineage.

## Run lanes

Every run has a lane, `accepted` or `candidate`, fixed when it starts (migration 085): `candidate` when its actor holds
an operator-written grant in `broker_candidate_grants` whose `until` is still ahead, otherwise `accepted`. No role can
read or write that table, the workspace API never names a lane, and a trigger refuses any later change of a run's lane.
A coordinator claims one lane (`--run-lane`, default `accepted`); the three-argument claim every earlier coordinator
calls claims accepted runs only. A candidate coordinator runs the same image beside the accepted one and may take its
own lead system text (`--lead-system-file`, read once at start, at most four times the compiled text's bytes; an
accepted coordinator refuses the flag). Every lead call's receipt records the text's SHA-256 (`system_sha256`), so a
result always names the text that produced it. A candidate's retrieval generation comes from its own knowledge
configuration (`lab_retrieval_generations`). This lets a lead change be measured on a lab's real rows, paired with the
accepted lane, before any release.

## Lab files

A lab objects method (migration 083, route `server-objects`) reads files from the lab's object store. The claim shows
the lab's file collections found for the goal, each with a sample of file names; the lead proposes `objects`: the
`collection` it was shown and up to 20 `files` from that sample. The coordinator refuses any other shape with a
`proposal_objects_*` code and commits the request with the collection's bucket, prefix, copy generation and manifest
digest. A worker reads the request only under its own lease, copies the named files, checks each against the manifest
and records one receipt per attempt; it never shows the lead a file's contents. Each delimited file becomes a table a
template analysis may read by reference, when it has 1 to 64 columns and at most 50,000 rows.

A file wider than 64 columns (the October 30 canary's table had 96) is read again with `columns`: 1 to 64 distinct
names, each 1 to 128 bytes without control characters, written exactly as the step's tables listed them (migration
084). Each table the step makes keeps only those columns, in that order; a table missing one, or holding one twice, stays
a file only. An absent `columns` keeps every column; an empty list or `null` is refused
(`proposal_objects_columns_invalid`), never read as absent. The coordinator records `columns` only when named, so a
step without it digests exactly as before 084. A worker older than 084 refuses a request that names columns with a
fixed store error, before it reads any object.

## Step checks

A wrong result found only when a person reads the end of a long run means every step
after it rested on it. Each step the lead proposes now carries `expect`, what its result must show if the step works:
`columns` it must have, `rows` (`min`, `max`), `not_null` and `unique` columns, and `ranges` (at most 12, each with a
`column`, `min`, `max` or both, and a `basis` saying where the bounds come from), with `table` naming the result table
when the method writes several. A reply whose step has none is refused as `proposal_step_expect_missing`, and one whose
statement code cannot check (nothing stated, a list over 16 names or naming one twice, `min` above `max`, a range
without bounds or basis, more than 2 KiB) as `proposal_step_expect_invalid`; each names the steps' keys, the second
with the part at fault, and two such refusals in a row narrow the next call to one step. These run after every other
step check, so an archived key or a missing query is still named first. The plan step keeps its expectation
(`PlanStep.expect`; a step planned before step checks has none and is never checked).

Before each lead call the coordinator checks every stored result of a step that states one (`step_checks::checks_for`)
from the result's digest, whose column summaries the worker computed over every row of the table, so a table too large
to show is still checked whole. A result passes when every stated property holds, fails when one does not, and is
`not_checkable` when the digest cannot tell (a summary or count left out for room, distinct values past 20,000, a column
the digest does not list because the table is wider than 64, or a lab's table, whose values never reach the lead); not
checkable is never a pass. The reasons are code's own sentences, for example a column that goes above the expected
bound, with the value as written and the basis. The lead reads each analysis's worst check (`check`: state and at most
six reasons). A result that failed is shown without its cells (`cells_omitted: check_failed`), so no finding or table
statement can bind to it (the refusal names it as failing its step's check), no answer table may give it
(`table N: check failed`) and no template analysis may read it (it leaves `analysis_source_tables`). Its query, alone among
finished queries, may run again, under a new key with a corrected expectation when the expectation rather than the result
was wrong. The context record counts the checks (`context_inputs.step_checks`), and the turn record keeps each as the lead
read it (`step_checks`, within 12 KiB) for the workspace, whose analysis page shows the newest check of each result.
A plan whose steps, expectations or lessons would take the checkpoint past 96 KiB is refused as `proposal_too_large`
(`plan: N bytes over`); it is measured as the commit writes it, each delegated step with its task and active, with room
for each unfinished step's result references, so neither the commit nor a later reconciliation outgrows it. A step lists
at most 8 of its results' references in the plan (the claim gives up to 64; the rest stay on the task, and the lead reads
results by task). A finished step's list never grows: artifacts are registered only under a task's lease, and recovery
adds them only to a failed or cancelled task, which stays so.
A turn record past PostgreSQL's 128 KiB, measured with the tasks, actor and outcome the commit adds, first drops checks
from its end (counted in `omitted`) and is then refused the same way (`record: N bytes over`).

The lead's instructions grew by 1,429 bytes for this. That text changes every run's call, so the autonomy suite runs on
the release before it is accepted (`the_system_text_is_the_one_the_suite_accepted`). The methods review sends back an
expectation that contradicts the question, its query or the data card; a loose one is allowed. The checks are
structural: they catch a wrong join, filter, unit or row count, not a wrong scientific choice that still produces a
plausible table. A verifier specialist and checks against known answers are the next steps.

The coordinator reads a bound written as a number in text (`"max": "100"`) as that number and stores it as a number, so the
checkpoint's format is unchanged; text that is not exactly one finite number is still refused (a test run ended
on that refusal). A reply refused as `proposal_shape_invalid` now names each rule it broke (`action`, `summary`,
`delegate_without_actions`, `actions_without_delegate`, `actions_over_N`, `plan_steps_over_N`, `lessons_over_limit`),
in the coordinator's log and in the next call's `prior_attempt`.

## Supervisor

Off by default (`--supervisor-labs`). Before each lead call in a listed lab the
coordinator reads the run's whole history (`broker_research_run_faults`, migration 074: the newest 128 deferrals other
than waits, by code and reason, and each committed turn's outcome and tasks) and counts, across the run rather than the
last calls in a row (`coordinator/supervisor.rs`): refused replies by reason, refused replies in all, results that failed
their step's check, and delegating turns in a row, newest first, whose tasks all finished with no usable result (a
completed result whose check did not fail). Only this run counts: its own refused replies (the reply check's
`proposal_*`, `sql_refused*`, `code_*` and `read_rounds_spent`; not a model service, catalog, specialist or context
fault, though some share the event code) and the results of tasks its own turns submitted, each failed result once by
its reference. Under supervision each turn record keeps this run's failed results by task and reference (`failed_results`,
chosen from the run's own tasks, then at most 16) apart from the checks it shows, so a failure stays counted after its step is archived or its check is left out for room. Its
bounds:

| Count | Narrows the next call | Pauses the run |
|---|---|---|
| one refusal reason in the run | 3, to the reason's own redirect where one fits | 4 (`repeated_refusal`) |
| refused replies in all | | 6 (`refusals_total`) |
| results that failed their check | 3, to one step | 5 (`failed_checks`) |
| idle delegating turns in a row | 2, to one step | 3 (`no_progress`) |

A narrowing sets the context's `redirect` (`after`: `supervisor_<reason>`), which the reply check already holds the
reply to, unless a refusal's own redirect already narrows the call; the turn record keeps the counts and the action
(`supervisor`). A pause (`broker_supervisor_pause_research_run`, under the run's lease, only those four reasons) sets the
run `paused` with code `supervisor_paused` and records an event, so the run waits for a person with a fixed reason. The
supervisor never edits a reply, and the lead's instructions do not change. It is measured on the autonomy suite with
and without the flag before it is turned on for a lab.

## Orientation before planning

Off by default (`--orientation-labs`). On the first turn of a run in a
listed lab, after the context is compiled and the literature review placed, an orientation call (`coordinator/
orientation.rs`, its own short instructions) reads the same context the lead plans from and returns a typed
orientation: the question restated in one sentence; the query methods it expects to read and the other methods or
reviewed queries it expects to use, each by key with why; the passages, claims, reading parts or review items that
bear on the question, by ref, with what each establishes; the lab's earlier research that bears on it, by its
`past_research` ref, with what each establishes (in one measurement the reasoner cited earlier research
as papers in 28 of 28 refused paper refs); what stays unknown until the data is read; and at most three profiling
queries, each with all six fields. The instructions give the reply's exact JSON form, with each profiling step's `expect` an object, never a
text. Code reads only the form's fields, so a field the reply adds is ignored and never kept (the first measurement
refused every reply because the reasoner added a method beside most source keys); a missing list is empty, and each
item that is not its list's form is named by list, position and its first field that is missing or of another JSON
type (`profiles 1: title missing`). Code checks the rest: at most 6 sources, 8 tools, 6 papers, 6 earlier records, 6
unknowns and 3 profiles, each profiling step's `expect` an object, every text
1 to 300 characters with no control characters and nothing the instruction screen flags, no number the question, its
constraints or the scientist's instructions do not state, every key and ref one the call showed in its own section
(a paper's ref from the literature, claims, reading or review sections, an earlier record's from `past_research`),
none named twice.
A reply that breaks a rule is asked again once with its faults named by list, position and fixed code; a second
refusal, or a model service fault on the call (its server errors are already asked again once), is kept as `refused`
with its fixed codes and the run goes on as it does without orientation; neither counts as a failure of the run. Its
calls' texts are kept like any decision's.

A profiling query names a method with a query field, a step key neither the plan nor its archive uses, and an
`expect`. The profiling steps, at most as many as the run takes at once, become a lead-shaped proposal (`delegate`,
`not_yet`, the restated question as its summary, no findings) that `check_lead_reply` holds to every rule a lead step
meets, and each query, as the SQL guard admits it, must return a single row of aggregates or at most 50 rows. A step
that is not that small is left out and named; a refused set is dropped with its refusal code. The record keeps only
the profiling steps that passed, so a refused query is never stored or shown; a record holding a NUL, or one whose
text as PostgreSQL writes it could pass the table's 64 KiB check, is kept as `refused` instead. The orientation is kept
once per run (`broker_orientation_for_run`, migration 074, under the run's lease; a record another holder kept first
wins) with the profiling steps' keys. When steps remain, the turn delegates them through the methods specialist
instead of calling the lead: its record names the orientation's call as `lead` and carries `orientation` (status, step
keys, whether this turn committed them as its steps, whether the section fitted). A first turn that kept the record
and then failed before its commit leaves the steps to the next attempt, which admits them again and delegates them
without asking for another orientation.

Every later lead call of the run reads the orientation as one section, `orientation`, while the context fits: the
restated question, sources, tools, papers, earlier research (empty for a record kept before that field), unknowns and the
profiling steps by key, title and the status the plan
gives each (`not_run` when the plan has none), never their queries, with one sentence of instructions carried in the
section, so a lead call without an orientation keeps its instructions byte for byte. The workspace's project page shows
the newest run's orientation above the plan (`GET /api/v1/research-runs/{run}/orientation`, a project reader who is a
member of its lab), a view without the record's receipts, queries and expectations. The
coordinator starts with orientation labs only when schema 74 is in place and its role may call
`broker_orientation_for_run`. It is measured on the autonomy suite, two attempts with the qualification lab listed and
two without on the same image, and turned on for a lab only when it scores at least as well on passed, correct final
and correct ever.

## Library passages

With `retrieval_generation` in the knowledge configuration (knowledge 0.21), the lead's library sections
come from the knowledge service's retrieval v2 (`POST /v1/retrieve`, kinds `section`): each section is ranked through
passages of its whole text, by BM25 under the run's delegated scope, the GPU embedder and the recorded relations, and
the lead reads the passage that matched (its paper title and heading path, then the passage) in place of the section's
opening. A section with two matching passages appears once. Without the field the lead reads search v1 as before.
`lab_retrieval_generations` sets a lab's own generation in place of that field for that lab (a generation's name, or
`null` for search v1), so one lab can measure a generation on its own questions while the others keep theirs; each key
must be one of the file's `labs`, or the file is refused.

## Guarded database queries

A method with a registered source (`reference_sql.md`) reads a public reference database. The claim
adds the source to its method as `query` (title, release, description, relations, functions and row cap). The lead
proposes `sql`, one SELECT, and no `code` or `settings`; `proposal_sql_missing` and `proposal_sql_unexpected` refuse the
wrong shape. The lead prompt's list of step fields names the field each kind of method requires (`code`, `settings` or
`sql`): in test runs five of thirteen readable lead replies left a query step without its query, and the shape
logged for a test run showed the `sql` field absent with no query anywhere in the step, while that list named only the
fields every step has. The coordinator admits the query with the guard and returns a refusal's fixed code and refused names to the
next lead call. Before the guard, the first argument of a two-argument `round` or `trunc` is cast to numeric, the only
type PostgreSQL rounds to places (`reference_sql.md`); the proposal, the methods review and the turn
record read the cast text, and the turn record counts the casts as `query_places_cast`. The turn commit (migration 038)
stores the proposed text, the admitted text and its digest and refuses a query that does not match its source; the worker runs it read-only against the source's snapshot and retains the
table as the analysis's result.

The broker adds [reviewed query tools](reviewed_queries.md) under a source method's `query.reviewed_queries`.
The lead sees schemas and output columns, never templates, and may choose `reviewed_query: {tool, parameters}`
instead of `sql`. The coordinator validates, lowers and admits on every proposal; commit retains the tool version,
template digest and requested and normalized values. The worker rechecks admission and keeps the template,
parameters and lowered query in the result package. Migration 071 is required at start. A reviewed tool's result
still needs a person's validation.

## API and deployment

| Operation | Path |
|---|---|
| List approved research starting points | `GET /api/v1/projects/{project}/workspaces` |
| List a lab's starting points before its first project (the reader's labs only) | `GET /api/v1/labs/{lab}/workspaces` |
| Save the first question and the selected starting point, or several | `POST /api/v1/projects/{project}/prepare` |
| Start a bounded run | `POST /api/v1/projects/{project}/runs` |
| List a project's runs | `GET /api/v1/projects/{project}/runs` |
| Follow a project's changes as they happen (fingerprint only; the page reads the project again) | `GET /api/v1/projects/{project}/events/stream` |
| Read accepted turn records | `GET /api/v1/research-runs/{run}/turns?after=0&limit=50` |
| Read a kept text a decision's models were given or a reply they gave (lab members) | `GET /api/v1/research-runs/{run}/contexts/{sha256}` |
| Pause further lead decisions | `POST /api/v1/research-runs/{run}/pause` |
| Read every deferral with its reason code | `GET /api/v1/research-runs/{run}/events?after=0&limit=200` |
| Read a result package's stored digest | `GET /api/v1/tasks/{task}/artifacts/{artifact}/digest` |

Start fields are `run_id`, `expected_revision`, `allowed_profiles`, `max_turns` and `max_parallel`.
Mutations use the existing intent header. Only one active run is allowed per project. Pause fences the lead; tasks
already submitted retain their separate cancellation and evidence lifecycle. A new UUID starts a later run; retrying
the same UUID with the same original fields returns the original run.

The project stream sends `project_state` on connect and `project_changed` when the project, its analyses or its work
sessions change, each as a fingerprint and whether work is active. The workspace reads the project again only on a
change, instead of every 5 seconds, and falls back to that polling when the stream is unavailable.
`scripts/qualify_remote_broker.sh project-stream` checks it against isolated PostgreSQL with the API role.

### Starting a research project

The Svelte workspace lets a scientist select a registered data/method set, write a question and state study
constraints. Saving creates the initial checkpoint; starting the agent remains a separate action. Neither
selection nor a model supplies container paths, image tags, resource flags or data rows.

Serve accepts optional `--workspace-catalog /run/private/workspaces.json`. Without it, the starting-point list is
empty. This private, owner-readable file contains `schema_version: 1` and `entries`. Each entry has a unique `key`,
human-readable `name` and `description`, `group`, optional `principals` UUID list, nonempty `allowed_profiles`, and
the existing pinned `workspace` structure. Omitting `principals` allows current members of the named group whose
profile permissions cover the entry; an explicit list further restricts it. This is a selection catalog, not an
authorization grant. The broker and custody adapters still enforce the current profile, project and data bindings
at execution. Site owners must keep this catalog consistent with those bindings.

`prepare` accepts `workspace_key`, `goal`, `constraints`, `request_id` and `expected_revision: 0`. In place of
`workspace_key`, `workspace_keys` names two to sixteen distinct sets when serve runs with
`--combine-starting-points true` (`AGENT_SANDBOX_BROKER_COMBINE_STARTING_POINTS`, off by default): both lists of
starting points then answer `combine: true`. Turn it on only once the research coordinator reads `starting_points`
(migration 061 and later); an older coordinator refuses a saved version that names them. While it is off the lists
answer `combine: false` and `workspace_keys` is refused with `workspace_combination_unavailable` before anything is
saved. To roll the coordinator back to a version before 061, turn the option off first; a project already started from
several sets then stops at its next claim under the older coordinator (its saved version names `starting_points`), so
count those projects (`state->'workspace' ? 'starting_points'` in `broker_project_checkpoints`) before choosing to
roll back. The checkpoint's workspace then holds the union of their data and tool references in the order chosen, the
first set's profile and digests, and `starting_points` with each set's key, name, description,
profile, source and references; the answer's `allowed_profiles` is the union of their methods. Sets that need
different workspace images are refused with `workspace_combination_unsupported` and the chosen `keys`; an unknown key
is `workspace_unavailable` (404); both fields, one set as a list, or a repeated key are `workspace_selection_invalid`.
A catalog entry may not declare `starting_points` itself. The research lead is told to plan across every chosen set,
cover each one the question touches, and name the set behind each finding. The locked
checkpoint transaction enforces steering permission, rejects replacement of existing history, and makes retrying
the same request idempotent. A changed starting-point catalog can invalidate a retry; the client must reopen the
project instead of substituting a new request silently. The initial preparation receipt is always null: only
actual input custody can establish that data was prepared. Creating a question does not assert scientific validity.

The broker image also builds the Svelte client with its npm lockfile and a pinned Node LTS build image. Serve's
optional `--workspace-ui` exposes only `/` and `/assets` from the packaged, read-only UI directory at the same
origin. It never serves the artifact root as a directory. Project records and downloads retain their authenticated
API routes. UI responses use a separate same-origin content policy; API/download responses keep their restrictive
policy. Deploy behind the existing identity proxy and configure the broker to verify that application's issuer
and audience. The switch is off by default and does not enable authentication by itself.

Apply migration 011 with the migration identity and rerun the API/worker grant script. Create a separate coordinator
login, then use `grant_broker_coordinator.psql` with distinct `coordinator_role`, `api_role` and `worker_role` names. The long-running process receives its own DSN
file, a tenant and a private model configuration file:

```sh
agent-sandbox-broker coordinate --database-dsn-file /run/private/coordinator.dsn \
  --tenant research-site --model-config-file /run/private/model.json --max-parallel 4
```

`--ready-file /var/run/enrichedlab/coordinator/ready.json` enables the coordinator's readiness marker.
It is off by default. The path must be absolute and its parent an existing writable directory reserved for this
process. The command entrypoint removes any stale marker before telemetry initialization or checking arguments,
model budgets and reading allowances, library and knowledge client configuration, migrations and grants. Only after every start check passes and the claim loop is
entered does it write a short JSON object with `ready_at` (UTC) and `version` (the coordinator package version), using
a temporary file in the same directory, mode 0600, file fsync, rename and directory fsync. Rejected starts never publish
readiness. With readiness enabled, SIGINT and SIGTERM remove the marker and cancel active work before exit; normal
one-shot exits and errors clear it too. Lease recovery handles cancelled uncommitted work as before. An unclean kill
cannot remove the marker, so the next start clears it before validation. The marker is a start-readiness signal, not
an ongoing database or model health check.

The site should mount a pod-local writable directory at that path and use the exact exec probe and rolling-update
settings in [the broker image deployment contract](broker_image.md#coordinator-readiness). A failed replacement must
stay unready while the old Pod remains available. The PostgreSQL `lead-library` drill checks marker absence after a
refused migration check, presence during actual model calls, and removal after one-shot completion; local tests cover
path validation, telemetry failure through the command entrypoint, other refused starts, an interruption after writing
the temporary marker but before rename (no final marker and no temporary left behind), and SIGINT/SIGTERM cleanup.

Model configuration contains `schema_version: 1`, `endpoint` (the exact chat-completion URL), `model` (the inference
role), optional `credential_file`, `allow_internal_http`, `max_input_bytes` (at least the lead prompt plus 6,144
bytes for the methods review and a 4,096-byte lead budget, at most 262,144), `max_output_tokens` (512 to 65,536),
`context_window_tokens`, `timeout_seconds` (10 to 900), optional `sections` and `settings`. Input bytes conservatively count as tokens, and
startup requires `max_input_bytes + max_output_tokens + 1,024 <= context_window_tokens`. The lead-context budget is
`max_input_bytes` minus the fixed lead prompt and 6,144-byte review reserve. Its relation quarter, restored-title
eighth, revision-feedback quarter, query-facts eighth and remaining room for results therefore grow from the configured
ceiling up to their named caps.

`sections` sets upper bounds for variable lead-context sections. It is a strict object: an unknown key refuses the
model configuration. A missing object or key keeps the legacy default and produces the same context bytes at any input
ceiling. An explicit byte value must be at least its default and no more than half the derived lead-context budget. The
half-budget check applies to explicit overrides so an existing configuration without `sections` remains valid and
byte-identical even when a legacy nominal cap is above half a small lead budget; the section's share and final context
fit still constrain it. An explicit item count must be at least its default and at most 16.

| Key | Default | Effective share or other bound |
| --- | ---: | --- |
| `query_relations_bytes` | 12,288 bytes | Per query source, the smaller of this value and one quarter of the lead-context budget. At the 262,144-byte input ceiling the quarter is still below an 80 KiB projected relation list, so that list remains partial even when this value is raised. |
| `data_card_bytes` | 6,144 bytes | The whole `data_cards` section, including its wrapper, and no more than the room left after results. |
| `literature_bytes` | 4,096 bytes | All shown passages together, after results and data cards, and no more than the remaining lead-context budget. Each excerpt remains limited to 900 characters. |
| `literature_max` | 4 passages | At most this many passages, also subject to `literature_bytes` and the remaining budget; configurable through 16. |
| `past_research_bytes` | 3,072 bytes | All shown past-research records together, after literature, and no more than the remaining budget. Each summary remains limited to 400 characters. |
| `past_research_max` | 6 records | At most this many records, also subject to `past_research_bytes` and the remaining budget; configurable through 16. The separate limit of two figure versions does not change. |
| `own_query_bytes` | 4,096 bytes | All query views attached to this project's analyses. The newest-task and 1,500-character per-query limits do not change, and the final compiled context must fit. |
| `prior_query_bytes` | 3,072 bytes | All working queries attached to past research, also subject to the remaining budget. The separate limit of two queries and 1,500 characters per query does not change. |
| `tool_card_bytes` | 4,096 bytes | The complete tool card; methods that do not fit are counted as omitted. The final compiled context must fit. |
| `lab_notes_bytes` | 4,096 bytes | All shown lab notes together. The separate limit of eight notes does not change, and the final compiled context must fit. |
| `answerability_bytes` | 4,096 bytes | The full ledger is compared with this value, then retried without reasons, then reduced to a count if still over. The final compiled context must fit. |

These settings do not change the context sections' compile order. They can change relation order: a source shown whole
keeps its registration order, while a partial source is rebuilt in relevance order (relations read by this project's
queries, newest query first, then matches with the question, then matches with the run's constraints and the
scientist's instructions, then the remaining relations in registration order). Increasing a cap can therefore
move a source from relevance order to registration order. It may have no effect when its fraction, item count, character
cap, remaining budget or final context fit is tighter.

`settings` holds the served model's recommended
request settings from its model card or serving recipe: `temperature`, `top_p`, `min_p`, `top_k`, `reasoning_effort`
and `chat_template_kwargs` (at most eight flags or short lowercase words, such as `enable_thinking` or the template's
own effort name). Each is range-checked at start (a negative zero is refused, since the stored receipt could not
keep it), and any other key, including a field the request already holds, refuses the configuration. A `temperature`
there replaces the default of 0; every other key adds its field to every call. Each call's receipt records the
settings beside the texts' digests, so a request still rebuilds byte for byte. Without `settings` the request is the
original greedy one. A reply may be up to 1 MiB, room for a reply at the largest output allowance. The
settings belong to the served model, not the role, so a model swap behind the role changes them in the same
configuration change. Credentials are read from a private file at each call and never enter model context or logs.
Redirects and endpoint choices from model output are unavailable. HTTP requires an explicit
local/Kubernetes service route; otherwise HTTPS is required. Operator network policy must restrict the actual route.

The adapter uses OpenAI-compatible JSON chat completions and rejects truncated or malformed replies. Every call has
a deadline and bounded response body. The UTF-8 byte input budget plus output reserve is conservative; configure the
real model window and record actual token counts. A replaced model or provider changes this adapter/configuration,
not project identity, tool permissions, state or artifact custody. JEV remains an optional advisory classifier and
does not participate in authorization or scientific acceptance.

Multiple coordinator processes can claim different runs, one claim at a time per tenant. Lease generations fence
committed decisions, but do not make model calls exactly once: a call interrupted before commit may run again. Already committed tool actions do not
repeat. External tool side effects still need their own idempotency contract. The existing execution worker's storage
writer limit is unchanged by this coordinator.

## Scoring lead replies offline

`agent-sandbox-broker check-lead-replies` runs the coordinator's lead checks on replies read from standard input, for
the replay pool: one JSON object per line with `id`, the `context` exactly as the lead call sent it, the
`reply` content and `finish_reason` as the model returned them, the run's checkpoint `state` at that call, its allowed
`profiles` and `parallel` steps, its `methods` (the claim's profiles) and query `sources` as
`broker_research_run_sql_sources` returns them, the `own_queries` that call read, and the `analysis_catalog` it read
(without it a step for the template-analysis method is refused), plus the call's `max_input_bytes` (the
historical 65,536-byte ceiling when absent, for old replay records). Each reply is read as a live
reply is (a length cut, an unfinished reply or text outside one JSON object is `unreadable`) and checked by
`check_lead_reply`, the same function the coordinator calls for every turn: the proposal schema, its shape and steps,
configured settings, query admission and repeats, literature, findings, prose numbers and each block's admission. The
result index and the rejected analyses are rebuilt from the kept context with
`ResultIndex::from_context_with_ceiling`, which a test holds equal to what `compile_context` built. Both index the
tables as the call showed them (`shown_tables`): a table with a column named `#`, whose cells a context never shows
because `#` numbers its rows, is withheld, and a digest that is not summarized shows no tables, so a reply can bind
neither (earlier the live index read the stored digest and held both). A context no lead call could have
sent (its serialized bytes plus the fixed lead system text exceed that reply record's `max_input_bytes` or the
262,144-byte supported maximum, a shown row is not an object, or it holds more than 65,536 shown cells), or one whose
tables cannot be rebuilt (a shown table names a column twice, while its row objects hold one value per name), is
reported as `context_invalid` before any table is rebuilt. This replay validation reserves the system-text bytes only
for reconstruction; methods review sizing continues to receive the record's full input ceiling. An earlier replay pool left out
runs with a digest that is neither `summarized` nor
`no_result_tables` and has tables, since the live index of that time held them. One
JSON line comes out per reply with `outcome` (`accepted`, `unreadable`, `refused` or `context_invalid`), the fixed
code with the unreadable reply's fault or the number of names a refusal gave (never the names: they go to the next
model call only and can hold result numbers or query text), or an accepted reply's counts of steps, findings (with
how many were composed from table statements, and how many statements), lessons and the numbers masked in the summary. It prints no reply or context text, opens no database or network connection, and takes at most 10,000 lines
of up to 8 MiB each, reading a line at most one byte past that limit. This command and the other three that answer on
standard output (`digest-packages`, `screen-texts`, `check-queries`) log to standard error.
Replies kept before step checks propose steps without `expect`, so this command refuses their steps as
`proposal_step_expect_missing`; compare scores only between replies written under the same instructions.

## Current limits

The model receives tool descriptions, task records and the result digests described above. It never receives input
rows; a digest reads only `results/**` tables of a retained package, and for lab data cells
reach the lead only where the lab's policy allows. It does not read artifact bodies beyond those bounded excerpts. The existing
trusted custody preparer still resolves actual bytes. A workspace declaration does not prove materialization.
Model-written code runs only through a reviewed template profile as described above. Schema-bound scientific
parameters and authorized evidence-content retrieval are separate execution adapters. No model can supply cluster
flags, paths or credentials through this coordinator's action schema.

One checkpoint holds up to 128 plan steps and lessons; original revisions and turns remain in PostgreSQL. When the working
plan could not take another delegation, the coordinator first moves its oldest finished steps (completed, failed or
skipped, each with its analysis finished and read by no earlier finding or answer table) into the next archive page until the plan holds
96 (migration 048). `broker_archive_research_plan` does it under the run's lease and the project's
expected revision: the database takes the named steps from the checkpoint it holds, saves the rest as a new checkpoint
that the plan ledger records as `plan_archive`, and stores the moved steps, in plan order, in an immutable page with
their SHA-256, and reserves each moved key in `broker_plan_archived_keys`: a trigger refuses any analysis under a
reserved key, so no coordinator, the rollback image included, plans one again. Their analyses, results and history stay
in the project; the claim carries the archived keys and analyses, so an indexed archived analysis is never offered back
as another project's research, and the project's 64 newest analyses by submission, archived or not, whose queries the
lead sees as already run (plan order is not age, because archival keeps older steps an earlier answer reads). Lessons
are not archived: a project
still holds at most 128. `scripts/qualify_remote_broker.sh plan-pages` checks the writer, the claim and growth past one
working plan on PostgreSQL. The coordinator uses a bounded pending-work loop, not a runtime snapshot or a second
workflow database.

The methods check returns typed findings (migration 049). Its reply is `proceed`, `revise` or
`needs_review`: `revise` when the proposal has mistakes the lead can correct from the records it was shown, each
listed as a finding with the part of the proposal it is in (`summary`, `answer`, `parts`, `part N`, `finding N`,
`table statement N` or `step KEY`), one of ten fixed kinds of mistake (`wrong_rows`, `wrong_order`, `wrong_value`,
`units_mixed`, `rows_dropped`, `restriction_lost`, `repeated_work`, `question_not_answered`, `unsupported_statement`,
`other`), what is wrong in at most 300 characters, and at most four refs of results it was shown (any shown result,
with tables or not); `needs_review` only when a person must decide. Code checks every finding
(`method_review_findings_invalid` names each fault, a `proceed` that lists findings included, and a detail follows the
reason's number rule). Each fault is measured: `finding 2: detail 412 characters, at most 300`, `finding 1: refs 6 of
at most 4`, `finding 3: ref 2 is not a shown result`, `finding 1: about names no part of this proposal`. These
names are fixed words, positions and counts, so a deferred turn's log line carries them, as it carries schema field
names; every other refusal's names stay out of logs. A reply
refused for its findings or a number (`method_review_findings_invalid`, `method_review_prose_number`) is asked once more
in the same turn on the same proposal, with the code and names as `prior_attempt` (names within 512 bytes; the query
facts leave 768 bytes for that note), before the turn is refused (in three days 14 of 60
refusals were the check's own, and each cost the turn's lead call and a step toward a pause). A retry whose context
would not fit is not asked. The corrective request is one request: a reply to it that cannot be read leaves the first
refusal, and a model failure stands as itself. When the second reply passes, the turn records the first reply's code
and receipt as `methods_specialist_refused` and keeps its context, reply and reasoning, which a lab member reads as the
decision's other texts (migration 063); the names stay only with the call that read them. A second refusal ends the
turn as one refusal did before. A reply that is no review at all
(`method_review_invalid`) is not asked again here. A revision commits the turn with outcome `revise`: no analysis is submitted, the proposal is
kept as `revised_proposal` so no reader of answers takes it for one, its lessons wait, and the run returns to the
queue. The turn record also keeps `revision.targets`, the words of the part each finding names, taken by code from that
proposal (at most 240 characters each), because the lead's next call has no other copy of it. The lead's next context
carries `methods_review` (the reason, each finding with its target, and `revisions_left`) within a quarter of the
lead's budget and at most 6,144 bytes. Each finding's part, kind, target and detail are what the lead corrects from,
so room is taken first from the refs, then from the reason (it keeps only what the findings leave), then the targets
and details share what remains in equal parts, a shorter one keeping its whole length and each keeping at least 32
bytes; past that only the leading findings are shown, with the rest counted. A shortened text ends with `…`. At the
smallest lead budget (a 1,024-byte room) six findings at their limits keep about 47 bytes of each target and detail.
The methods check reads its own earlier findings there. A run is
sent back at most twice: the database refuses a third `revise`, and past the budget the coordinator records the review
as one a person decides. Before that, every such review ended the run for a person; on the earlier suites that was the largest
loss (five or six of eighteen goals per three runs, each on a lead mistake the review named).
`scripts/qualify_remote_broker.sh methods-revision` checks the commit, the results read and the budget on PostgreSQL.

Before the methods check reads a proposal, code measures each query step against its method's schema card
(`src/broker/query_facts.rs`): it reads the admitted query with PostgreSQL's own parser and, for each value the
query compares a card column with (`=`, `IN`, `LIKE` and `ILIKE` with the column on the left), states whether the card
holds it, how many rows use it and, when it does not, the nearest values the card holds (`entry`); how many entries a
pattern matches (`pattern`); the units of the lookup entries the query's own conditions on that lookup keep, when they
differ (`unit`); the rows an inner join drops because their value names no row on the other side, as the card measured
them (`join_drop`; an outer join's preserved side drops none); and how many rows a value keeps (`filter`, with the
relation's rows as `of` only when the card counted them exactly). It states
them only for a query it reads whole: one `SELECT` with no set operation, a `WITH` of plain `SELECT`s that is not
recursive, and no subquery, derived table or function in `FROM`; any other query gets no facts rather than facts about
a column whose owner or meaning it cannot prove. Each `FROM` item is one instance named by its alias or relation, a
`WITH` name never stands for a card relation, and a cast with a type modifier or to a float states nothing. The card
writes values as text, so values compare under the column's type as the source registered it (integers and exact
decimals as numbers, a quoted literal read as the column's type reads its input, text as text, `NULL` never); a column
of another or unregistered type states nothing. A join's drop is stated only when the query compares the two columns
with the casts the card's own `on` condition uses. Values the card does not hold come first; a step
carries at most eight facts and counts the rest. Computation uses the original card. Before ranking, sizing or sending
facts, the recursive boundary screens every string value and key, including detail-column names used as example keys.
Only failed strings and keys change; the measured counts, examples and `complete` still refer to the original card.
The turn's `query_facts.withheld` records each held string's source, step, safe location, kind, reason, SHA-256 and
screen version without text. This applies even when the
lead's card was left out. The facts are the card's own measurements, so no model writes them and
no call is spent on them. The methods check reads them as `query_facts`, whole steps within what its input ceiling
leaves, and the turn records them. When the check sends the proposal back, the record keeps them under
`revision.query_facts`, the run's results read gives them with the revision request (migration 051), and the lead's next
context carries them as `query_facts` within an eighth of its budget. Stored revision suggestions pass the screen again
before sizing, including older turns; additional withheld fields enter `context_inputs.query_facts.withheld`.
`scripts/qualify_remote_broker.sh query-facts`
checks the read on PostgreSQL.

A lab the operator names in `--literature-claims-labs` gets a literature specialist: before the lead's
call it reads the library passages the lead would read and returns typed claims, each anchored to one of them, which the
lead reads in their place; a run asks once per set of passages, and a reply that cannot be read or checked leaves the
passages ([lab library](lab_library.md)). No lab is named by default.

`scripts/qualify_remote_broker.sh research-runs` checks the actual PostgreSQL boundary: separate role authority,
exclusive claims, atomic task/checkpoint commit, a reconnect, stale revision rejection, pause fencing and revocation.
`cargo test --locked --test broker_research_runs` checks model proposal scope and uncertain evidence handling.
`scripts/qualify_remote_broker.sh generated-code` checks that a template needs an admitted block, a fixed method refuses
one, and only the current lease's worker and custodian read the reference and the bytes.
`scripts/qualify_remote_broker.sh result-digests` checks that only the current lease's worker records a digest, bound to
the registered bytes and immutable, that only the coordinator holding a run reads its results and newest findings, and
that only a reader of the artifact reads its digest. The coordinator refuses to start without migration 021 and its
grant, and without migration 025, whose results read carries people's reviews; `person-review` checks that read returns
only the newest person's review of each planned analysis, named by id since migration 027. It also refuses to start
without migration 027 and its grant for the lab's shared notes; `lab-notes` checks that read.
`scripts/qualify_remote_broker.sh configured-methods` checks that a configured task holds only its rendered recipe:
code, other settings, unknown or missing choices, another template or an extra field are refused at commit, and the
readers and lineage return the configuration. The API and the coordinator refuse to start without migration 022.
`scripts/qualify_remote_broker.sh reference-sql` checks the query path end to end on PostgreSQL; the coordinator and
a worker with a query folder refuse to start without migration 038 and their grants.
