<script lang="ts">
  import type { Snippet } from "svelte";
  import type { Link } from "./follow.ts";
  import { tallyWords as wordsFor } from "./live_words.ts";
  import { standingNames, standingTone, type Standing, type StepMark } from "./standing.ts";
  import StatusMark from "./StatusMark.svelte";
  import { when } from "./language.ts";

  // A project's live status (Glass Box rule 2): where it stands, one sentence, what the system does in the current step
  // and for how long, the steps against the plan, how long recent analyses waited and ran, and the action that fits. When the block scrolls under the bar a
  // compact line keeps the standing, the sentence and that action in view.
  let { standing, sentence, seen = "", now = null, pace = null, steps, stepWords, link, lastRead, hint = "", actions, pinned = null }: {
    standing: Standing; sentence: string; seen?: string; now?: { text: string; time: string; note?: string } | null; pace?: string | null;
    steps: { mark: StepMark; title: string; label: string }[]; stepWords: string; link: Link; lastRead: string; hint?: string;
    actions: Snippet; pinned?: Snippet | null;
  } = $props();

  // Few steps read best named under their marks; a long plan reads as one bar with counts. Never a share of a whole.
  const LABELED = 6;
  const tallyWords = $derived(wordsFor(steps.map((s) => s.mark)));

  let block = $state<HTMLElement>();
  let stuck = $state(false);
  $effect(() => {
    const el = block;
    if (!el) return;
    const bar = () => document.querySelector("header.bar")?.getBoundingClientRect().bottom ?? 0;
    let observer: IntersectionObserver | null = null;
    const watch = () => {
      observer?.disconnect();
      observer = new IntersectionObserver(([entry]) => { stuck = !entry.isIntersecting && entry.boundingClientRect.top < bar(); },
        { rootMargin: `-${Math.round(bar())}px 0px 0px 0px` });
      observer.observe(el);
    };
    watch();
    addEventListener("resize", watch);
    return () => { observer?.disconnect(); removeEventListener("resize", watch); };
  });
</script>

<div class="pin-track">
  <div class="pin" class:shown={stuck} aria-hidden={!stuck} inert={!stuck}>
    <StatusMark tone={standingTone[standing]} label={standingNames[standing]} />
    <span class="pin-sentence">{sentence}</span>
    {#if pinned}<span class="pin-action">{@render pinned()}</span>{/if}
  </div>
</div>

<section id="section-summary" class="live" aria-labelledby="summary-title" bind:this={block}>
  <div class="first">
    <h2 id="summary-title" class="standing" class:attention={standing === "waiting"}><StatusMark tone={standingTone[standing]} label={standingNames[standing]} /></h2>
    <p class="sentence">{sentence}</p>
  </div>
  {#if seen}<p class="small soft">Seen by {seen}.</p>{/if}
  {#if now}<p class="doing"><strong>{now.text}</strong><span class="timer">{now.time}</span>{#if now.note}<span class="note">{now.note}</span>{/if}</p>{/if}
  {#if steps.length}
    <div class="steps">
      {#if steps.length <= LABELED}
        <ol class="stepline" aria-label="The agent's steps">
          {#each steps as step, i (i)}<li class={step.mark}><span class="visually-hidden">Step {i + 1}, {step.label}: </span><span class="step-name" aria-hidden="true">{i + 1} · {step.title}</span><span class="visually-hidden">{step.title}</span></li>{/each}
        </ol>
      {:else}
        <ol class="many" style:--steps={steps.length} aria-label={`The agent's steps: ${tallyWords.join(", ")}`}>
          {#each steps as step, i (i)}<li class={step.mark} title={`Step ${i + 1}: ${step.title}`}><span class="visually-hidden">Step {i + 1}, {step.label}: {step.title}</span></li>{/each}
        </ol>
        <p class="tally">{#each tallyWords as words (words)}<span>{words}</span>{/each}<span class="soft">The plan can change.</span></p>
      {/if}
      <p class="step-words">{stepWords}</p>
    </div>
  {/if}
  {#if pace}<p class="pace">{pace}</p>{/if}
  {#if link === "reconnecting" || link === "polling"}
    <p class="small soft">{link === "reconnecting" ? "Reconnecting for live updates." : "Live updates are unavailable."} This page was last updated {when(lastRead)}.</p>
  {/if}
  <div class="actions">{@render actions()}{#if hint}<p class="hint">{hint}</p>{/if}</div>
</section>

<style>
  /* The compact line rides under the bar; it takes no room in the page until it shows. */
  .pin-track { position: sticky; top: var(--bar-h); z-index: 7; height: 0; margin-bottom: calc(-1 * var(--page-gap, 26px)); }
  .pin { position: absolute; inset: 0 auto auto 0; width: 100%; display: flex; align-items: center; gap: 12px; min-height: 48px;
    padding: 6px 14px; border-radius: 0 0 var(--r-3) var(--r-3); background: var(--glass); box-shadow: var(--group-edge);
    -webkit-backdrop-filter: saturate(1.6) blur(14px); backdrop-filter: saturate(1.6) blur(14px);
    opacity: 0; transform: translateY(-6px); pointer-events: none; transition: opacity 160ms var(--ease), transform 160ms var(--ease); }
  .pin.shown { opacity: 1; transform: none; pointer-events: auto; }
  .pin :global(.status) { font-weight: 700; color: var(--heading); white-space: nowrap; }
  .pin-sentence { flex: 1; min-width: 0; font-size: var(--font-small); color: var(--text-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .pin-action { flex: none; }
  @media (prefers-reduced-motion: reduce) { .pin { transition: none; transform: none; } }

  /* Not `.status`: that global class belongs to StatusMark and sets no-wrap. */
  .live { display: grid; grid-template-columns: minmax(0, 1fr); gap: 10px; padding: 16px 18px; border-radius: var(--r-3); background: var(--surface); box-shadow: var(--group-edge); }
  .first { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 14px; }
  .standing :global(.status) { font-size: calc(18px * var(--type-scale)); font-weight: 700; color: var(--heading); gap: 9px; white-space: nowrap; }
  .standing :global(.status .glyph) { width: 13px; height: 13px; }
  .standing :global(.status .waveform) { height: 15px; }
  .sentence { font-size: calc(15px * var(--type-scale)); color: var(--text); min-width: 0; }
  .doing { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 10px; font-size: var(--font-small); color: var(--text-2); }
  .doing strong { font-weight: 600; color: var(--text); }
  .timer, .note { color: var(--text-3); font-variant-numeric: tabular-nums; }
  .pace { font-size: var(--font-small); color: var(--text-2); max-width: 72ch; font-variant-numeric: tabular-nums; }
  .steps { display: grid; gap: 8px; }
  .stepline { list-style: none; margin: 2px 0 0; padding: 0; display: grid; grid-auto-flow: column; grid-auto-columns: minmax(0, 1fr); gap: 8px; }
  .stepline li { display: grid; gap: 6px; align-content: start; font-size: var(--font-xs); line-height: 1.35; color: var(--text-3); min-width: 0; }
  .stepline li::before, .many li { content: ""; height: 4px; border-radius: 2px; background: var(--line-2); transition: background-color 220ms var(--ease); }
  .stepline li.done { color: var(--text-2); }
  .stepline li.current { color: var(--text); font-weight: 600; }
  .stepline li.done::before, .many li.done { background: var(--heading); }
  .stepline li.current::before, .many li.current { background: var(--active); }
  .stepline li.failed::before, .many li.failed { background: var(--failed); }
  .stepline li.check::before, .many li.check { background: var(--mark); }
  .step-name { overflow-wrap: anywhere; }
  .many { list-style: none; margin: 2px 0 0; padding: 0; display: grid; grid-template-columns: repeat(var(--steps), minmax(0, 1fr)); gap: 3px; }
  .tally { display: flex; flex-wrap: wrap; gap: 4px 16px; font-size: var(--font-xs); color: var(--text-2); font-variant-numeric: tabular-nums; }
  .step-words { font-size: var(--font-small); color: var(--text-2); }
  .actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 10px; margin-top: 2px; }
  .hint { flex: 1 1 22ch; font-size: var(--font-xs); color: var(--text-3); max-width: 52ch; }
  @media (prefers-reduced-motion: reduce) { .stepline li::before, .many li { transition: none; } }
  @media (max-width: 640px) {
    .live { padding: 14px; }
    /* Named steps do not fit side by side on a phone; the marks stay and the line under them names the current step. */
    .stepline .step-name { display: none; }
    .pin { padding: 6px 12px; gap: 10px; }
    .pin-sentence { display: none; }
    .pin-action { margin-left: auto; }
  }
</style>
