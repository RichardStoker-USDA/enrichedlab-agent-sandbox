<script lang="ts">
  import { untrack } from "svelte";
  import AnswerTableView from "./AnswerTableView.svelte";
  import { broker, BrokerError, verifiedArtifact, verifiedMember } from "./api.ts";
  import { isPackage, type Artifact, type GeneratedCode as Generated, type Identity, type PackageMember, type Task } from "./contracts.ts";
  import EvidencePackage from "./EvidencePackage.svelte";
  import FileView from "./FileView.svelte";
  import { artifactResult, memberResult, sameValues } from "./results.ts";
  import GeneratedCode from "./GeneratedCode.svelte";
  import { agentNotes } from "./history.ts";
  import { bytes, deferralLabel, fileTitle, isModelRetry, lessonLabel, methodsCheckWords, reviewFindingLine, runSentence, stepStatus, taskStatus, when } from "./language.ts";
  import { integritySentence, packageHolds } from "./package_view.ts";
  import PeoplePanel from "./PeoplePanel.svelte";
  import { findingFlag, reviewMark } from "./person_review.ts";
  import RecordedSteps from "./RecordedSteps.svelte";
  import StatusMark from "./StatusMark.svelte";
  import { fromArtifact, fromMember, saveBlob, saveName } from "./viewer.ts";
  import WbFiles from "./WbFiles.svelte";
  import { factsOf, filesFor, ids, labTasks, stepTitleFor, taskOf, type ProjectModel, type WbNode } from "./workbench.ts";
  import { answerHistory, answerWords, artifactOf, display, glued, parts, partStateWords, referencedArtifacts, shownFindings, shownTables, sourceWords, type AnswerTable, type ArtifactDigest, type Binding } from "./findings.ts";
  import { projects } from "./projects.ts";
  import { inputWords, standingNames, standingOf, standingTone } from "./standing.ts";

  // The editor pane for one record. Files are read only through the broker's verified routes; every other view is
  // drawn from the project's saved records. Agent text is always labelled as the agent's reading.
  let { node, model, identity, open, preview = open, openClassic, notify, reload, since = null }: {
    node: WbNode; model: ProjectModel; identity: Identity;
    open: (id: string) => void; preview?: (id: string) => void; openClassic: (hash: string) => void;
    notify: (text: string, error?: boolean) => void; reload: () => void; since?: string | null;
  } = $props();

  const title = $derived(stepTitleFor(model, identity.profiles));
  const research = $derived(model.project.checkpoint?.state ?? null);
  const groupName = $derived(identity.groups.find((g) => g.key === model.project.group_key)?.name ?? "Research group");
  const evidence = $derived(node.taskId ? model.evidence[node.taskId] : undefined);
  const pkg = $derived(evidence?.packages.find((p) => p.artifact.id === node.artifactId));
  const member = $derived(pkg && node.memberPath ? pkg.contents.listing.members.find((m) => m.path === node.memberPath) : undefined);
  const originalMember = $derived(pkg && node.originalPath ? pkg.contents.listing.members.find((m) => m.path === node.originalPath) : undefined);
  // The member in its result frame: the broker's pages, the method's declared views and the package's files that hold
  // the same numbers (Glass Box, results rules 1 and 2). A figure keeps its PDF original.
  const memberFile = $derived.by(() => {
    if (!member || !pkg || !node.taskId) return null;
    const file = memberResult(node.taskId, pkg.artifact, member, node.label, () => save({ artifact: pkg.artifact, member }));
    return originalMember ? { ...file, original: { file: fromMember(originalMember, node.label), download: () => save({ artifact: pkg.artifact, member: originalMember }) } } : file;
  });
  const memberGroup = $derived(memberFile && pkg && node.taskId ? sameValues(memberFile, pkg.contents.listing.members.filter((m) => m.path !== memberFile.file.path)
    .map((m) => memberResult(node.taskId!, pkg.artifact, m, fileTitle(m.path), () => save({ artifact: pkg.artifact, member: m })))) : []);
  // A stop for review asks this reader only when they may steer or review the project.
  const canAct = $derived(model.project.capabilities.includes("steer") || model.project.capabilities.includes("review"));
  const artifact = $derived(evidence?.artifacts.find((a) => a.id === node.artifactId));
  const task = $derived(node.taskId ? taskOf(model, node.taskId) : undefined);
  const runEntry = $derived(node.runId ? model.runs.find(({ run }) => run.run_id === node.runId) : undefined);
  const turn = $derived(runEntry && node.turn !== undefined ? runEntry.turns.find((t) => t.turn === node.turn) : undefined);

  // Records the editor reads on demand: the full task for its recorded steps, and model-written code.
  let fullTask = $state<Task | null>(null);
  let generated = $state<Generated | null>(null);
  let lazyFailure = $state("");
  let packageOpen = $state(false);
  $effect(() => {
    const kind = node.kind, taskId = node.taskId;
    const c = new AbortController();
    untrack(() => {
      fullTask = null; generated = null; lazyFailure = "";
      if (!taskId) return;
      if (kind === "steps") broker.task(taskId, c.signal).then((t) => (fullTask = t)).catch(() => { if (!c.signal.aborted) lazyFailure = "The recorded steps could not be read."; });
      if (kind === "code") broker.generatedCode(taskId, c.signal).then((g) => (generated = g)).catch(() => { if (!c.signal.aborted) lazyFailure = "The model-written code could not be read."; });
    });
    return () => c.abort();
  });

  async function save(target: { artifact: Artifact; member?: PackageMember }) {
    if (!node.taskId) return;
    try {
      const blob = target.member ? await verifiedMember(node.taskId, target.artifact, target.member) : await verifiedArtifact(node.taskId, target.artifact);
      saveBlob(blob, target.member ? saveName(target.member.path) : saveName(target.artifact.relative_path) + (isPackage(target.artifact) && !/\.[a-z0-9]+$/i.test(target.artifact.relative_path) ? ".tar.gz" : ""));
      notify("Downloaded after checking its recorded checksum.");
    } catch (cause) {
      notify(cause instanceof BrokerError && cause.code === "use_verified_cli" ? "This file is larger than the browser can check. Use the verified command-line download." : "The file did not match its recorded checksum or is unavailable. Nothing was saved.", true);
    }
  }

  // The Overview: where the project stands and the agent's current answer, its values read from the stored digests of
  // the result tables they name, never from the agent's text.
  const facts = $derived(factsOf(model));
  const standing = $derived(standingOf(facts));
  // The project page's sentence for the same records, so both views say the same thing.
  const standingWords = $derived(standing === "finished" && facts.latest_run?.status === "review_required"
    ? "A person accepted its results. The agent does nothing more until someone tells it to continue."
    : runSentence(facts.latest_run ?? undefined, model.project.tasks.some((t) => ["queued", "leased", "starting", "running", "cancelling"].includes(t.status)), canAct).text);
  const answers = $derived(answerHistory(model.runs));
  const answer = $derived(answers.current);
  let digests = $state<Record<string, ArtifactDigest>>({});
  $effect(() => {
    if (node.kind !== "question") return;
    const wanted = referencedArtifacts(shownFindings(answers), shownTables(answers)).filter((a) => !digests[a.ref]);
    if (!wanted.length) return;
    const c = new AbortController();
    void Promise.all(wanted.map((a) => projects.digest(a.task, a.artifact, c.signal).then((d) => [a.ref, d] as const).catch(() => null)))
      .then((got) => { if (!c.signal.aborted) digests = { ...digests, ...Object.fromEntries(got.filter((g): g is readonly [string, ArtifactDigest] => !!g)) }; });
    return () => c.abort();
  });
  const accepted = $derived.by(() => {
    const read = [...new Set([...(answer?.findings.flatMap((f) => Object.values(f.bindings).map((b) => artifactOf(b.ref)?.task ?? "")) ?? []),
      ...(answer?.tables.map((t) => artifactOf(t.ref)?.task ?? "") ?? [])].filter(Boolean))];
    return read.length > 0 && read.every((t) => { const r = model.reviews?.[t]; return !!r && r.reviewer_kind === "person" && r.decision === "accepted" && r.covers_current_outputs; });
  });
  // A value opens the table it was read from, inside the package that holds it.
  function source(binding: Binding) {
    const a = artifactOf(binding.ref);
    if (a) open(ids.file(model.project.project_id, a.task, a.artifact, binding.table));
  }
  // An answer table opens the same way: the table inside its package, read from the verified file.
  function tableSource(table: AnswerTable) {
    const a = artifactOf(table.ref);
    return a ? { from: title(a.task), label: "Open the table", open: () => open(ids.file(model.project.project_id, a.task, a.artifact, table.table)) } : null;
  }
  // A person's review of the table's analysis marks it as on the project page.
  function tableFlag(table: AnswerTable) {
    const a = artifactOf(table.ref);
    return a ? findingFlag([a.task], model.reviews ?? {}, "table") : null;
  }
  const fileGroups = $derived(node.kind === "files" ? filesFor(model, identity.profiles, since) : []);
  const dataUsed = $derived(node.kind === "data" ? [...model.project.tasks, ...labTasks(model)].flatMap((t) => (model.evidence[t.task_id]?.packages ?? [])
    .flatMap((pkg) => pkg.view.inputs.map((m) => ({ task: t, pkg, member: pkg.contents.listing.members.find((x) => x.path === m.path) ?? null, path: m.path })))) : []);
  const latestRun = $derived(model.runs[0]?.run);
  const anyActive = $derived(model.project.tasks.some((t) => ["queued", "leased", "starting", "running", "cancelling"].includes(t.status)));
</script>

<div class="view" data-kind={node.kind}>
  {#if node.kind === "file"}
    {#if member && pkg}
      <FileView file={fromMember(member, node.label)} load={(signal) => verifiedMember(node.taskId!, pkg.artifact, member, signal)} title={node.label}
        onDownload={() => save({ artifact: pkg.artifact, member })}
        original={originalMember ? fromMember(originalMember, node.label) : null} onOriginal={originalMember ? () => save({ artifact: pkg.artifact, member: originalMember }) : undefined}
        result={memberFile} group={memberGroup} />
      {#each originalMember ? [member, originalMember] : [member] as m (m.path)}
        <p class="machine">{m.path} · {bytes(m.size_bytes)} · SHA-256 {m.sha256.slice(0, 16)}… ·
          {m.manifest === "matched" ? "matches the package checksum list" : m.manifest === "mismatched" ? "does not match the package checksum list" : m.manifest === "unlisted" ? "not on the package checksum list" : "checked when read"}</p>
      {/each}
    {:else if artifact && isPackage(artifact)}
      <h1>{artifact.label}</h1>
      <p class="lede">The complete retained analysis package ({bytes(artifact.size_bytes)}): {packageHolds(pkg?.view)}. Extract it and follow its README to rerun the analysis on your computer.</p>
      <p><button class="button" onclick={() => save({ artifact })}>Download the analysis package</button></p>
      {#if pkg}<p class="soft small">{integritySentence(pkg.view.integrity)}</p>{/if}
    {:else if artifact}
      <FileView file={fromArtifact(artifact)} load={(signal) => verifiedArtifact(node.taskId!, artifact, signal)} title={node.label} onDownload={() => save({ artifact })}
        result={artifactResult(node.taskId!, artifact, () => save({ artifact }))} />
    {:else}
      <p class="soft">This file is no longer listed for the analysis.</p>
    {/if}

  {:else if node.kind === "analysis" && task}
    {@const status = taskStatus(task.status)}
    {@const review = model.reviews?.[task.task_id]}
    <p class="running">{model.project.title} · Analysis</p>
    <h1>{title(task.task_id)}</h1>
    <div class="now"><StatusMark tone={status.tone} label={status.label} />
      {#if review}{@const mark = reviewMark(review, identity.principal.id)}<StatusMark tone={mark.tone} label={mark.label} wrap />{/if}
      <span class="soft small">{identity.profiles.find((p) => p.key === task.profile)?.name ?? task.profile} · requested {when(task.submitted_at)} · updated {when(task.updated_at)}</span></div>
    {#each evidence?.packages ?? [] as p (p.artifact.id)}
      <section><h2>{p.artifact.label}</h2>
        <p>{p.view.figures.length} figure{p.view.figures.length === 1 ? "" : "s"}, {p.view.tables.length} table{p.view.tables.length === 1 ? "" : "s"}, {p.view.code.length} code file{p.view.code.length === 1 ? "" : "s"}, {p.view.inputs.length} input{p.view.inputs.length === 1 ? "" : "s"}.</p>
        <p class="soft small">{integritySentence(p.view.integrity)}</p></section>
    {:else}
      <p class="soft">{["queued", "leased", "starting", "running", "cancelling"].includes(task.status) ? "Still running. Its files appear in the explorer when it finishes." : "No retained package is listed for this analysis."}</p>
    {/each}
    <p class="links">
      <button class="link" onclick={() => open(ids.steps(model.project.project_id, task.task_id))}>Recorded steps</button>
      <button class="link" onclick={() => openClassic(`#/projects/${model.project.project_id}/analyses/${task.task_id}`)}>Open the analysis page</button>
    </p>

  {:else if node.kind === "steps"}
    <h1>Recorded steps</h1>
    <p class="soft small">{title(node.taskId ?? "")}</p>
    {#if fullTask}<RecordedSteps task={fullTask} artifacts={evidence?.artifacts ?? []} onDownload={(a) => save({ artifact: a })} />
    {:else if lazyFailure}<p class="notice error" role="alert">{lazyFailure}</p>
    {:else}<p class="soft" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Reading the recorded steps…</p>{/if}

  {:else if node.kind === "code"}
    {#if generated}
      <GeneratedCode taskId={node.taskId!} code={generated} pkg={evidence?.packages.find((p) => p.view.template) ?? null}
        onDownload={() => { saveBlob(new Blob([generated!.content], { type: "text/plain" }), "block.R"); notify(generated!.configuration ? "Downloaded the configured recipe." : "Downloaded the model-written block."); }} />
    {:else if lazyFailure}<p class="notice error" role="alert">{lazyFailure}</p>
    {:else}<p class="soft" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Reading the admitted block…</p>{/if}

  {:else if node.kind === "turn" && turn && runEntry}
    <p class="running">Research agent · work session started {when(runEntry.run.created_at ?? runEntry.run.updated_at)}</p>
    <h1>Decision {turn.turn}</h1>
    <p class="unverified">The agent's reading of the project at this decision. It is not a verified result.</p>
    <section><h2>{turn.record.outcome === "revise" ? "Proposal sent back" : "Proposal"}</h2><p class="serif">{turn.record.proposal.summary}</p>
      {#if turn.record.proposal.actions.length}<ol class="actions">{#each turn.record.proposal.actions as action, i (i)}<li><strong>{action.title}</strong>{#if action.rationale}<span class="soft"> {action.rationale}</span>{/if}</li>{/each}</ol>{/if}</section>
    <section><h2>Methods check</h2><p><strong>{methodsCheckWords(turn.record.method_review.decision)}</strong>{#if turn.record.method_review.reason}: {turn.record.method_review.reason}{/if}</p>
      {#if turn.record.method_review.findings?.length}<ul class="plain">{#each turn.record.method_review.findings as finding, i (i)}<li>{reviewFindingLine(finding, turn.record.proposal.actions)}</li>{/each}</ul>{/if}</section>
    <section><h2>What it started</h2>
      {#if turn.record.submitted_tasks.length}
        <ul class="plain">{#each turn.record.submitted_tasks as s (s.task_id)}<li><button class="link" onclick={() => open(ids.analysis(model.project.project_id, s.task_id))}>{title(s.task_id)}</button></li>{/each}</ul>
      {:else}<p class="soft">No analysis was started at this decision.</p>{/if}</section>
    <p class="machine">Outcome {turn.record.outcome} · scientific validation {turn.record.scientific_validation} · plan version {turn.checkpoint_revision} · {when(turn.created_at)}</p>

  {:else if node.kind === "run" && runEntry}
    {@const sentence = runSentence(runEntry.run, false, canAct)}
    {@const rejected = runEntry.events.filter((e) => isModelRetry(e.code))}
    <h1>Work session started {when(runEntry.run.created_at ?? runEntry.run.updated_at)}</h1>
    <p class="lede">{sentence.text}</p>
    <p class="soft small">{runEntry.turns.length} of {runEntry.run.max_turns} decision turns used · up to {runEntry.run.max_parallel} analysis at a time · updated {when(runEntry.run.updated_at)}</p>
    <section><h2>Decisions</h2>
      <ol class="plain">{#each runEntry.turns as t (t.turn)}<li><button class="link" onclick={() => open(ids.turn(model.project.project_id, runEntry.run.run_id, t.turn))}>Decision {t.turn}</button> <span class="soft">{t.record.proposal.summary}</span></li>{:else}<li class="soft">No decisions recorded yet.</li>{/each}</ol></section>
    <section><h2>Waits and retries</h2>
      {#if rejected.length}<p>{rejected.length} repl{rejected.length === 1 ? "y" : "ies"} from the planning model {rejected.length === 1 ? "was" : "were"} rejected by validation. No analysis ran because of {rejected.length === 1 ? "it" : "them"}.</p>{/if}
      <ol class="events">{#each runEntry.events as e (e.sequence)}<li><time datetime={e.occurred_at}>{when(e.occurred_at)}</time><span>{deferralLabel(e.code, e.reason)}</span></li>{:else}<li class="soft">No waits recorded.</li>{/each}</ol></section>

  {:else if node.kind === "question"}
    <p class="running">{groupName} · {model.project.visibility === "lab" ? "Shared with the lab" : "Private"}{model.project.checkpoint ? ` · Saved version ${model.project.checkpoint.revision}` : ""}</p>
    <h1>{model.project.title}</h1>
    {#if research?.goal}<p class="serif lede question">{research.goal}</p>{:else}<p class="soft">No question is saved yet.</p>{/if}
    <p class="standing"><StatusMark tone={standingTone[standing]} label={standingNames[standing]} /><span>{standingWords}</span></p>
    <section class="readout" aria-labelledby="ov-found">
      <h2 id="ov-found">{answer ? "What it found" : "What it found so far"}</h2>
      {#if answer}
        <p class="reading">{accepted ? "The agent's reading. A person accepted the analyses it rests on." : "The agent's reading, not checked by a person."}</p>
        <p class="serif answer">{answerWords[answer.answer]}</p>
        {#if answer.findings.length}
          <ol class="finds" aria-label="The agent's findings">
            {#each answer.findings as finding, i (i)}
              <li><span class="n" aria-hidden="true">{i + 1}</span><p class="serif">{#each glued(parts(finding, digests)) as g, j (j)}{#if "text" in g}{g.text}{:else}{@const part = g.value}<span class="nw">{g.before}{#if part.value !== null}<button class="val" title={`${sourceWords(part.binding)}: ${part.value}. Opens the table.`} onclick={() => source(part.binding)}><data class="bound" value={part.value}>{display(part.value)}</data></button>{:else}<span class="missing" title={`${sourceWords(part.binding)}: not found in the stored digest`}>value not found</span>{/if}{g.after}</span>{/if}{/each}</p></li>
            {/each}
          </ol>
        {/if}
        {#each answer.tables as table, i (i)}<AnswerTableView {table} {digests} source={tableSource(table)} flag={tableFlag(table)} />{/each}
        {#if answer.parts.length}
          <dl class="parts">{#each answer.parts as part, i (i)}<div><dt>{partStateWords[part.state]}</dt><dd>{part.part}{#if part.reason}<span class="soft small">{part.reason}</span>{/if}</dd></div>{/each}</dl>
        {/if}
      {:else}<p class="soft">No answer yet. The agent states one when it stops for review.</p>{/if}
    </section>
    {#if research}
      {#if research.constraints.length}<section><h2>Limits you set</h2><ul class="plain">{#each research.constraints as c, i (i)}<li>{c}</li>{/each}</ul></section>{/if}
      {#if research.next_action}<section><h2>What the agent plans next</h2><p class="serif">{research.next_action}</p><p class="soft small">The agent's reading, not a result.</p></section>{/if}
      <section aria-labelledby="ov-steps"><h2 id="ov-steps">Steps</h2>
        <!-- A short plan shows each step's reason; a long one keeps to titles and statuses, and each step's view holds its reason. -->
        <ol class="plan-list" aria-label="The agent's steps">
          {#each research.plan as step, i (step.action_key)}
            {@const st = stepStatus(step.status)}
            <li><span class="sn" aria-hidden="true">{i + 1}</span>
              <span class="plan-body"><button class="link" aria-label={`Step ${i + 1}: ${step.title}`}
                onclick={() => open(step.task_id ? ids.analysis(model.project.project_id, step.task_id) : ids.planned(model.project.project_id, step.action_key))}>{step.title}</button>
                {#if research.plan.length <= 8 && step.rationale}<span class="soft small">{step.rationale}</span>{/if}</span>
              <StatusMark tone={st.tone} label={st.label} /></li>
          {:else}<li class="soft">No steps planned yet.</li>{/each}
        </ol></section>
      {@const notes = agentNotes(model.checkpoints)}
      {#if notes.length}<section><h2>Notes the agent saved</h2><ul class="notes">{#each notes as note, i (i)}<li class:withdrawn={!note.current}><span class="kind">{lessonLabel(note.kind)}{note.current ? "" : " · withdrawn"}</span>{note.statement}</li>{/each}</ul></section>{/if}
    {/if}
    <p class="links"><button class="link" onclick={() => openClassic(`#/projects/${model.project.project_id}`)}>Open the project page</button></p>

  {:else if node.kind === "planned"}
    {@const step = research?.plan.find((s) => ids.planned(model.project.project_id, s.action_key) === node.id)}
    <p class="running">{model.project.title} · Step {node.step}</p>
    <h1>{step?.title ?? node.label}</h1>
    {#if step}
      {@const st = stepStatus(step.status)}
      <p class="standing"><StatusMark tone={st.tone} label={st.label} /><span>{step.status === "pending" ? "No analysis has started for this step yet." : "No analysis is recorded for this step."}</span></p>
      {#if step.rationale}<section><h2>The agent's reason</h2><p class="serif">{step.rationale}</p><p class="soft small">The agent's words; the plan can change.</p></section>{/if}
    {:else}<p class="soft">This step is no longer in the plan.</p>{/if}

  {:else if node.kind === "data"}
    <p class="running">{model.project.title}</p>
    <h1>Data used</h1>
    {#if research?.workspace.input_refs.length}
      <section><h2>Data sources</h2><ul class="plain">{#each research.workspace.input_refs as ref (ref)}{@const w = inputWords(ref)}<li><strong>{w.name}</strong> <span class="soft">{w.kind}</span></li>{/each}</ul>
        <p class="soft small">Each analysis works on a sealed copy of its inputs. The copies below are the exact files each step read, with their checksums.</p></section>
    {/if}
    <section><h2>Copies each step read</h2>
      {#if dataUsed.length}
        <ul class="inputs">
          {#each dataUsed as item (`${item.task.task_id}:${item.path}`)}
            <li><button class="link" onclick={() => preview(ids.file(model.project.project_id, item.task.task_id, item.pkg.artifact.id, item.path))}>{item.path}</button>
              <span class="soft small">{title(item.task.task_id)}{#if item.member} · {bytes(item.member.size_bytes)} · SHA-256 {item.member.sha256.slice(0, 16)}… · {item.member.manifest === "matched" ? "matches the package checksum list" : item.member.manifest === "mismatched" ? "does not match the package checksum list" : "checked when read"}{/if}</span></li>
          {/each}
        </ul>
      {:else}<p class="soft">No step has retained a copy of its inputs yet.</p>{/if}
    </section>

  {:else if node.kind === "files"}
    <p class="running">{model.project.title}</p>
    <h1>All files</h1>
    <WbFiles groups={fileGroups} open={(row, keep) => (keep ? open(row.id) : preview(row.id))} />

  {:else if node.kind === "people"}
    <h1>People and access</h1>
    <PeoplePanel projectId={model.project.project_id} manage={model.project.capabilities.includes("manage")} members={model.members}
      audit={model.audit} visibility={model.project.visibility} {groupName} onChanged={reload} />

  {:else if node.kind === "package"}
    <h1>Evidence package</h1>
    <p class="lede">The whole project as one file: the question and every saved version of its plan, the work sessions and their decisions, the access record, and every retained analysis file with its checksum. A reviewer can rerun the analyses from it.</p>
    <p><button class="button" onclick={() => (packageOpen = true)}>Prepare the evidence package</button></p>
    <EvidencePackage projectId={model.project.project_id} bind:open={packageOpen} />

  {:else}
    {@const sentence = runSentence(latestRun ? { ...latestRun } : undefined, anyActive, canAct)}
    <p class="running">{groupName} · {model.project.visibility === "lab" ? "Shared with the lab" : "Private"}</p>
    <h1>{model.project.title}</h1>
    {#if research?.goal}<p class="serif lede">{research.goal}</p>{/if}
    <p>{sentence.text}</p>
    <p class="soft small">{model.project.tasks.length} {model.project.tasks.length === 1 ? "analysis" : "analyses"} · {model.runs.length} work session{model.runs.length === 1 ? "" : "s"} · saved version {model.project.revision} · updated {when(model.project.updated_at)}</p>
    <p class="links"><button class="link" onclick={() => openClassic(`#/projects/${model.project.project_id}`)}>Open the project page</button></p>
  {/if}
</div>

<style>
  .view { max-width: 1040px; padding: 22px 28px 40px; display: grid; gap: 14px; align-content: start; }
  h1 { font-size: calc(24px * var(--type-scale)); line-height: 1.2; letter-spacing: -0.01em; }
  h2 { font-size: var(--font-small); font-weight: 700; color: var(--heading); margin-bottom: 6px; }
  section { display: grid; gap: 4px; padding-top: 10px; border-top: 1px solid var(--line); }
  .running { font-size: var(--font-xs); color: var(--text-3); }
  .lede { font-size: calc(15px * var(--type-scale)); line-height: 1.55; max-width: 72ch; }
  .serif { font-family: var(--serif); line-height: 1.55; max-width: 70ch; }
  .unverified { position: relative; padding-left: 12px; font-size: var(--font-small); color: var(--text-2); }
  .unverified::before { content: ""; position: absolute; left: 0; top: 2px; bottom: 2px; width: 3px; background: var(--mark); }
  .now { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; }
  .machine { font: var(--font-xs) / 1.5 var(--mono); color: var(--text-3); overflow-wrap: anywhere; }
  .links { display: flex; flex-wrap: wrap; gap: 8px 18px; }
  .plain { list-style: none; margin: 0; padding: 0; display: grid; gap: 4px; }
  .actions { margin: 6px 0 0; padding-left: 20px; display: grid; gap: 6px; }
  .events { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); }
  .events li { display: grid; grid-template-columns: 150px minmax(0, 1fr); gap: 12px; padding: 6px 0; border-bottom: 1px solid var(--line); font-size: var(--font-small); }
  .events time { color: var(--text-3); font-variant-numeric: tabular-nums; }
  .notes { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
  .notes li { display: grid; gap: 2px; padding-left: 12px; border-left: 2px solid var(--line-2); }
  .notes .kind { font-size: var(--font-xs); color: var(--text-3); }
  .notes .withdrawn { color: var(--text-3); text-decoration: line-through; text-decoration-color: var(--line-2); }
  /* All files fills the editor so its list scrolls on its own, a window at a time. */
  .view[data-kind="files"] { height: 100%; max-width: none; grid-template-rows: auto auto minmax(0, 1fr); padding-bottom: 16px; }
  .question { font-size: calc(18px * var(--type-scale)); max-width: 62ch; }
  .standing { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 12px; font-size: calc(15px * var(--type-scale)); }
  .standing :global(.status) { font-size: calc(15px * var(--type-scale)); font-weight: 700; color: var(--heading); }
  .readout { gap: 10px; }
  .reading { font-size: var(--font-xs); font-style: italic; color: var(--text-2); }
  .answer { font-size: calc(18px * var(--type-scale)); }
  .finds { list-style: none; margin: 0; padding: 0; display: grid; gap: 10px; max-width: 70ch; }
  .finds li { display: grid; grid-template-columns: 22px minmax(0, 1fr); gap: 8px; padding-top: 10px; border-top: 1px solid var(--line); }
  .finds .n { font-size: var(--font-xs); font-weight: 700; color: var(--text-3); padding-top: 4px; }
  .finds p { font-size: calc(16.5px * var(--type-scale)); line-height: 1.55; }
  .val { display: inline; padding: 0 1px; border-radius: 3px; font: inherit; color: var(--heading); font-weight: 650;
    text-decoration: underline; text-decoration-color: var(--mark); text-decoration-thickness: 2px; text-underline-offset: 4px; }
  .val:hover { background: var(--row-hover); }
  .missing { font-size: var(--font-small); font-style: italic; color: var(--failed); }
  .parts { display: grid; gap: 8px; margin: 0; max-width: 70ch; }
  .parts > div { display: grid; grid-template-columns: 160px minmax(0, 1fr); gap: 12px; padding-left: 12px; border-left: 2px solid var(--line-2); }
  .parts dt { font-size: var(--font-xs); font-weight: 650; color: var(--text-2); padding-top: 2px; }
  .parts dd { margin: 0; display: grid; gap: 2px; font-family: var(--serif); }
  .sn { display: inline-block; min-width: 18px; font-size: var(--font-xs); font-weight: 700; color: var(--text-3); font-variant-numeric: tabular-nums; }
  .plan-list { list-style: none; margin: 0; padding: 0; max-width: 78ch; }
  .plan-list li { display: grid; grid-template-columns: 28px minmax(0, 1fr) auto; column-gap: 10px; align-items: baseline; padding: 7px 0; border-top: 1px solid var(--line); }
  .plan-list li:last-child { border-bottom: 1px solid var(--line); }
  .plan-list .sn { padding-top: 1px; }
  .plan-body { display: grid; gap: 2px; justify-items: start; min-width: 0; }
  .plan-body .link { text-align: start; overflow-wrap: anywhere; }
  .plan-list :global(.status) { font-size: var(--font-xs); }
  .inputs { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
  .inputs li { display: grid; gap: 1px; justify-items: start; }
  .inputs .link { font-family: var(--mono); font-size: var(--font-small); overflow-wrap: anywhere; text-align: start; }
  @media (max-width: 700px) { .view { padding: 16px 16px 32px; } .events li { grid-template-columns: 1fr; gap: 2px; } .parts > div { grid-template-columns: 1fr; gap: 2px; } }
</style>
