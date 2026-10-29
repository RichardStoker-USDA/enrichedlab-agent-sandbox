<script lang="ts">
  import { onDestroy, tick, untrack } from "svelte";
  import type { Identity, LabLink, LabLinkQueue, Library, LibrarySearch } from "./contracts.ts";
  import { currentLab } from "./lab.svelte.ts";
  import { day } from "./lab_notes.ts";
  import { acceptAllWords, acceptanceWords, addedWords, batchItemWords, batchWords, byline, fileProblem, labelTitle, library, libraryClaim, libraryFailure, libraryOutcomeUnknown, parseWords, queryProblem, settleAcceptance, settleUploads, unacceptedWords, type BatchState } from "./library.ts";
  import { BrokerError } from "./api.ts";
  import { commonsFailure, knowledge, linkWords, MAX_REVIEW_NOTE, outcomeUnknown, queueWords, reviewedWords } from "./knowledge.ts";
  import { covers, live } from "./live.ts";
  import { href } from "./route.ts";

  // The lab's library: the papers people added, a word search over their text, and adding a PDF.
  // Every rule (membership, who may remove, what a label reads as) is the broker's; this page words its answers.
  let { identity, notify, openPaper }: {
    identity: Identity;
    notify: (message: string, error?: boolean) => void;
    openPaper: (paper: string) => void;
  } = $props();

  const group = $derived(currentLab(identity));
  let view = $state<Library | null>(null);
  let loading = $state(true);
  let failure = $state("");
  let controller: AbortController | null = null;

  // The library read again for settling a batch: the list only when it was read whole and the page still shows that
  // lab, otherwise none, so an earlier list never settles a write.
  async function reload(key: string): Promise<Library | null> {
    try {
      const next = await library.list(key);
      if (!alive || group?.key !== key) return null;
      view = next; failure = "";
      return next;
    } catch {
      return null;
    }
  }

  async function load(quiet = false) {
    const key = group?.key;
    if (!key) { loading = false; return; }
    controller?.abort();
    const c = new AbortController();
    controller = c;
    loading = true; if (!quiet) failure = "";
    try {
      const next = await library.list(key, c.signal);
      if (!c.signal.aborted) { view = next; failure = ""; }
    } catch (cause) {
      if (!c.signal.aborted && !(quiet && view)) failure = libraryFailure(cause, "load");
    } finally {
      if (!c.signal.aborted) loading = false;
    }
    await loadLinks(key, c.signal);
  }

  // Links the knowledge service proposed from the lab's papers, such as a paper that names one of the lab's tools.
  // A person who curates or leads the lab accepts or rejects each one; the service decides
  // and records the review in that person's name. Without the knowledge service the section stays away.
  let links = $state<LabLinkQueue | null>(null);
  let linksFailure = $state("");
  let reviewing = $state("");
  let rejecting = $state("");
  let rejectNote = $state("");
  let linkFailure = $state<{ id: string; message: string } | null>(null);
  async function loadLinks(key: string, signal: AbortSignal) {
    linksFailure = "";
    try {
      const next = await knowledge.links(key, signal);
      if (!signal.aborted) links = next;
    } catch (cause) {
      if (signal.aborted) return;
      links = null;
      const quiet = cause instanceof BrokerError && ["knowledge_not_configured", "knowledge_person_required"].includes(cause.reason);
      if (!quiet) linksFailure = commonsFailure(cause, "links");
    }
  }
  async function review(link: LabLink, accept: boolean) {
    if (reviewing) return;
    if (rejectNote.length > MAX_REVIEW_NOTE) { linkFailure = { id: link.relation_id, message: "The note is at most 500 characters." }; return; }
    reviewing = link.relation_id; linkFailure = null;
    try {
      const done = await knowledge.review(link.relation_id, accept, accept ? "" : rejectNote);
      if (links) links = { ...links, proposals: links.proposals.filter((l) => l.relation_id !== link.relation_id) };
      rejecting = ""; rejectNote = "";
      notify(reviewedWords(done.outcome));
    } catch (cause) {
      linkFailure = { id: link.relation_id, message: commonsFailure(cause, "review") };
      // A review sent but not confirmed may have been made: the queue is read again before any action is offered, so
      // the page shows what the service holds (the message says it was reloaded).
      const key = group?.key;
      if (outcomeUnknown(cause) && key) await loadLinks(key, controller?.signal ?? new AbortController().signal);
    } finally {
      reviewing = "";
    }
  }
  $effect(() => { void group?.key; untrack(() => {
    view = null; found = null; query = ""; onlyUnaccepted = false; confirmAll = false; acceptFailures = []; acceptUnknown = [];
    if (!running) batch = [];
    void load();
  }); });

  // A lead or reviewer sees which papers still wait for their acceptance and can list only those.
  let onlyUnaccepted = $state(false);
  const curator = $derived(view?.papers.some((p) => p.can_accept) ?? false);
  const unaccepted = $derived(view?.papers.filter((p) => !p.acceptance) ?? []);
  const shown = $derived(onlyUnaccepted && unaccepted.length ? unaccepted : view?.papers ?? []);
  // A lead or reviewer accepts every waiting paper they may accept, after one confirming step; each is the same
  // acceptance as on its page, sent one at a time with its own request key, so a repeated step saves once. It stops
  // after the paper being accepted when the person leaves the library or changes lab. A paper whose answer was lost is
  // settled from the list read again afterwards, never counted as refused or accepted on a guess.
  const acceptable = $derived(unaccepted.filter((p) => p.can_accept));
  let confirmAll = $state(false);
  let acceptingAll = $state<{ done: number; total: number } | null>(null);
  let acceptFailures = $state<string[]>([]);
  let acceptUnknown = $state<string[]>([]);
  let acceptAllButton = $state<HTMLButtonElement | null>(null);
  let confirmButton = $state<HTMLButtonElement | null>(null);
  const acceptKeys = new Map<string, string>();
  async function openConfirm() { confirmAll = true; await tick(); confirmButton?.focus(); }
  async function cancelConfirm() { confirmAll = false; await tick(); acceptAllButton?.focus(); }
  async function acceptAll() {
    const key = group?.key;
    const waiting = acceptable.map((p) => ({ id: p.paper_id, title: p.title ?? "Untitled paper" }));
    if (!key || !waiting.length || acceptingAll) return;
    confirmAll = false; acceptFailures = []; acceptUnknown = [];
    acceptingAll = { done: 0, total: waiting.length };
    const refused = new Map<string, string>();
    const lost = new Set<string>();
    let sent = 0;
    try {
      for (const paper of waiting) {
        if (!alive || group?.key !== key) break;
        if (!acceptKeys.has(paper.id)) acceptKeys.set(paper.id, crypto.randomUUID());
        try {
          await library.accept(key, paper.id, { accept: true, request_id: acceptKeys.get(paper.id)! });
          acceptKeys.delete(paper.id);
        } catch (cause) {
          if (libraryOutcomeUnknown(cause)) lost.add(paper.id); else refused.set(paper.id, libraryFailure(cause, "accept"));
        }
        sent += 1;
        acceptingAll = { done: sent, total: waiting.length };
      }
      if (!alive || group?.key !== key) {
        notify(`Stopped after leaving ${alive ? "the lab" : "the library"}: ${sent} of ${waiting.length} papers sent for acceptance. Open the library again to see which are accepted.`, true);
        return;
      }
      const fresh = await reload(key);
      if (!alive || group?.key !== key) {
        notify(`Stopped after leaving ${alive ? "the lab" : "the library"}: ${sent} of ${waiting.length} papers sent for acceptance. Open the library again to see which are accepted.`, true);
        return;
      }
      const accepted = fresh ? new Set(fresh.papers.filter((p) => p.acceptance).map((p) => p.paper_id)) : null;
      const ids = waiting.slice(0, sent).map((p) => p.id);
      const settled = settleAcceptance(ids, refused, lost, accepted);
      for (const id of ids) if (accepted?.has(id)) acceptKeys.delete(id);
      const title = new Map(waiting.map((p) => [p.id, p.title]));
      acceptFailures = settled.refused.map((id) => `${title.get(id)}: ${refused.get(id) ?? "its answer was lost and it is not accepted; try again."}`);
      acceptUnknown = settled.unknown.map((id) => title.get(id) ?? "Untitled paper");
      notify(acceptAllWords(settled.accepted, waiting.length, settled.unknown.length), settled.accepted < waiting.length);
    } finally {
      acceptingAll = null;
    }
  }
  // A change to the lab's records reads the page again in place (live.ts); a failed quiet read keeps what it shows.
  const unwatch = live().watch(["lab:*"], (changed) => { if (group && covers(changed, `lab:${group.key}`)) return load(true); }, { gapMs: 2_000 });
  let alive = true;
  onDestroy(() => { alive = false; controller?.abort(); searching?.abort(); unwatch(); });

  // Search: one request at a time; a newer search replaces an older one.
  let query = $state("");
  let found = $state<LibrarySearch | null>(null);
  let searchFailure = $state("");
  let searchBusy = $state(false);
  let searching: AbortController | null = null;
  async function search(event: SubmitEvent) {
    event.preventDefault();
    const key = group?.key;
    if (!key) return;
    const problem = queryProblem(query);
    if (problem) { searchFailure = problem; return; }
    searching?.abort();
    const c = new AbortController();
    searching = c;
    searchBusy = true; searchFailure = "";
    try {
      const next = await library.search(key, query.trim(), c.signal);
      if (!c.signal.aborted) found = next;
    } catch (cause) {
      if (!c.signal.aborted) { found = null; searchFailure = libraryFailure(cause, "search"); }
    } finally {
      if (!c.signal.aborted) searchBusy = false;
    }
  }
  function clearSearch() { searching?.abort(); found = null; query = ""; searchFailure = ""; searchBusy = false; }

  // Adding: one PDF opens its page once read; several are read one at a time, each listed with where it stands, and
  // the person may stop the batch after the paper being read. Each file keeps its request key until it is saved, so
  // choosing the same file again after a failure saves it once.
  let input = $state<HTMLInputElement | null>(null);
  let adding = $state<string | null>(null);
  let addFailure = $state("");
  let running = $state(false);
  let stopping = $state(false);
  let batch = $state<{ name: string; state: BatchState; problem: string }[]>([]);
  let batchLab = $state("");
  const requestKeys = new Map<string, string>();
  const fileKey = (file: File) => `${file.name}/${file.size}/${file.lastModified}`;
  function requestFor(file: File) {
    const key = fileKey(file);
    if (!requestKeys.has(key)) requestKeys.set(key, crypto.randomUUID());
    return requestKeys.get(key)!;
  }
  async function chosen() {
    const files = Array.from(input?.files ?? []);
    if (input) input.value = "";
    const key = group?.key;
    if (!files.length || !key || running) return;
    addFailure = "";
    if (files.length === 1) return addOne(key, files[0]);
    running = true; stopping = false;
    batchLab = group?.name ?? "";
    batch = files.map((file) => {
      const problem = fileProblem(file);
      return { name: file.name, state: (problem ? "failed" : "waiting") as BatchState, problem };
    });
    // This operation owns its items until they are settled: the list cannot be cleared while it runs.
    const items = batch;
    try {
      for (let i = 0; i < files.length; i++) {
        if (items[i].state !== "waiting") continue;
        if (stopping || !alive || group?.key !== key) { items[i].state = "skipped"; continue; }
        items[i].state = "reading"; adding = files[i].name;
        try {
          const got = await library.add(key, files[i], requestFor(files[i]));
          requestKeys.delete(fileKey(files[i]));
          items[i].state = got.created ? "added" : got.restored ? "restored" : "present";
        } catch (cause) {
          if (libraryOutcomeUnknown(cause)) items[i].state = "unknown";
          else { items[i].state = "failed"; items[i].problem = libraryFailure(cause, "add"); }
        }
      }
      adding = null;
      if (!alive) {
        notify(`Stopped after leaving the library: ${batchWords(items.map((item) => item.state))}`, true);
        return;
      }
      // A file whose outcome is unknown is settled only from a fresh list of the same lab, by its bytes.
      if (items.some((item) => item.state === "unknown")) {
        const fresh = group?.key === key ? await reload(key) : null;
        const hashes: (string | null)[] = [];
        for (let i = 0; i < files.length; i++) hashes.push(fresh && items[i].state === "unknown" ? await sha256(files[i]) : null);
        const settled = settleUploads(items.map((item) => item.state), hashes, fresh ? new Set(fresh.papers.map((p) => p.source_sha256)) : null);
        settled.forEach((state, i) => {
          if (items[i].state !== "unknown") return;
          items[i].state = state;
          if (state === "added") requestKeys.delete(fileKey(files[i]));
          if (state === "failed") items[i].problem = "Its answer was lost and it is not in the library; choose it again to add it.";
        });
      } else if (group?.key === key) {
        await reload(key);
      }
      const states = items.map((item) => item.state);
      const unsettled = states.some((state) => state === "failed" || state === "unknown" || state === "skipped");
      if (!alive) { notify(`Stopped after leaving the library: ${batchWords(states)}`, true); return; }
      if (group?.key !== key) { batch = []; notify(`${batchLab}: ${batchWords(states)} Open its library to see the papers.`, unsettled); return; }
      notify(batchWords(states), unsettled);
    } finally {
      adding = null; running = false; stopping = false;
    }
  }
  async function sha256(file: File) {
    const digest = await crypto.subtle.digest("SHA-256", await file.arrayBuffer());
    return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
  }
  async function addOne(key: string, file: File) {
    const problem = fileProblem(file);
    batch = [];
    if (problem) { addFailure = problem; return; }
    adding = file.name; running = true;
    try {
      const got = await library.add(key, file, requestFor(file));
      requestKeys.delete(fileKey(file));
      notify(got.created ? "The paper is in the library." : got.restored ? "The paper is back in the library." : "This paper was already in the library.");
      await load();
      openPaper(got.paper.paper_id);
    } catch (cause) {
      addFailure = libraryFailure(cause, "add");
    } finally {
      adding = null; running = false;
    }
  }
  const reading = $derived(batch.findIndex((item) => item.state === "reading"));
</script>

<section class="library-page" aria-labelledby="library-title">
  <header>
    <h1 id="library-title">Library</h1>
    {#if group}<p>Papers {group.name} keeps for its research. {libraryClaim}</p>{/if}
  </header>
  {#if !group}<p class="soft">Your account is not in a lab yet.</p>
  {:else if loading && !view}<p class="soft" role="status">Opening the library…</p>
  {:else if failure}<p class="notice error" role="alert">{failure}</p>
  {:else if view}
    {#if !view.configured}
      <p class="soft">The library is not set up for {group.name} yet. Papers can be added and searched once it is.</p>
    {:else}
      <div class="tools">
        <form class="search" role="search" onsubmit={search} aria-label="Search the library">
          <label class="visually-hidden" for="library-query">Search the lab's papers</label>
          <input id="library-query" type="search" bind:value={query} maxlength="200" placeholder="Search the text of the lab's papers" autocomplete="off" />
          <button class="button" type="submit" disabled={searchBusy}>{searchBusy ? "Searching…" : "Search"}</button>
        </form>
        <div class="add">
          <input bind:this={input} id="library-file" class="visually-hidden" type="file" accept="application/pdf,.pdf" multiple onchange={chosen} disabled={running} />
          <label class="button secondary" for="library-file" aria-disabled={running}>{running ? (batch.length > 1 ? "Reading the papers…" : "Reading the paper…") : "Add papers"}</label>
        </div>
      </div>
      {#if adding && batch.length <= 1}<p class="soft small" role="status">Reading “{adding}”: its sections, pages and figure captions. A long paper can take a minute.</p>{/if}
      {#if batch.length > 1}
        <section class="batch" aria-labelledby="batch-title">
          <h2 id="batch-title" class="group-title">{running ? `Adding ${batch.length} files to ${batchLab}` : `Added to ${batchLab} from ${batch.length} files`}</h2>
          <p class="small soft" role="status">{running && reading >= 0 ? `Reading ${reading + 1} of ${batch.length}. A long paper can take a minute; the others wait their turn.` : batchWords(batch.map((item) => item.state))}</p>
          <ol class="batch-list">
            {#each batch as item, i (i)}
              <li class:open={item.state === "reading"}><span class="name">{item.name}</span><span class="small" class:soft={item.state !== "failed"}>{batchItemWords(item.state, item.problem)}</span></li>
            {/each}
          </ol>
          {#if running}
            <button class="link small" disabled={stopping} onclick={() => (stopping = true)}>{stopping ? "Stopping after this paper…" : "Stop after this paper"}</button>
          {:else}
            <button class="link small" onclick={() => (batch = [])}>Clear this list</button>
          {/if}
        </section>
      {/if}
      {#if addFailure}<p class="notice error" role="alert">{addFailure}</p>{/if}
      {#if searchFailure}<p class="notice error" role="alert">{searchFailure}</p>{/if}
      {#if found}
        <section class="results" aria-labelledby="results-title">
          <h2 id="results-title" class="group-title">Passages for “{found.query}”</h2>
          <div class="results-head">
            <p class="small soft">Ranked by the words they share with the search, not by meaning.</p>
            <button class="link small" onclick={clearSearch}>Clear the search</button>
          </div>
          {#if !found.results.length}
            <p class="soft">No passage in the lab's papers contains these words.</p>
          {:else}
            <ol class="passages">
              {#each found.results as hit, i (i)}
                <li>
                  <a class="link title" href={href({ name: "paper", paper: hit.paper_id })} onclick={(e) => { e.preventDefault(); openPaper(hit.paper_id); }}>{hit.title ?? "Untitled paper"}</a>
                  <p class="small meta">{[hit.heading_path.length ? hit.heading_path.join(" › ") : hit.section, hit.pages].filter(Boolean).join(" · ")}</p>
                  <blockquote>{hit.text}</blockquote>
                </li>
              {/each}
            </ol>
          {/if}
        </section>
      {/if}
    {/if}
    <section aria-labelledby="papers-title">
      <h2 id="papers-title" class="group-title">{view.papers.length ? `Papers (${view.papers.length})` : "Papers"}</h2>
      {#if !view.papers.length}
        <p class="soft empty">{view.configured ? `No paper has been added to ${group.name}'s library yet.` : "No paper yet."}</p>
      {:else}
        {#if curator && unaccepted.length}
          <p class="small waiting">{unacceptedWords(unaccepted.length)}
            <button class="link small" onclick={() => (onlyUnaccepted = !onlyUnaccepted)}>{onlyUnaccepted ? "Show every paper" : "Show only these"}</button>
            {#if acceptable.length && !acceptingAll && !confirmAll}<button class="link small" bind:this={acceptAllButton} onclick={openConfirm}>{acceptable.length === 1 ? "Accept it…" : `Accept all ${acceptable.length}…`}</button>{/if}</p>
          {#if confirmAll}
            <div class="confirm" role="group" aria-labelledby="accept-all-question">
              <p id="accept-all-question" class="small">{acceptable.length === 1 ? "Accept the waiting paper" : `Accept the ${acceptable.length} waiting papers`} for the research agent? It can then read and cite {acceptable.length === 1 ? "it" : "them"} in this lab's projects. Any one can be withdrawn later on its page.</p>
              <div class="actions">
                <button class="button" bind:this={confirmButton} onclick={acceptAll}>{acceptable.length === 1 ? "Accept 1 paper" : `Accept ${acceptable.length} papers`}</button>
                <button class="link small" onclick={cancelConfirm}>Cancel</button>
              </div>
            </div>
          {/if}
          {#if acceptingAll}<p class="small soft" role="status">Accepting {acceptingAll.done + 1} of {acceptingAll.total}…</p>{/if}
        {/if}
        {#if acceptFailures.length}
          <div class="notice error" role="alert">
            <p>{acceptFailures.length === 1 ? "One paper was not accepted:" : `${acceptFailures.length} papers were not accepted:`}</p>
            <ul>{#each acceptFailures as line, i (i)}<li>{line}</li>{/each}</ul>
          </div>
        {/if}
        {#if acceptUnknown.length}
          <div class="notice" role="status">
            <p>{acceptUnknown.length === 1 ? "The answer for one paper was lost" : `The answers for ${acceptUnknown.length} papers were lost`} and the library could not be read again, so whether {acceptUnknown.length === 1 ? "it is" : "they are"} accepted is not known. Reload the library to see.</p>
            <ul>{#each acceptUnknown as line, i (i)}<li>{line}</li>{/each}</ul>
          </div>
        {/if}
        <ol class="papers">
          {#each shown as paper (paper.paper_id)}
            {@const facts = parseWords(paper)}
            <li>
              <a class="link title" href={href({ name: "paper", paper: paper.paper_id })} onclick={(e) => { e.preventDefault(); openPaper(paper.paper_id); }}>{paper.title ?? "Untitled paper"}</a>
              {#if byline(paper)}<p class="small meta">{byline(paper)}</p>{/if}
              {#if paper.labels.length}
                <p class="small labels">{#each paper.labels as label, i (`${label.namespace}:${label.value}`)}{i ? " · " : ""}<span title={labelTitle(label)} class:proposed={label.state === "proposed"}>{label.value}</span>{/each}</p>
              {/if}
              <p class="small soft">{addedWords(paper, day)}{facts ? ` ${facts}.` : ""} {acceptanceWords(paper, day)}</p>
            </li>
          {/each}
        </ol>
      {/if}
      {#if view.removed}<p class="small soft">{view.removed === 1 ? "One paper was" : `${view.removed} papers were`} removed from the library.</p>{/if}
    </section>
    {#if linksFailure}<p class="notice error" role="alert">{linksFailure}</p>{/if}
    {#if linkFailure && !links?.proposals.some((l) => l.relation_id === linkFailure?.id)}
      <!-- The reloaded queue no longer holds the link the message is about, so the message stays here. -->
      <p class="notice error" role="alert">{linkFailure.message}</p>
    {/if}
    {#if links && links.proposals.length}
      <section aria-labelledby="links-title">
        <h2 id="links-title" class="group-title">Proposed links ({links.proposals.length})</h2>
        <p class="small waiting">{queueWords(links)}</p>
        <ol class="links" class:marking={links.can_review}>
          {#each links.proposals as link (link.relation_id)}
            {@const words = linkWords(link)}
            <li class:waits={links.can_review}>
              <p class="claim" id={`link-${link.relation_id}`}>{words.claim}</p>
              <p class="small soft">{words.why}</p>
              {#if links.can_review}
                {#if rejecting === link.relation_id}
                  <form class="reject" aria-label="Reject this link" onsubmit={(e) => { e.preventDefault(); void review(link, false); }}>
                    <label class="small" for={`note-${link.relation_id}`}>Why reject it (optional)</label>
                    <textarea id={`note-${link.relation_id}`} bind:value={rejectNote} maxlength={MAX_REVIEW_NOTE} rows="2"></textarea>
                    <div class="actions">
                      <button class="button" type="submit" disabled={!!reviewing}>{reviewing === link.relation_id ? "Rejecting…" : "Reject the link"}</button>
                      <button class="link small" type="button" onclick={() => { rejecting = ""; rejectNote = ""; }}>Keep it waiting</button>
                    </div>
                  </form>
                {:else}
                  <div class="actions">
                    <button class="button secondary" disabled={!!reviewing} aria-describedby={`link-${link.relation_id}`} onclick={() => review(link, true)}>{reviewing === link.relation_id ? "Accepting…" : "Accept the link"}</button>
                    <button class="link small" disabled={!!reviewing} aria-describedby={`link-${link.relation_id}`} onclick={() => { rejecting = link.relation_id; rejectNote = ""; linkFailure = null; }}>Reject…</button>
                  </div>
                {/if}
              {/if}
              {#if linkFailure?.id === link.relation_id}<p class="notice error" role="alert">{linkFailure.message}</p>{/if}
            </li>
          {/each}
        </ol>
      </section>
    {/if}
  {/if}
</section>

<style>
  .library-page { padding: 40px var(--gutter) 24px; display: grid; gap: 22px; width: 100%; max-width: 1080px; margin: 0 auto; align-content: start; }
  header { display: grid; gap: 10px; }
  h1 { font-size: clamp(30px, calc(3.4vw * var(--type-scale)), calc(42px * var(--type-scale))); letter-spacing: -0.022em; }
  header p, .empty { max-width: 76ch; }
  .tools { display: flex; flex-wrap: wrap; gap: 12px 20px; align-items: center; max-width: 900px; }
  .search { display: flex; gap: 10px; flex: 1 1 360px; min-width: 0; }
  .search input { flex: 1; min-width: 0; min-height: 40px; }
  .add label { cursor: pointer; }
  .add label[aria-disabled="true"] { cursor: progress; opacity: 0.7; }
  .add input:focus-visible + label { outline: 2px solid var(--focus); outline-offset: 2px; }
  .group-title { font-size: calc(19px * var(--type-scale)); font-weight: 700; color: var(--heading); padding-bottom: 8px; border-bottom: 1px solid var(--line); max-width: 900px; }
  .results { display: grid; gap: 10px; }
  .results-head { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: 6px 20px; max-width: 900px; }
  .passages, .papers { list-style: none; margin: 0; padding: 0; display: grid; gap: 20px; max-width: 900px; }
  .passages li, .papers li { display: grid; gap: 4px; padding-left: 14px; border-left: 1px dashed var(--line-2); justify-items: start; min-width: 0; }
  .title { font-weight: 600; font-size: calc(16px * var(--type-scale)); text-align: left; overflow-wrap: anywhere; }
  .meta { color: var(--text-2); max-width: 80ch; }
  blockquote { margin: 4px 0 0; font-family: var(--serif); font-size: calc(15.5px * var(--type-scale)); line-height: 1.6; color: var(--text); max-width: 72ch; overflow-wrap: anywhere; }
  .labels { color: var(--text-2); }
  .labels .proposed { font-style: italic; color: var(--text-3); }
  .waiting { color: var(--text-2); max-width: 900px; margin: 10px 0 14px; }
  .waiting .link + .link { margin-left: 14px; }
  .links { list-style: none; margin: 0; padding: 0; display: grid; gap: 20px; max-width: 900px; }
  .links li { position: relative; display: grid; gap: 4px; padding-left: 14px; border-left: 1px dashed var(--line-2); justify-items: start; min-width: 0; }
  /* A reviewer's waiting links carry the editor's change bar, as anything that needs the reader does. */
  .links.marking li.waits::before { content: ""; position: absolute; left: -16px; top: 3px; bottom: 3px; width: 3px; background: var(--mark); }
  .claim { font-family: var(--serif); font-size: calc(15.5px * var(--type-scale)); line-height: 1.55; max-width: 72ch; overflow-wrap: anywhere; }
  .links .actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; margin-top: 4px; }
  .reject { display: grid; gap: 6px; justify-self: stretch; max-width: 60ch; margin-top: 4px; }
  .reject textarea { width: 100%; min-height: 56px; }
  .batch { display: grid; gap: 10px; max-width: 900px; justify-items: start; }
  .batch-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 6px; justify-self: stretch; }
  .batch-list li { display: flex; flex-wrap: wrap; gap: 2px 14px; align-items: baseline; padding-left: 14px; border-left: 1px dashed var(--line-2); min-width: 0; }
  .batch-list li.open { border-left: 3px solid var(--mark); padding-left: 12px; }
  .batch-list .name { font-weight: 600; overflow-wrap: anywhere; }
  .confirm { display: grid; gap: 8px; max-width: 72ch; margin: 0 0 14px; padding-left: 14px; border-left: 3px solid var(--mark); }
  .confirm .actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; }
</style>
