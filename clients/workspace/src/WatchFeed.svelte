<script lang="ts">
  import { when } from "./language.ts";
  import { watchKindNames, type WatchLine } from "./watch.ts";

  // Watching it work (Glass Box, long runs rule 3): the newest lines of the project's record as they are written. A
  // line that arrives while it is open slides in at the top and is read out once; the full record keeps every line.
  let { lines, close, openTask, link, showRecord }: {
    lines: WatchLine[]; close: () => void; openTask: (task: string) => void; link: (task: string) => string; showRecord: () => void;
  } = $props();

  const SHOWN = 12;
  const shown = $derived(lines.slice(0, SHOWN));
  const clock = (at: string) => {
    const date = new Date(at);
    return date.toDateString() === new Date().toDateString() ? date.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" }) : when(at);
  };

  // The newest recorded line, read out when it changes while the feed is open; the line about what runs now is in the
  // status line already.
  const newest = $derived(lines.find((l) => l.kind !== "now"));
  let heard = "", announcement = $state("");
  $effect(() => {
    const line = newest;
    if (!line) return;
    if (heard && heard !== line.key) announcement = `New in the record: ${line.text}`;
    heard = line.key;
  });

  function arrive(_node: Element) {
    const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
    return { duration: still ? 0 : 220, easing: (t: number) => 1 - (1 - t) ** 3, css: (t: number) => `opacity: ${t}; transform: translateY(${(1 - t) * -6}px)` };
  }
</script>

<section id="section-watch" class="watch" aria-labelledby="watch-title">
  <div class="head">
    <h2 id="watch-title" class="section-title" tabindex="-1">Watching it work</h2>
    <p class="small soft">The record as it is written, newest first. Agent lines are its reading; the others are records.</p>
    <button class="link small close" onclick={close}>Close</button>
  </div>
  {#if shown.length}
    <ol class="feed" aria-label="The record, newest first">
      {#each shown as line (line.key)}
        <li class={`${line.kind} ${line.tone}`} in:arrive>
          {#if line.kind === "now"}<span class="time">now</span>{:else}<time class="time" datetime={line.at}>{clock(line.at)}</time>{/if}
          <span class="text">{#if line.kind === "now"}<span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span>{/if}{#if line.taskId}<a class="line-link" href={link(line.taskId)}
            onclick={(event) => { event.preventDefault(); openTask(line.taskId!); }}>{line.text}</a>{:else}{line.text}{/if}</span>
          <span class="kind">{watchKindNames[line.kind]}</span>
        </li>
      {/each}
    </ol>
    {#if lines.length > SHOWN}<button class="link small more" onclick={showRecord}>Every entry is in the full record</button>{/if}
  {:else}
    <p class="soft">Nothing recorded yet.</p>
  {/if}
  <p class="visually-hidden" aria-live="polite">{announcement}</p>
</section>

<style>
  .watch { display: grid; gap: 12px; }
  .head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 14px; }
  .head p { flex: 1 1 28ch; min-width: 0; }
  .close { margin-left: auto; min-height: 44px; }
  .feed { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); }
  .feed li { display: grid; grid-template-columns: 76px minmax(0, 1fr) auto; align-items: first baseline; gap: 4px 16px; padding: 10px 0; border-bottom: 1px solid var(--line); }
  .time { font-size: var(--font-xs); color: var(--text-3); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .text { min-width: 0; font-size: var(--font-small); line-height: 1.5; color: var(--text); overflow-wrap: anywhere; padding-left: 12px; border-left: 1px solid var(--line-2); }
  /* The agent's lines are its reading, drawn with the dashed rule the full record uses for them. */
  .agent .text { border-left-style: dashed; }
  .attention .text { box-shadow: inset 2px 0 0 var(--mark); }
  .failed .text, .failed .line-link { color: var(--failed); }
  .now .text { font-weight: 600; color: var(--heading); }
  .now .waveform { margin-right: 8px; vertical-align: -1px; }
  .now .time { font-weight: 600; color: var(--heading); }
  .kind { font-size: var(--font-xs); color: var(--text-3); white-space: nowrap; }
  .line-link { color: inherit; text-decoration: underline; text-decoration-thickness: 1px; text-underline-offset: 3px;
    text-decoration-color: transparent; transition: text-decoration-color var(--t-fast) var(--ease); }
  .line-link:hover { text-decoration-color: var(--mark); }
  .line-link:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; border-radius: 2px; }
  .more { justify-self: start; min-height: 44px; }
  @media (max-width: 640px) {
    .feed li { grid-template-columns: 56px minmax(0, 1fr); row-gap: 0; }
    /* The kind sits under the line, inside the same rule. */
    .kind { grid-column: 2; padding: 3px 0 0 13px; border-left: 1px solid var(--line-2); }
    .agent .kind { border-left-style: dashed; }
  }
  @media (prefers-reduced-motion: reduce) { .line-link { transition: none; } }
</style>
