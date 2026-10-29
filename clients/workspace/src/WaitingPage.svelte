<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { flip } from "svelte/animate";
  import { BrokerError } from "./api.ts";
  import type { Identity } from "./contracts.ts";
  import { forLab, inbox, itemKey, itemLink, itemSentence, kindHeading, kindOrder, ordered, type InboxItem } from "./inbox.ts";
  import { lab } from "./lab.svelte.ts";
  import { when } from "./language.ts";
  import { live, mayBeOld } from "./live.ts";
  import { arrive, closeGap, leave } from "./motion.ts";
  import { waiting } from "./waiting.svelte.ts";

  // What waits for this reader in the lab in view (never another lab's items
  // unasked). Other labs' counts are in the lab menu; `labs` names the labs a widened index showed, so its link opens
  // the same labs. The broker decides every item and who may act on it; marking an item as seen records that this
  // person saw its current version. The page shows the workspace's one copy of the list (waiting.svelte.ts), which
  // every change reads again, so an item the reader dealt with elsewhere is gone when they arrive, and one that arrives
  // while they read rises into place.
  let { identity, notify, labs }: { identity: Identity; notify: (message: string, error?: boolean) => void; labs?: string[] } = $props();

  let marking = $state("");
  const keys = new Map<string, string>();
  // The first answer arrives still; later arrivals rise into place and are announced once.
  let settled = $state(false);
  let arrived = $state("");
  onMount(() => {
    if (mayBeOld(waiting.readAt, live().link, Date.now())) void waiting.load();
    void tick().then(() => { settled = waiting.phase === "ready"; });
  });
  const chosen = $derived(labs?.length && !(labs.length === 1 && labs[0] === lab.key) ? labs : null);
  const items = $derived(chosen ? waiting.items.filter((i) => chosen.includes(i.lab)) : forLab(waiting.items, lab.key));
  const scope = $derived(chosen ? chosen.join(",") : lab.key);
  // Arrivals are counted within one view: changing lab (the operator lab adds its own items) is the reader's choice, not
  // news, and the list for it appears at once (the keyed block below runs no item's transition).
  let shownKeys: Set<string> | null = null, shownLab = "";
  $effect(() => {
    const now = items, ready = waiting.phase === "ready", current = scope;
    untrack(() => {
      if (!ready) return;
      const fresh = shownKeys && shownLab === current ? now.filter((i) => !shownKeys!.has(itemKey(i))).length : 0;
      shownKeys = new Set(now.map(itemKey));
      shownLab = current;
      if (fresh) {
        // Cleared first, so the same words for a later arrival are read out again.
        const words = fresh === 1 ? "One new item waits for you." : `${fresh} new items wait for you.`;
        arrived = "";
        void tick().then(() => { arrived = words; });
      }
      if (!settled) void tick().then(() => { settled = true; });
    });
  });

  const labName = (key: string) => identity.groups.find((g) => g.key === key)?.name ?? "a lab";
  const groups = $derived(kindOrder.map((kind) => ({ kind, items: ordered(items.filter((i) => i.kind === kind)) })).filter((g) => g.items.length));

  const link = itemLink;

  async function markSeen(item: InboxItem) {
    const key = itemKey(item);
    if (marking) return;
    if (!keys.has(key)) keys.set(key, crypto.randomUUID());
    marking = key;
    try {
      const seen = await inbox.acknowledge(item, keys.get(key)!);
      waiting.seen(item, seen.seen_at);
    } catch (cause) {
      const code = cause instanceof BrokerError ? cause.code : "";
      notify(code === "inbox_item_not_found" ? "That item changed a moment ago. The list shows it as it is now." : "It could not be marked as seen. Try again.", true);
      if (code === "inbox_item_not_found") void waiting.load();
    } finally { marking = ""; }
  }
</script>

<section class="waiting-page" aria-labelledby="waiting-title">
  <header>
    <h1 id="waiting-title">Waiting for you</h1>
    {#if chosen}
      <p>What asks for you in {chosen.map(labName).join(", ").replace(/, ([^,]*)$/, " and $1")}, the labs your filter shows:
        the agent's requests for review, results no person has checked, and the labs' papers and notes that need a lead or
        reviewer. <a class="link" href="#/waiting">Only {labName(lab.key)}</a></p>
    {:else}
      <p>What asks for you in this lab: the agent's requests for review, results no person has checked, and the lab's papers
        and notes that need a lead or reviewer. Your other labs' counts are in the lab menu.</p>
    {/if}
    <p>Marking an item as seen tells your colleagues you saw it; the item stays until someone deals with it.</p>
  </header>
  {#if waiting.phase === "idle"}<p class="soft" role="status">Opening what waits for you…</p>
  {:else if waiting.phase === "failed"}<p class="notice error" role="alert">What waits for you could not be loaded. Nothing has changed; reload to try again.</p>
  {:else if !groups.length}<p class="soft empty" in:arrive={{ skip: !settled }}>Nothing waits for you right now.</p>
  {:else}
    {#key scope}{#each groups as group (group.kind)}
      <section class="group" aria-labelledby={`waiting-${group.kind}`} animate:flip={closeGap()} in:arrive={{ skip: !settled }} out:leave>
        <h2 class="group-title" id={`waiting-${group.kind}`}>{kindHeading[group.kind]} <span class="count">{group.items.length}</span></h2>
        <ol class="items">
          {#each group.items as item (itemKey(item))}
            {@const to = link(item)}
            <li class:seen={!!item.seen_at} animate:flip={closeGap()} in:arrive={{ skip: !settled }} out:leave>
              <p class="what">{itemSentence(item, labName)}</p>
              {#if item.kind === "review_request" && item.reason}
                <p class="reason"><span>{item.reason_source === "methods_check" ? "The methods check's words." : "The agent's words."}</span> {item.reason}</p>
              {/if}
              <p class="small meta">{`${labName(item.lab)} · ${when(item.at)}${item.seen_at ? ` · Seen ${when(item.seen_at)}` : ""}`}</p>
              <div class="actions">
                {#if to}<a class="link small" href={to.href}>{to.label}</a>{/if}
                {#if waiting.canAcknowledge && !item.seen_at}
                  <button class="link small" onclick={() => markSeen(item)} disabled={marking === itemKey(item)}>{marking === itemKey(item) ? "Marking…" : "Mark as seen"}</button>
                {/if}
              </div>
            </li>
          {/each}
        </ol>
      </section>
    {/each}{/key}
    {#if !waiting.canAcknowledge}<p class="small soft">This account is a service account: it sees these items but cannot mark them as seen.</p>{/if}
  {/if}
  <p class="visually-hidden" role="status" aria-live="polite">{arrived}</p>
</section>

<style>
  .waiting-page { padding: 40px var(--gutter) 24px; display: grid; gap: 22px; width: 100%; max-width: 1080px; margin: 0 auto; align-content: start; }
  header { display: grid; gap: 10px; }
  h1 { font-size: clamp(30px, calc(3.4vw * var(--type-scale)), calc(42px * var(--type-scale))); letter-spacing: -0.022em; }
  header p, .empty { max-width: 76ch; }
  .group { display: grid; gap: 22px; }
  .group-title { font-size: calc(19px * var(--type-scale)); font-weight: 700; color: var(--heading); padding-bottom: 8px; border-bottom: 1px solid var(--line); max-width: 900px; display: flex; align-items: baseline; gap: 10px; }
  .count { font-size: var(--font-small); font-weight: 600; color: var(--text-2); font-variant-numeric: tabular-nums; }
  .items { list-style: none; margin: 0; padding: 0; display: grid; gap: 20px; max-width: 900px; }
  .items li { display: grid; gap: 6px; padding-left: 14px; border-left: 2px solid var(--mark); justify-items: start; min-width: 0; }
  .items li.seen { border-left-color: var(--line-2); }
  .items li.seen .what { color: var(--text-2); }
  .what { font-weight: 600; color: var(--heading); max-width: 76ch; }
  .reason { font-family: var(--serif); font-size: calc(16px * var(--type-scale)); line-height: 1.55; max-width: 72ch; color: var(--text); }
  .reason span { font-family: var(--ui); font-size: var(--font-xs); font-weight: 650; color: var(--text-2); font-style: italic; margin-right: 4px; }
  .meta { max-width: 80ch; color: var(--text-2); }
  .actions { display: flex; gap: 6px 22px; flex-wrap: wrap; }
</style>
