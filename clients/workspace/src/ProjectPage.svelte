<script lang="ts">
  import LabDataSection from "./LabDataSection.svelte";
  import { onDestroy, tick, untrack } from "svelte";
  import AcceptSheet, { type Acceptable } from "./AcceptSheet.svelte";
  import AnswerExport from "./AnswerExport.svelte";
  import AnswerTableView from "./AnswerTableView.svelte";
  import { broker, BrokerError, verifiedMember, watchProject } from "./api.ts";
  import { sharing } from "./context.ts";
  import type { Identity, LabNote, LabNotes, Library, TaskEvent, TaskReview } from "./contracts.ts";
  import Dialog from "./Dialog.svelte";
  import EvidencePackage from "./EvidencePackage.svelte";
  import { canWriteDown } from "./documents.ts";
  import { loadMany, loadTaskEvidence, type TaskEvidence } from "./evidence.ts";
  import FigurePlate from "./FigurePlate.svelte";
  import { followProject, viaChanges, type Follower, type Link } from "./follow.ts";
  import { live } from "./live.ts";
  import { withFiguresOnce } from "./figure.ts";
  import { answerHistory, answerWords, display, glued, partStateWords, parts, referencedArtifacts, shownFindings, shownTables, sourceWords, type AnswerTable, type ArtifactDigest, type Binding, type Finding } from "./findings.ts";
  import { agentNotes, buildHistory, type AuditRecord } from "./history.ts";
  import HistoryTimeline from "./HistoryTimeline.svelte";
  import { inbox, type Seen } from "./inbox.ts";
  import { lab } from "./lab.svelte.ts";
  import { labNotes, noteStatus, notesRead, sharedAs, shareBlocker } from "./lab_notes.ts";
  import { library, paperReadWords, papersRead } from "./library.ts";
  import { activeTask, fileTitle, lessonLabel, runSentence, stepStatus, taskStatus, when, type Tone } from "./language.ts";
  import { nowLine, paceWords, tallyWords, type StepTimes } from "./live_words.ts";
  import PeoplePanel from "./PeoplePanel.svelte";
  import { archivedStepCount, earlierReasons, removedSteps, stepMoments, type PlanLedger } from "./plan_ledger.ts";
  import { byPerson, findingFlag, reviewMark } from "./person_review.ts";
  import { chosenFigure, openWork, pageTitle, projectIndex } from "./project_index.svelte.ts";
  import { projects, taskTitle, type Checkpoint, type Project, type ProjectMembers, type ResearchRun, type ResearchState, type ResearchTurn, type RunEvent, type RunOrientation } from "./projects.ts";
  import { figureSource, valueSource, type Made, type Source } from "./provenance.ts";
  import ResearchControls from "./ResearchControls.svelte";
  import ShareNote from "./ShareNote.svelte";
  import { href } from "./route.ts";
  import { sinceLooked, type SinceKey } from "./since.ts";
  import SinceList from "./SinceList.svelte";
  import { inputWords, instructionHistory, isInstruction, MAX_INSTRUCTION, standingOf, stepLine, withInstruction, withoutInstruction, type InstructionHistory } from "./standing.ts";
  import StatusBlock from "./StatusBlock.svelte";
  import StatusMark from "./StatusMark.svelte";
  import { fromMember } from "./viewer.ts";
  import WatchFeed from "./WatchFeed.svelte";
  import { watchLines } from "./watch.ts";
  import WorkspaceSetup from "./WorkspaceSetup.svelte";

  let { projectId, identity, section = "", inspection: known = null, openTask, openDataset, onEndInspection, onInspection, onRunIndividual, notify }: {
    projectId: string; identity: Identity; section?: string; inspection?: string | null;
    openTask: (task: string) => void; openDataset: (dataset: string) => void; onEndInspection: () => void; onInspection?: (expires: string | null) => void;
    onRunIndividual: (project: Project) => void; notify: (message: string, error?: boolean) => void;
  } = $props();

  let project = $state<Project | null>(null);
  let runs = $state<{ run: ResearchRun; turns: ResearchTurn[]; events: RunEvent[] }[]>([]);
  let checkpoints = $state<Checkpoint[]>([]);
  let audit = $state<AuditRecord[]>([]);
  let members = $state<ProjectMembers | null>(null);
  let evidence = $state<Record<string, TaskEvidence>>({});
  // Stored digests of the result packages the agent's findings read, by artifact reference.
  let digests = $state<Record<string, ArtifactDigest>>({});
  // Analyses whose retained files could not be read on the last attempt. The analysis
  // itself is unchanged; its entry says so and offers another try.
  let evidenceFailed = $state<Record<string, boolean>>({});
  // The newest person's review of each analysis; `reviewsFailed` keeps a failed read from reading as "none".
  let reviews = $state<Record<string, TaskReview>>({});
  let reviewsFailed = $state(false);
  // The lab's shared notes, or null when they could not be read or the reader is not in the lab.
  let labMemory = $state<LabNotes | null>(null);
  // The lab's library list names the papers the agent read. It is read again only when the papers
  // read at the latest decision change.
  let labLibrary = $state<Library | null>(null);
  // The plan ledger, or null when it could not be read; the plan itself still shows.
  let ledger = $state<PlanLedger | null>(null);
  // Who saw each review request of this project.
  let seenBy = $state<Seen[]>([]);
  let libraryKey = "";
  // The newest run's orientation, read once per run.
  let orientation = $state<RunOrientation | null>(null);
  let orientationKey = "";
  let loading = $state(true);
  let failure = $state("");
  let startOpen = $state(false);
  let editOpen = $state(false);
  let packageOpen = $state(false);
  let pausing = $state(false);
  let controller: AbortController | null = null;
  // Once the project has been read, the project stream says when to read it again (follow.ts); the note under the status
  // says when the stream is not connected. While the page is hidden a change waits, and the tab title still learns
  // whether work runs.
  let follower: Follower | null = null;
  let link = $state<Link>("connecting");
  let lastRead = $state("");
  // Every change to this project reads it again (live.ts): the reader's own, their other tabs', and the agent's and
  // colleagues' through the change stream, at most every two seconds while work runs.
  let unwatch: (() => void) | null = null;
  function unfollow() { follower?.stop(); follower = null; unwatch?.(); unwatch = null; link = "connecting"; openWork.busy = false; }

  // The server's project view is the authority; the shell's value covers the first load.
  const inspection = $derived(project ? project.inspection_expires_at ?? null : known);
  $effect(() => { const value = project ? project.inspection_expires_at ?? null : undefined; if (value !== undefined) untrack(() => onInspection?.(value)); });
  const can = (capability: string) => project?.capabilities.includes(capability) ?? false;
  const research = $derived(project?.checkpoint?.state ?? null);
  const latestRun = $derived(runs[0]?.run);
  // The people who saw the agent's current request for review, in the order they saw it.
  const seenNow = $derived(latestRun?.status === "review_required" ? seenBy.filter((s) => s.run_id === latestRun!.run_id) : []);
  const seenWords = $derived.by(() => {
    const names = seenNow.map((s) => (s.principal_id === identity.principal.id ? "you" : s.display_name?.trim() || "a colleague"));
    return names.length <= 1 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
  });
  const activeRun = $derived(runs.find(({ run }) => ["queued", "running", "waiting"].includes(run.status))?.run);
  const tasksActive = $derived(project?.tasks.some((t) => activeTask(t.status)) ?? false);
  const sentence = $derived.by(() => standing === "finished" && latestRun?.status === "review_required"
    ? { text: "A person accepted its results. The agent does nothing more until someone tells it to continue.", tone: "done" as const }
    : runSentence(latestRun ? { ...latestRun, failures: latestRun.failures } : undefined, tasksActive, can("steer") || can("review")));
  const stepFor = (taskId: string) => research?.plan.find((s) => s.task_id === taskId);
  const taskFor = (taskId: string) => project?.tasks.find((t) => t.task_id === taskId);
  const titleFor = (taskId: string) => stepFor(taskId)?.title ?? taskTitle(project, taskId) ?? identity.profiles.find((p) => p.key === taskFor(taskId)?.profile)?.name ?? "Analysis";
  const profileName = (key: string) => identity.profiles.find((p) => p.key === key)?.name ?? key;
  // The lab takes lab data only when the reader's identity says it holds the dataset method; a missing signal reads false.
  const takesLabData = $derived(!!project && identity.groups.find((g) => g.key === project!.group_key)?.lab_data === true);
  // The analyses, oldest first, the way a paper's results run. A figure drawn again is listed once, by its newest
  // drawn version; its page lists the others.
  const ordered = $derived(withFiguresOnce([...(project?.tasks ?? [])]).sort((a, b) => a.submitted_at.localeCompare(b.submitted_at)));
  const personName = (id: string | undefined) => {
    if (!id) return "A person";
    if (id === identity.principal.id) return "you";
    const found = members ? [members.owner, ...members.grants, ...members.eligible].find((m) => m.principal_id === id) : undefined;
    return found ? (found.display_name?.trim() || found.email) : "A colleague";
  };
  const history = $derived(project ? buildHistory({
    me: identity.principal.id, checkpoints, runs, audit,
    tasks: project.tasks.map((t) => ({ task_id: t.task_id, action_key: t.action_key, status: t.status, submitted_at: t.submitted_at, updated_at: t.updated_at, profile: t.profile })),
    stepTitle: (id) => titleFor(id), personName,
  }) : []);
  const notes = $derived(agentNotes(checkpoints));
  // Analyses that finished with results and have no recorded review by a person.
  const unreviewed = $derived(ordered.filter((t) => (t.status === "completed" || t.status === "review_required")
    && !(reviews[t.task_id] && byPerson(reviews[t.task_id]))));

  // The finished analyses, without figure versions (drawings, not analyses; migration 040), and people's current
  // reviews of them counted the way the broker counts them for the index. A stop for review whose
  // analyses a person accepted reads as finished; an unread review list counts nothing.
  const analysesDone = $derived(project?.tasks.filter((t) => !t.figure && (t.status === "completed" || t.status === "review_required")) ?? []);
  const currentReview = (taskId: string) => { const r = reviews[taskId]; return r && byPerson(r) && r.covers_current_outputs ? r : null; };
  const reviewCounts = $derived.by(() => {
    if (reviewsFailed) return {};
    const current = analysesDone.map((t) => currentReview(t.task_id)).filter((r): r is TaskReview => !!r);
    return { reviewed: current.length, rejected: current.filter((r) => r.decision === "rejected").length, changes: current.filter((r) => r.decision === "needs_changes").length };
  });
  // Where it stands, from the same records the lab index reads.
  const standing = $derived(project ? standingOf({
    question: research?.goal ?? null, capabilities: project.capabilities,
    latest_run: latestRun ? { run_id: latestRun.run_id, status: latestRun.status, code: latestRun.code, turn: latestRun.turn, max_turns: latestRun.max_turns, updated_at: latestRun.updated_at } : null,
    task_counts: { total: project.tasks.length, active: project.tasks.filter((t) => activeTask(t.status)).length,
      finished: analysesDone.length, failed: project.tasks.filter((t) => t.status === "failed").length, ...reviewCounts },
  }) : "not_started");
  const steps = $derived(stepLine(research?.plan ?? []));
  const current = $derived(steps.findIndex((s) => s.mark === "current"));
  const stepWords = $derived(!steps.length ? ""
    : current >= 0 ? `Step ${current + 1} of ${steps.length}: ${steps[current].title}`
    : steps.every((s) => s.mark === "done") ? (steps.length === 1 ? "Its one step finished" : steps.length === 2 ? "Both steps finished" : `All ${steps.length} steps finished`)
    : `${steps.filter((s) => s.mark === "done").length} of ${steps.length} steps finished`);
  // The agent's own reading at its latest decision: its summary for the scientist and the next
  // step it saved. Neither is verified, and neither says the question was answered.
  // The newest session with a recorded decision, and its last decision.
  // A proposal the methods check sent back is not the agent's reading.
  const decided = (turns: ResearchTurn[]) => turns.filter((t) => t.record.outcome !== "revise");
  const lastTurn = $derived(decided(runs.find(({ turns }) => decided(turns).length)?.turns ?? []).at(-1));
  const reading = $derived(standing === "working" ? "" : lastTurn?.record.proposal.summary ?? "");
  // The agent's newest answer, even one that states no findings, and the findings it stated before it,
  // kept as dated history. Each value is filled from the stored digest.
  const answers = $derived(answerHistory(runs));
  const answer = $derived(answers.current);
  // Its summary is called a summary only beside an answer. Without one it is often the plan for the next step, so it is
  // shown as the agent's note and never in the place of an answer.
  const answered = $derived(!!answer && answer.answer !== "not_yet");
  // The next step it saved, unless it repeats the note above word for word.
  const plain = (text: string) => text.replace(/\s+/g, " ").trim().replace(/\.$/, "").toLowerCase();
  const suggestion = $derived(research?.next_action && standing === "waiting"
    && !(reading && (plain(reading).includes(plain(research.next_action)) || plain(research.next_action).includes(plain(reading))))
    ? research.next_action : "");
  // Each instruction with the decision that read it and the version that withdrew it.
  const instructions = $derived(instructionHistory(checkpoints, runs.flatMap(({ run, turns }) =>
    turns.map((t) => ({ run: run.run_id, turn: t.turn, context_revision: t.context_revision, created_at: t.created_at })))));
  const studyConstraints = $derived(research?.constraints.filter((c) => !isInstruction(c)) ?? []);
  const findings = $derived(research?.lessons ?? []);
  // A lab lead or reviewer may share an observation once a person accepted the analyses it rests on.
  const curate = $derived(!inspection && !!labMemory?.can_curate && project?.visibility === "lab");
  const readNotes = $derived(notesRead(lastTurn?.record, labMemory?.notes ?? []));
  const readPapers = $derived(papersRead(lastTurn?.record, labLibrary?.papers ?? null));
  function sharedNow(note: LabNote) {
    if (labMemory) labMemory = { ...labMemory, notes: [note, ...labMemory.notes] };
    notify("The note is shared with the lab. The agent reads it in matching projects.");
  }
  const taskFromRef = (ref: string) => /^artifact:\/\/tasks\/([0-9a-f-]{36})\//i.exec(ref)?.[1] ?? null;
  const defaults = $derived(latestRun?.allowed_profiles ?? []);
  const checks = $derived(runs.flatMap(({ turns }) => turns)
    .sort((a, b) => a.created_at.localeCompare(b.created_at))
    .filter((t) => t.record.submitted_tasks.length)
    .map((t) => ({ key: `${t.created_at}-${t.turn}`, agreed: t.record.method_review.decision === "proceed", reason: t.record.method_review.reason,
      steps: t.record.submitted_tasks.map((s) => titleFor(s.task_id)), at: t.created_at })));

  // The reader's last look: each newer revision the reader has on screen is recorded, so
  // the landing rule and "since you last looked" know what they saw. Only a broker that reports a last look is told,
  // never during an administrator's inspection, and a failed record is left for the next read.
  let looked = -1;
  function look(p: Project) {
    if (!p.last_look || inspection || document.hidden || p.revision <= looked || (p.last_look.revision ?? -1) >= p.revision) return;
    looked = p.revision;
    projects.look(p.project_id, p.revision).catch(() => { looked = -1; });
  }
  async function load(first = false) {
    controller?.abort();
    const c = new AbortController();
    controller = c;
    if (first) { loading = true; failure = ""; }
    try {
      const p = await projects.get(projectId, c.signal);
      const [runList, saved, auditPage, reviewList, notesList, planLedger, seenList] = await Promise.all([
        projects.runs(projectId, c.signal), projects.allHistory(projectId, c.signal), projects.audit(projectId, c.signal),
        projects.reviews(projectId, c.signal).catch(() => null),
        labNotes.list(p.group_key, c.signal).catch(() => null),
        projects.planLedger(projectId, c.signal).catch(() => null),
        inbox.seen(projectId, c.signal).catch(() => [] as Seen[]),
      ]);
      const runDetail = await loadMany(runList.runs, 3, async (run) => ({
        run, turns: (await projects.turns(run.run_id, c.signal)).turns, events: await projects.runEvents(run.run_id, c.signal),
      }));
      const lastRecord = runDetail.find(({ turns }) => turns.length)?.turns.at(-1)?.record;
      const readIds = [...new Set((lastRecord?.context_inputs?.literature?.passages ?? []).map((p) => p.paper_id))].sort().join(",");
      const libraryList = readIds && readIds !== libraryKey ? await library.list(p.group_key, c.signal).catch(() => null) : null;
      // Only the newest run's orientation: an older run's would read as this work's.
      const newest = runDetail[0];
      const oriented = newest && newest.turns.some((t) => t.record.orientation?.status === "read") ? newest.run.run_id : "";
      const orientationRead = oriented && oriented !== orientationKey ? await projects.orientation(oriented, c.signal).catch(() => null) : null;
      const memberList = p.capabilities.includes("manage") ? await projects.members(projectId, c.signal) : null;
      const history = answerHistory(runDetail);
      const wanted = referencedArtifacts(shownFindings(history), shownTables(history)).filter((a) => !digests[a.ref]);
      const read = await loadMany(wanted, 3, (a) => projects.digest(a.task, a.artifact, c.signal).catch(() => null));
      const needs = withFiguresOnce(p.tasks).filter((t) => !activeTask(t.status) && evidence[t.task_id] === undefined);
      const loaded = await loadMany(needs, 3, (t) => loadTaskEvidence(t.task_id, c.signal).catch(() => null));
      if (c.signal.aborted) return;
      const failedNow = { ...evidenceFailed };
      loaded.forEach((item, i) => { if (item) delete failedNow[needs[i].task_id]; else failedNow[needs[i].task_id] = true; });
      evidenceFailed = failedNow;
      project = p; runs = runDetail; checkpoints = saved; audit = auditPage.events as AuditRecord[]; members = memberList;
      if (reviewList) reviews = Object.fromEntries(reviewList.map((r) => [r.task_id, r]));
      reviewsFailed = !reviewList;
      labMemory = notesList;
      ledger = planLedger;
      seenBy = seenList;
      if (libraryList) { labLibrary = libraryList; libraryKey = readIds; }
      if (orientationRead) { orientation = orientationRead; orientationKey = oriented; } else if (!oriented) { orientation = null; orientationKey = ""; }
      pageTitle.value = p.title;
      projectIndex.remember(p);
      // The letterhead follows the lab of the project being read.
      if (p.group_key !== lab.key && identity.groups.some((g) => g.key === p.group_key)) lab.set(p.group_key);
      const next = { ...evidence };
      loaded.forEach((item, i) => { if (item) next[needs[i].task_id] = item; });
      evidence = next;
      const nextDigests = { ...digests };
      read.forEach((item, i) => { if (item) nextDigests[wanted[i].ref] = item; });
      digests = nextDigests;
      failure = "";
      lastRead = new Date().toISOString();
      clock = Date.now();
      // The look the broker held before this visit, taken once per project, before this page records its own.
      if (sinceTaken !== p.project_id) { sinceTaken = p.project_id; since = p.last_look?.at ?? null; }
      look(p);
      const busy = p.tasks.some((t) => activeTask(t.status)) || runList.runs.some((r) => ["queued", "running", "waiting"].includes(r.status));
      unwatch ??= live().watch([`project:${p.project_id}`], () => load(), { gapMs: 2_000 });
      // Without the change stream the project's own stream follows it, and news of the agent's progress there reads the
      // waiting list and the projects again too, as the broker's stream would name them.
      follower ??= followProject({ open: (on) => (live().link === "live" ? viaChanges(live(), p.project_id, on) : watchProject(p.project_id, on)),
        read: () => load(), hidden: () => document.hidden,
        link: (next) => { link = next; }, busy: (next) => { openWork.busy = next; },
        changed: () => { if (live().link !== "live") live().changed(["inbox", "projects"], "stream"); } });
      follower.loaded(busy);
      if (busy) void readTimes(p, c.signal);
      if (watching) void readToolSteps(p, c.signal);
    } catch (cause) {
      if (c.signal.aborted) return;
      const closed = cause instanceof BrokerError && (cause.code === "record_unavailable" || cause.code === "access_required");
      // A project closed to this account is not followed any further; reading it again starts over.
      if (closed) unfollow();
      failure = closed
        ? inspection ? "The inspection has ended or expired, so this project is closed to your account." : "This project is not available to your account."
        : "The project could not be loaded. Your saved work is unchanged on the server.";
    } finally {
      if (!c.signal.aborted) loading = false;
    }
  }
  $effect(() => { void projectId; untrack(() => { unfollow(); project = null; evidence = {}; evidenceFailed = {}; digests = {}; reviews = {}; reviewsFailed = false; labMemory = null; labLibrary = null; libraryKey = ""; orientation = null; orientationKey = ""; runs = []; checkpoints = []; watching = false; toolSteps = {}; times = {}; void load(true); }); });
  $effect(() => {
    const shown = () => { if (!document.hidden) follower?.shown(); };
    document.addEventListener("visibilitychange", shown);
    return () => document.removeEventListener("visibilitychange", shown);
  });
  onDestroy(() => { controller?.abort(); unfollow(); });

  async function retryEvidence(taskId: string) {
    const c = controller ?? new AbortController();
    try {
      const loaded = await loadTaskEvidence(taskId, c.signal);
      if (c.signal.aborted) return;
      evidence = { ...evidence, [taskId]: loaded };
      const next = { ...evidenceFailed }; delete next[taskId]; evidenceFailed = next;
    } catch { if (!c.signal.aborted) notify("The files still could not be read. The analysis is unchanged; try again shortly.", true); }
  }

  // One page, no tabs (Glass Box, "A project"). A section address opens the part of the page that holds it; the names
  // the tabbed page used still work.
  type Part = "top" | "since" | "watch" | "found" | "next" | "steps" | "record" | "people";
  const PARTS: Record<string, Part> = { since: "since", watch: "watch", found: "found", overview: "found", summary: "found", findings: "found", next: "next",
    data: "next", downloads: "next", steps: "steps", results: "steps", analyses: "steps", plan: "steps", notebook: "steps", checks: "steps",
    record: "record", history: "record", people: "people" };
  const partOf = (id: string): Part => PARTS[id] ?? "top";
  let recordOpen = $state(false), manageOpen = $state(false);
  const reduced = () => typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  // Brings a part of the page into view below the bar and moves the keyboard to its heading.
  async function reveal(part: Part) {
    if (part === "people") { manageOpen = true; return; }
    if (part === "record") recordOpen = true;
    if (part === "watch" && !watching) { watching = true; if (project) void readToolSteps(project, controller?.signal); }
    if (part === "top") return;
    await tick();
    const target = document.getElementById(`section-${part}`);
    if (!target) return;
    target.scrollIntoView({ behavior: reduced() ? "auto" : "smooth", block: "start" });
    target.querySelector<HTMLElement>("h2, summary")?.focus({ preventScroll: true });
  }
  // A link to a section opens it once the project has loaded, and again when the address names another one.
  let revealed = "";
  $effect(() => {
    const part = partOf(section), ready = !!project;
    untrack(() => { if (ready && revealed !== `${projectId}:${section}`) { revealed = `${projectId}:${section}`; void reveal(part); } });
  });

  // Since you last looked: the broker's last look as it stood when this page opened. The page records its own look
  // after reading, so the value is kept from the first read of each project.
  let since = $state<string | null>(null);
  let sinceTaken = "";
  const sinceRows = $derived(since && project && research ? sinceLooked({
    at: since,
    plan: research.plan.map((s) => ({ title: s.title, task_id: s.task_id })),
    tasks: project.tasks.filter((t) => !t.figure).map((t) => ({ task_id: t.task_id, status: t.status, updated_at: t.updated_at })),
    files: Object.values(evidence).flatMap((record) => record.packages.map((pkg) => ({
      task_id: record.taskId, created_at: pkg.artifact.created_at, figures: pkg.view.figures.length, tables: pkg.view.tables.length,
      code: pkg.view.code.length, logs: pkg.view.logs.length,
      other: pkg.view.inputs.length + pkg.view.notes.length + pkg.view.parameters.length + pkg.view.environment.length + pkg.view.other.length,
      intact: pkg.view.integrity.hasList && !pkg.view.integrity.mismatched.length && !pkg.view.integrity.missing.length }))),
    turns: runs.flatMap(({ turns }) => turns).map((t) => ({ turn: t.turn, created_at: t.created_at, outcome: t.record.outcome,
      review: t.record.method_review.decision, proposed: t.record.submitted_tasks.length })),
    instructions: instructions.filter((i) => !i.withdrawn).map((i) => ({ text: i.text, read: i.read })),
    answer: answer ? { answer: answer.answer, at: answer.at, findings: answer.findings.length, parts: answer.parts } : null,
  }) : []);
  const isNew = (taskId: string) => {
    const t = taskFor(taskId);
    return !!since && !!t && Date.parse(t.updated_at) > Date.parse(since) && (t.status === "completed" || t.status === "review_required");
  };
  function goSince(key: SinceKey) {
    void reveal(key === "answer" ? "found" : key === "instructions" ? "next" : "steps");
  }

  // What the system does now and for how long; the clock moves only while work runs and the page is in view.
  let clock = $state(Date.now());
  $effect(() => {
    if (standing !== "working") return;
    const id = setInterval(() => { if (!document.hidden) clock = Date.now(); }, 30_000);
    return () => clearInterval(id);
  });
  const doing = $derived(standing !== "working" ? null : nowLine(latestRun ? { status: latestRun.status, code: latestRun.code, updated_at: latestRun.updated_at } : undefined,
    project?.tasks.filter((t) => activeTask(t.status)).map((t) => ({ status: t.status, submitted_at: t.submitted_at, queue: t.queue })) ?? [], clock));
  const stepMarks = $derived(steps.map((s, i) => ({ ...s, label: stepStatus(research?.plan[i]?.status ?? "").label })));

  // The pace of recent analyses while work runs (Glass Box long runs rule 2): each ended analysis's recorded start and
  // end, read once, since an ended analysis does not change; only the newest ten are needed.
  let times = $state<Record<string, StepTimes>>({});
  const ended = (p: Project) => p.tasks.filter((t) => !t.figure && !activeTask(t.status)).sort((a, b) => b.updated_at.localeCompare(a.updated_at)).slice(0, 10);
  async function readTimes(p: Project, signal: AbortSignal) {
    const want = ended(p).filter((t) => !times[t.task_id]);
    if (!want.length) return;
    const read = await loadMany(want, 3, (t) => broker.task(t.task_id, signal).catch(() => null));
    if (signal.aborted) return;
    const next = { ...times };
    read.forEach((task, i) => { if (task) next[want[i].task_id] = { submitted_at: task.submitted_at, started_at: task.started_at, finished_at: task.finished_at }; });
    times = next;
  }
  const pace = $derived(standing === "working" && project ? paceWords(ended(project).map((t) => times[t.task_id]).filter((t): t is StepTimes => !!t)) : null);

  // Watch it work (Glass Box long runs rule 3): closed until the reader opens it, and it reads nothing while closed.
  // Open, it follows the recorded steps of the analyses that run and of the one that ended last, from where it left off.
  let watching = $state(false);
  let toolSteps = $state<Record<string, TaskEvent[]>>({});
  async function readToolSteps(p: Project, signal?: AbortSignal) {
    const last = p.tasks.filter((t) => !activeTask(t.status)).sort((a, b) => b.updated_at.localeCompare(a.updated_at)).slice(0, 1);
    const follow = [...p.tasks.filter((t) => activeTask(t.status)), ...last];
    const read = await loadMany(follow, 3, async (t) => {
      const have = toolSteps[t.task_id] ?? [], after = have.at(-1)?.sequence ?? 0;
      const page = await broker.events(t.task_id, after, signal).catch(() => null);
      return page ? [...have, ...page.events.filter((e) => e.sequence > after)] : null;
    });
    if (signal?.aborted || !watching) return;
    const next = { ...toolSteps };
    read.forEach((events, i) => { if (events) next[follow[i].task_id] = events; });
    toolSteps = next;
  }
  const watch = () => { if (watching) watching = false; else void reveal("watch"); };
  const stepOf = (taskId: string) => {
    const i = research?.plan.findIndex((s) => s.task_id === taskId) ?? -1;
    return i >= 0 && research ? { number: i + 1, title: research.plan[i].title } : null;
  };
  const filesOf = (taskId: string) => {
    const packages = evidence[taskId]?.packages ?? [];
    if (!packages.length) return null;
    const sum = (pick: (view: (typeof packages)[number]["view"]) => number) => packages.reduce((n, pkg) => n + pick(pkg.view), 0);
    return { figures: sum((v) => v.figures.length), tables: sum((v) => v.tables.length), code: sum((v) => v.code.length), logs: sum((v) => v.logs.length),
      other: sum((v) => v.inputs.length + v.notes.length + v.parameters.length + v.environment.length + v.other.length), intact: intactOf(taskId) === true };
  };
  const feed = $derived(watching && project ? watchLines({ history, tasks: project.tasks, step: stepOf, title: titleFor, files: filesOf, toolSteps,
    now: doing, at: new Date(clock).toISOString() }) : []);

  // Start again with the methods and limits of the last work session: Resume after a pause, Try again after a stop.
  let resuming = $state(false), resumeKey = "", resumeBody = "";
  async function resume() {
    if (!project || !latestRun || resuming) return;
    const body = { expected_revision: project.revision, allowed_profiles: [...latestRun.allowed_profiles].sort(), max_turns: latestRun.max_turns, max_parallel: latestRun.max_parallel };
    const serialized = JSON.stringify(body);
    if (serialized !== resumeBody) { resumeBody = serialized; resumeKey = crypto.randomUUID(); }
    resuming = true;
    try {
      await projects.start(project.project_id, { ...body, run_id: resumeKey });
      resumeBody = "";
      notify("The agent started again with the same methods and limits. You can leave this page.");
    } catch (cause) {
      notify(cause instanceof BrokerError && cause.code === "capacity_reached" ? "The shared analysis queue is full. Try again shortly."
        : cause instanceof BrokerError && (cause.code === "revision_conflict" || cause.code === "research_run_active") ? "The project changed a moment ago. It shows its newest state now."
        : "The agent did not start. Try again.", true);
    } finally { resuming = false; void load(); }
  }
  const canRun = $derived(!inspection && can("steer") && can("contribute"));

  // The figures through the results, numbered like a paper's; the lead figure is the first one from the analysis the
  // answer's first finding reads, else the newest.
  const figureList = $derived(ordered.flatMap((task) => (evidence[task.task_id]?.packages ?? []).flatMap((pkg) => pkg.view.figures
    .map((figure) => ({ task, pkg, figure }))).map((item, j) => ({ ...item, number: (numbering[task.task_id] ?? 1) + j }))));
  const leadTask = $derived(answer?.findings.length ? taskFromRef(Object.values(answer.findings[0].bindings)[0]?.ref ?? "")
    : answer?.tables.length ? taskFromRef(answer.tables[0].ref) : null);
  const lead = $derived(answered ? figureList.find((f) => f.task.task_id === leadTask) ?? figureList.at(-1) ?? null : null);
  const strip = $derived(figureList.filter((f) => f !== lead).reverse().slice(0, 3));
  const tableCount = $derived(Object.values(evidence).reduce((n, record) => n + record.packages.reduce((m, pkg) => m + pkg.view.tables.length, 0), 0));

  // Where a value or a figure came from (Glass Box rule 4). From 1,360 px the margin shows it while the pointer or
  // focus rests on it, and a click keeps it there; below that a click opens it as a sheet.
  let wide = $state(false);
  $effect(() => {
    const query = matchMedia("(min-width: 1360px)");
    const set = () => { wide = query.matches; };
    set();
    query.addEventListener("change", set);
    return () => query.removeEventListener("change", set);
  });
  let pointed = $state<Source | null>(null), kept = $state<{ key: string; source: Source } | null>(null), sourceOpen = $state(false);
  const shownSource = $derived(pointed ?? kept?.source ?? null);
  const stepIndex = (taskId: string) => { const i = research?.plan.findIndex((s) => s.task_id === taskId) ?? -1; return i >= 0 ? i + 1 : null; };
  const writtenByAgent = (taskId: string) => evidence[taskId]?.packages.some((pkg) => pkg.view.template) ?? false;
  const methodOf = (taskId: string) => {
    const task = taskFor(taskId);
    if (!task) return "";
    if (task.figure) return `Figure drawn by R from ${titleFor(task.figure.source_task)}`;
    if (task.template_analysis) return `A reviewed R analysis of ${titleFor(task.template_analysis.source_task)}`;
    return writtenByAgent(taskId) ? "R code the agent wrote, in a reviewed template" : profileName(task.profile);
  };
  const made = (taskId: string): Made => ({ task: taskId, step: stepIndex(taskId), title: titleFor(taskId), method: methodOf(taskId) });
  function point(source: Source | null) { if (wide) pointed = source; }
  function pick(key: string, source: Source | null) {
    if (!source) return;
    if (wide) { kept = kept?.key === key ? null : { key, source }; pointed = null; return; }
    kept = { key, source }; sourceOpen = true;
  }
  const valueOf = (binding: Binding, value: string | null) =>
    valueSource(binding, value, digests, evidence[artifactTask(binding.ref)]?.packages, made);
  const artifactTask = (ref: string) => taskFromRef(ref) ?? "";
  $effect(() => { void projectId; untrack(() => { pointed = null; kept = null; }); });

  // The analyses the answer's findings read, and whether a person's current review accepts each of them.
  const restsOn = $derived([...new Set([...(answer?.findings.flatMap((f) => Object.values(f.bindings).map((b) => artifactTask(b.ref))) ?? []),
    ...(answer?.tables.map((t) => artifactTask(t.ref)) ?? [])].filter(Boolean))]);
  // Where an answer table comes from, with a link to its analysis, which shows every row from the verified file.
  const tableAnalysis = (table: AnswerTable) => {
    const task = taskFromRef(table.ref);
    if (!task) return null;
    const step = stepIndex(task);
    return { href: href({ name: "analysis", project: projectId, task }), open: () => openTask(task), label: "Open the analysis",
      from: step ? `step ${step}, ${titleFor(task)}` : titleFor(task) };
  };
  const answerAccepted = $derived(restsOn.length > 0 && restsOn.every((t) => currentReview(t)?.decision === "accepted"));
  // Accept the result: every finished analysis no person's current review accepts, the ones the answer reads first. The
  // standing counts the same reviews, so accepting them all answers the stop for review on every surface.
  let acceptOpen = $state(false);
  const acceptable = $derived.by<Acceptable[]>(() => {
    if (!project || inspection || !can("review")) return [];
    const rests = (t: { task_id: string }) => restsOn.includes(t.task_id);
    return analysesDone.filter((t) => currentReview(t.task_id)?.decision !== "accepted")
      .sort((a, b) => Number(rests(b)) - Number(rests(a)))
      .map((t) => ({ task_id: t.task_id, title: titleFor(t.task_id), method: methodOf(t.task_id), written: writtenByAgent(t.task_id),
        checks: checkWords(t.task_id) || "No recorded check", review: reviews[t.task_id] ? reviewWords(t.task_id) : "", rests: rests(t) }));
  });
  function accepted(count: number) {
    notify(count === 1 ? "Your review is saved: you accepted the analysis." : `Your reviews are saved: you accepted ${count} analyses.`);
    void load();
  }

  // Each step's checks in one phrase: the methods check of the decision that proposed it and its files' checksums.
  const proposedBy = (taskId: string) => runs.flatMap(({ turns }) => turns).find((t) => t.record.submitted_tasks.some((s) => s.task_id === taskId));
  const intactOf = (taskId: string) => {
    const packages = evidence[taskId]?.packages ?? [];
    if (!packages.length) return null;
    return packages.every((pkg) => pkg.view.integrity.hasList && !pkg.view.integrity.mismatched.length && !pkg.view.integrity.missing.length);
  };
  function checkWords(taskId: string): string {
    const turn = proposedBy(taskId), intact = intactOf(taskId);
    if (intact === false) return "A result file does not match its checksum list";
    if (turn && turn.record.method_review.decision !== "proceed") return intact ? "Files intact; the methods check asked for review" : "The methods check asked for review";
    if (turn && intact) return "Checks passed";
    if (intact) return "Files match their checksums";
    if (turn) return "Methods check agreed";
    return "";
  }
  const reviewWords = (taskId: string) => {
    const review = reviews[taskId];
    if (review) return reviewMark(review, identity.principal.id).label;
    const task = taskFor(taskId);
    return task && !task.figure && (task.status === "completed" || task.status === "review_required") ? "Not yet reviewed by a person" : "";
  };
  // Analyses outside the plan: run by a person, figure versions, or steps since taken out.
  const planned = $derived(new Set(research?.plan.map((s) => s.task_id).filter((t): t is string => !!t) ?? []));
  const unplanned = $derived(ordered.filter((t) => !planned.has(t.task_id)));
  // A long plan folds its older steps into one row, so the working end stays in view.
  const SHOWN_STEPS = 6;
  const folded = $derived((research?.plan.length ?? 0) > SHOWN_STEPS + 2 ? (research?.plan.length ?? 0) - SHOWN_STEPS : 0);
  let composerOpen = $state(false), rerunOpen = $state(false), fullQuestion = $state(false);
  async function tellTheAgent() {
    composerOpen = true;
    await reveal("next");
    document.getElementById("ask-text")?.focus({ preventScroll: true });
  }
  function openFigure(taskId: string, path: string, preview: string, element: HTMLElement) {
    chosenFigure.task = taskId; chosenFigure.path = path; chosenFigure.preview = preview;
    // The plate carries this name into the page change; the analysis page's preview takes it up.
    if (preview) element.style.setProperty("view-transition-name", "chosen-figure");
    openTask(taskId);
  }
  // Figures are numbered through the results, like a paper; each analysis page numbers its own.
  const numbering = $derived.by(() => {
    const starts: Record<string, number> = {};
    let figure = 1;
    for (const task of ordered) {
      starts[task.task_id] = figure;
      for (const pkg of evidence[task.task_id]?.packages ?? []) figure += pkg.view.figures.length;
    }
    return starts;
  });
  const PLATES = 4;
  const groupName = $derived(project ? identity.groups.find((g) => g.key === project!.group_key)?.name ?? project.group_key : "");
  // "A colleague" opens a sentence; after "Started by" it reads in lower case.
  const starter = $derived(!project ? "" : project.owner_id === identity.principal.id ? "Started by you"
    : project.owner?.kind === "service_account" ? "Started automatically"
    : `Started by ${personName(project.owner_id).replace(/^A (colleague|person)$/, "a $1")}`);
  const sentences = (items: string[]) => items.map((c) => c.trim()).filter(Boolean).map((c) => (/[.!?]$/.test(c) ? c : `${c}.`)).join(" ");

  async function pause() {
    if (!activeRun || pausing) return;
    pausing = true;
    try { await projects.pause(activeRun.run_id); notify("The agent is paused. Analyses already running keep saving their files."); void load(); }
    catch { notify("The agent could not be paused. Reload and try again.", true); }
    finally { pausing = false; }
  }
  function changed(message: string) { notify(message); void load(); }
  const counts = (record: TaskEvidence) => {
    const figures = record.packages.reduce((n, p) => n + p.view.figures.length, 0), tables = record.packages.reduce((n, p) => n + p.view.tables.length, 0);
    const words = [figures ? `${figures} ${figures === 1 ? "figure" : "figures"}` : "", tables ? `${tables} ${tables === 1 ? "table" : "tables"}` : ""].filter(Boolean);
    return words.length ? words.join(" and ") : "No figures or tables; its files are in the analysis";
  };

  // Ask for changes: the instruction is saved with the question as a new version, then the agent
  // starts again with the methods and limits of its last work session. Each request carries its own
  // key, so a repeated click cannot save twice or start a second session.
  let ask = $state("");
  let asking = $state(false);
  let askFailure = $state("");
  let askKey = "", runKey = "", askBody = "";
  // A person can instruct the agent while it works (steer it when it heads the wrong way). With a work
  // session active the instruction is only saved: the agent reads it at its next decision, and the coordinator starts a
  // decision already under way again from the new version. Otherwise the agent starts again with the same methods.
  const canAsk = $derived(!inspection && !!research && can("steer") && can("contribute") && !!latestRun);
  async function sendInstruction() {
    if (!project || !research || !latestRun || asking) return;
    const running = !!activeRun;
    const next = withInstruction(research, ask);
    if (!next) {
      askFailure = research.constraints.length >= 32 ? "This question already carries 32 instructions and constraints. Edit the question to combine some first."
        : `Write what you want in ${MAX_INSTRUCTION} characters or fewer.`;
      return;
    }
    const body = JSON.stringify({ next, revision: project.revision });
    if (body !== askBody) { askBody = body; askKey = crypto.randomUUID(); runKey = crypto.randomUUID(); }
    asking = true; askFailure = "";
    let saved = false;
    try {
      const version = await projects.checkpoint(project.project_id, { expected_revision: project.revision, request_id: askKey, state: next });
      saved = true;
      if (!running) await projects.start(project.project_id, { run_id: runKey, expected_revision: version.revision, allowed_profiles: [...latestRun.allowed_profiles].sort(),
        max_turns: latestRun.max_turns, max_parallel: latestRun.max_parallel });
      ask = ""; askBody = "";
      notify(running ? "Your instruction is saved. The agent reads it at its next decision." : "Your instruction is saved with the question, and the agent has started again.");
      void load();
    } catch (cause) {
      void load();
      // Saved but not started: most often someone started the agent meanwhile, and it reads the instruction anyway.
      if (saved) { ask = ""; askBody = ""; askFailure = "Your instruction is saved, but the agent was not started again. If it is not working now, use Continue with different methods."; return; }
      askFailure = cause instanceof BrokerError && (cause.code === "request_conflict" || cause.code === "revision_conflict")
        ? "The project changed while you were writing. Your text is still here; send it again."
        : cause instanceof BrokerError && cause.code === "capacity_reached" ? "The shared analysis queue is full. Your instruction is kept here; try again shortly."
        : "The instruction was not sent. Your text is still here.";
    } finally { asking = false; }
  }

  // Withdrawing an instruction saves a version without it, after a confirmation in the page. The agent stops following
  // it at its next decision; the text stays in the project's history.
  const canWithdraw = $derived(!inspection && !!research && can("steer"));
  let withdrawing = $state(""), withdrawBusy = $state(false), withdrawFailure = $state("");
  let withdrawKey = "", withdrawBody = "";
  const instructionKey = (item: InstructionHistory) => `${item.revision}:${item.text}`;
  async function withdraw(item: InstructionHistory) {
    if (!project || !research || withdrawBusy) return;
    const next = withoutInstruction(research, item.text);
    if (!next) { withdrawFailure = "This instruction is no longer in the saved question. Reload the page to see its current state."; return; }
    const body = JSON.stringify({ next, revision: project.revision });
    if (body !== withdrawBody) { withdrawBody = body; withdrawKey = crypto.randomUUID(); }
    withdrawBusy = true; withdrawFailure = "";
    try {
      await projects.checkpoint(project.project_id, { expected_revision: project.revision, request_id: withdrawKey, state: next });
      withdrawing = ""; withdrawBody = "";
      notify("The instruction is withdrawn. The agent stops following it at its next decision.");
      void load();
    } catch (cause) {
      withdrawFailure = cause instanceof BrokerError && (cause.code === "request_conflict" || cause.code === "revision_conflict")
        ? "The project changed a moment ago. Reload the page and withdraw it again."
        : "The instruction was not withdrawn. Try again.";
    } finally { withdrawBusy = false; }
  }
</script>

{#snippet valueButton(part: { slot: string; value: string | null; binding: Binding }, key: string)}
  {#if part.value !== null}
    {@const source = valueOf(part.binding, part.value)}
    <button type="button" class="val" class:kept={kept?.key === key} aria-pressed={wide ? kept?.key === key : undefined}
      aria-label={`${display(part.value)}. ${sourceWords(part.binding)}. Show where it came from.`}
      onmouseenter={() => point(source)} onmouseleave={() => point(null)} onfocus={() => point(source)} onblur={() => point(null)}
      onclick={() => pick(key, source)}><data class="bound" value={part.value} title={`${sourceWords(part.binding)}: ${part.value}. The value is read from the result file, not written by the agent.`}>{display(part.value)}</data></button>
  {:else}<span class="bound missing" title={`${sourceWords(part.binding)}. The value could not be read from the result file.`}>value not available</span>{/if}
{/snippet}

{#snippet boundFinding(finding: Finding, n: number, prefix: string)}
  {@const task = artifactTask(Object.values(finding.bindings)[0]?.ref ?? "")}
  {@const flag = findingFlag([...new Set(Object.values(finding.bindings).map((b) => artifactTask(b.ref)))], reviews)}
  <li>
    <span class="fn" aria-hidden="true">{n}</span>
    <div class="finding-body">
      <p class="finding">{#each glued(parts(finding, digests)) as part, j (j)}{#if "text" in part}{part.text}{:else}<span class="nw">{part.before}{@render valueButton(part.value, `${prefix}${n}:${j}`)}{part.after}</span>{/if}{/each}</p>
      <p class="finding-meta">{#if task}<button class="link small" onclick={() => openTask(task)}>From {titleFor(task)}</button>{/if}
        {#if flag}<span class="small"><StatusMark tone={flag.tone} label={flag.label} wrap /></span>{/if}</p>
    </div>
  </li>
{/snippet}

{#snippet sourceNote(source: Source)}
  <div class="source">
    {#if source.kind === "value"}<p class="source-value">{source.shown}</p>{/if}
    <p class="source-what">{source.kind === "value" ? source.what : `Figure: ${source.what}`}</p>
    <dl class="facts">
      <div><dt>File</dt><dd class="mono">{source.file}</dd></div>
      <div><dt>{source.kind === "value" ? "Cell" : "Formats"}</dt><dd>{source.place}</dd></div>
      {#if source.kind === "value"}<div><dt>Exact text in the file</dt><dd class="mono">{source.exact}</dd></div>{/if}
      <div><dt>Made by</dt><dd>{source.made.step ? `Step ${source.made.step}: ` : ""}{source.made.title}. {source.made.method}.</dd></div>
      <div><dt>Data it read</dt><dd>{research?.workspace.input_refs.map((r) => inputWords(r).name).join(", ") || "Not recorded"}</dd></div>
      <div><dt>Checks</dt><dd><StatusMark tone={source.check.state === "matched" ? "done" : source.check.state === "mismatched" ? "failed" : "quiet"} label={source.check.words} wrap /></dd></div>
    </dl>
    <button class="link small" onclick={() => { sourceOpen = false; openTask(source.made.task); }}>How was this made?</button>
  </div>
{/snippet}

{#snippet about()}
  {@const packages = Object.values(evidence).flatMap((r) => r.packages)}
  {@const broken = packages.filter((pkg) => pkg.view.integrity.mismatched.length || pkg.view.integrity.missing.length).length}
  {@const people = Object.values(reviews).filter(byPerson)}
  <h2 class="about-title" id="about-title">{answered ? "About this answer" : "About this project"}</h2>
  <dl class="facts">
    {#if (research?.workspace.starting_points?.length ?? 0) > 1}<div><dt>Started from</dt><dd>{#each research!.workspace.starting_points! as point (point.key)}<span class="fact-line">{point.name}</span>{/each}</dd></div>{/if}
    <div><dt>Data used</dt><dd>{#each research?.workspace.input_refs ?? [] as ref (ref)}{@const words = inputWords(ref)}<span class="fact-line">{words.name} <span class="soft">({words.kind.toLowerCase()})</span></span>{/each}</dd></div>
    <div><dt>Methods</dt><dd>{[...new Set(ordered.filter((t) => !t.figure).map((t) => methodOf(t.task_id)))].join("; ") || (latestRun ? latestRun.allowed_profiles.map(profileName).join(", ") : "None run yet")}{#if ordered.some((t) => writtenByAgent(t.task_id))}. Code the agent wrote stays unvalidated until a person reads it.{/if}</dd></div>
    <div><dt>Checks</dt><dd>{#if !packages.length}No result files yet.{:else if broken}<StatusMark tone="failed" label={`${broken} of ${packages.length} result packages do not match their checksum lists`} wrap />{:else}<StatusMark tone="done" label="Every result file matches its recorded checksum" wrap />{/if}</dd></div>
    <div><dt>Reviewed by</dt><dd>{#if reviewsFailed}Reviews could not be read just now.{:else if people.length}{[...new Set(people.map((r) => reviewMark(r, identity.principal.id).label))].join("; ")}{:else}No person yet{/if}</dd></div>
    {#if latestRun && standing === "working"}<div><dt>Decisions</dt><dd>{latestRun.turn} of the {latestRun.max_turns} allowed for this work session</dd></div>{/if}
    <div><dt>Who can open it</dt><dd>{sharing(project!.visibility, groupName)}</dd></div>
    <div><dt>Rerun it</dt><dd>Download everything holds the code, inputs, settings and checksums.</dd></div>
  </dl>
  {#if wide && answered}<p class="hint">Point at a number or a figure to see where it came from.</p>{/if}
{/snippet}

{#snippet stepRow(task: Project["tasks"][number] | undefined, n: number | null, title: string, mark: { label: string; tone: Tone }, step?: ResearchState["plan"][number])}
  {@const record = task ? evidence[task.task_id] : undefined}
  {@const plates = record?.packages.flatMap((pkg) => pkg.view.figures.map((figure) => ({ pkg, figure }))) ?? []}
  {@const recovered = task?.status === "failed" && !!record?.artifacts.some((a) => a.relative_path.includes("/recovered/"))}
  {@const unsure = step?.status === "uncertain" ? "The system could not confirm its result files" : ""}
  {@const detail = task ? [unsure, methodOf(task.task_id) === title ? "" : methodOf(task.task_id), record?.packages.length ? counts(record) : activeTask(task.status) ? "Running now" : "",
    checkWords(task.task_id), reviewWords(task.task_id), recovered ? "Its output files were recovered by the operator" : ""].filter(Boolean)
    : [unsure || "Not started yet"]}
  <li class="story-step">
    <details>
      <summary>
        <span class="sn" aria-hidden="true">{n ?? ""}</span>
        <span class="story-body"><span class="story-title">{n ? `Step ${n}: ` : ""}{title}{#if task && isNew(task.task_id)}<span class="new">New</span>{/if}</span>
          <span class="story-detail">{detail.join(" · ")}</span></span>
        <span class="story-mark"><StatusMark tone={mark.tone} label={mark.label} /></span>
      </summary>
      <div class="story-open">
        {#if step?.rationale}<p class="agent-note"><span>The agent's reason.</span> {step.rationale}</p>{/if}
        {#if step && ledger}
          {@const moments = stepMoments(ledger, step.action_key, personName)}
          {@const earlier = earlierReasons(ledger, step.action_key)}
          {#if moments.length}
            <ol class="step-history small" aria-label="What happened to this step">
              {#each moments as moment, i (i)}<li><time datetime={moment.at}>{when(moment.at)}</time><span>{moment.text}</span></li>{/each}
            </ol>
          {/if}
          {#if earlier.length}
            <details class="earlier-reasons">
              <summary>{earlier.length === 1 ? "Its earlier reason" : `Its ${earlier.length} earlier reasons`}</summary>
              {#each earlier as reason (reason.at)}<p class="small soft">{reason.rationale} <span class="faint">(<time datetime={reason.at}>{when(reason.at)}</time>)</span></p>{/each}
            </details>
          {/if}
        {/if}
        {#if task}
          {@const turn = proposedBy(task.task_id)}
          {@const review = reviews[task.task_id]}
          <ul class="step-checks" aria-label="Its checks">
            {#if turn}<li><StatusMark tone={turn.record.method_review.decision === "proceed" ? "done" : "attention"} label={turn.record.method_review.decision === "proceed" ? "Methods check agreed" : "Methods check asked for review"} />
              <span class="small soft">{turn.record.method_review.reason} A second model's review of the agent's proposal, advisory only.</span></li>{/if}
            {#if record?.packages.length}
              {@const intact = intactOf(task.task_id)}
              <li><StatusMark tone={intact ? "done" : "failed"} label={intact ? "Result files intact" : "A result file does not match"} />
                <span class="small soft">{intact ? "Its result package matches the checksum list recorded when it ran." : "One package does not match its checksum list; it is still downloadable."}</span></li>
            {/if}
            {#if review}{@const markOf = reviewMark(review, identity.principal.id)}<li><StatusMark tone={markOf.tone} label={markOf.label} wrap />
              {#if review.note}<span class="review-note">{review.note}</span>{/if}
              <span class="small soft">{when(review.created_at)}{review.covers_current_outputs ? "" : "; the result files changed after it"}.</span></li>
            {:else if !turn && !record?.packages.length}<li><StatusMark tone="quiet" label="No recorded check yet" /></li>{/if}
          </ul>
          {#if evidenceFailed[task.task_id] && !record}
            <div class="load-failed" role="alert"><p class="small">Its files could not be read just now. The analysis record is unchanged.</p>
              <button class="link small" onclick={() => retryEvidence(task.task_id)}>Try again</button></div>
          {:else if plates.length}
            <ul class="plates" aria-label={`Figures from ${titleFor(task.task_id)}`}>
              {#each plates.slice(0, PLATES) as { pkg, figure }, j (figure.display.path)}
                {@const caption = task.figure && plates.length === 1 ? task.figure.title : figure.title}
                <li><FigurePlate file={fromMember(figure.display, caption)} title={caption} number={(numbering[task.task_id] ?? 1) + j}
                  load={(signal) => verifiedMember(task.task_id, pkg.artifact, figure.display, signal)}
                  onOpen={(preview, element) => openFigure(task.task_id, figure.display.path, preview, element)} /></li>
              {/each}
            </ul>
            {#if plates.length > PLATES}<p class="small soft">{plates.length - PLATES} more figures in the analysis.</p>{/if}
          {:else if record?.unreadable.length}
            <p class="small soft">The result package could not be opened here ({record.unreadable[0].code.replaceAll("_", " ")}). It is still available to download from the analysis.</p>
          {:else if record && record.artifacts.length && !record.packages.length}
            <p class="small soft">{record.artifacts.length} retained {record.artifacts.length === 1 ? "file" : "files"}, such as {fileTitle(record.artifacts[0].relative_path)}.</p>
          {/if}
          <a class="link small" href={href({ name: "analysis", project: projectId, task: task.task_id })} onclick={(e) => { e.preventDefault(); openTask(task.task_id); }}>Open the analysis</a>
        {/if}
      </div>
    </details>
  </li>
{/snippet}

{#if loading && !project}
  <div class="page-state" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Opening the project…</div>
{:else if failure && !project}
  <div class="page-state"><h1>Project unavailable</h1><p>{failure}</p><a class="link" href="#/">Back to your research</a></div>
{:else if project}
  <article class="page" class:with-margin={wide && !!research}>
    <div class="col">
      {#if inspection}
        <div class="inspection change-bar" role="status">
          <p><strong>Administrator inspection.</strong> You can read this project until {when(inspection)}. Your reason is in its access record, which the owner can see.</p>
          <button class="button secondary" onclick={onEndInspection}>End inspection</button>
        </div>
      {/if}
      <header class="head">
        <p class="running"><span class="lab-name">{groupName}</span> · {starter} on {when(project.created_at)} · <span class="who-can">{sharing(project.visibility, groupName)}</span></p>
        <h1>{project.title}</h1>
        {#if research}
          <p class="question" class:clamped={!fullQuestion}><span class="visually-hidden">Question: </span>{research.goal}</p>
          <button class="link small more" aria-expanded={fullQuestion} aria-controls="question-limits" onclick={() => (fullQuestion = !fullQuestion)}>
            {fullQuestion ? "Show less" : "Full question and limits"}</button>
          {#if fullQuestion}
            <dl class="facts limits" id="question-limits">
              <div><dt>Study constraints</dt><dd>{studyConstraints.length ? sentences(studyConstraints) : "None saved"}</dd></div>
              {#if latestRun}<div><dt>Limits of the work session</dt><dd>{latestRun.max_turns} decisions before it stops for review, {latestRun.max_parallel} {latestRun.max_parallel === 1 ? "analysis" : "analyses"} at the same time</dd></div>{/if}
              <div><dt>Saved version</dt><dd>{research ? project.checkpoint?.revision ?? project.revision : project.revision}, {when(project.checkpoint?.created_at ?? project.updated_at)}</dd></div>
            </dl>
          {/if}
        {/if}
      </header>

      {#if !research}
        {#if can("steer") && !inspection}<WorkspaceSetup {project} labName={groupName} update={(p) => { project = p; projectIndex.remember(p); void load(); }} />
        {:else}<p class="soft">The owner has not saved a research question yet.</p>{/if}
      {:else}
        {#snippet statusActions()}
          {#if standing === "working"}
            {#if canRun && activeRun}<button class="button secondary" onclick={pause} disabled={pausing}>{pausing ? "Pausing…" : "Pause the agent"}</button>{/if}
            {#if canAsk}<button class="button secondary" onclick={() => void tellTheAgent()}>Tell the agent</button>{/if}
            <button class="link watch-toggle" aria-expanded={watching} aria-controls="section-watch" onclick={watch}>Watch it work</button>
          {:else if standing === "waiting" && answer}<button class="button" onclick={() => void reveal("found")}>Review the answer</button>
          {:else if standing === "waiting"}<button class="button" onclick={() => void reveal("record")}>Review the record</button>
          {:else if standing === "paused" && canRun && latestRun}<button class="button" onclick={() => void resume()} disabled={resuming}>{resuming ? "Starting…" : "Resume"}</button>
          {:else if standing === "not_finished" && canRun && latestRun}<button class="button" onclick={() => void resume()} disabled={resuming}>{resuming ? "Starting…" : "Try again"}</button>
          {:else if !latestRun && canRun}<button class="button" onclick={() => (startOpen = true)}>Start the research</button>{/if}
        {/snippet}
        {#snippet pinnedAction()}
          {#if standing === "working" && canRun && activeRun}<button class="button secondary compact" onclick={pause} disabled={pausing}>{pausing ? "Pausing…" : "Pause"}</button>
          {:else if standing === "waiting" && answer}<button class="button compact" onclick={() => void reveal("found")}>Review the answer</button>
          {:else if (standing === "paused" || standing === "not_finished") && canRun && latestRun}<button class="button compact" onclick={() => void resume()} disabled={resuming}>{standing === "paused" ? "Resume" : "Try again"}</button>{/if}
        {/snippet}
        <StatusBlock {standing} sentence={sentence.text} seen={seenWords} now={doing} {pace} steps={stepMarks} {stepWords} {link} {lastRead}
          hint={standing === "working" ? "You can leave this page. It keeps working, and Waiting for you shows it when it needs you." : ""}
          actions={statusActions} pinned={pinnedAction} />

        {#if since}<SinceList at={since} rows={sinceRows} go={goSince} />{/if}

        {#if watching}<WatchFeed lines={feed} close={() => { watching = false; void tick().then(() => document.querySelector<HTMLElement>(".watch-toggle")?.focus()); }}
          {openTask} link={(task) => href({ name: "analysis", project: projectId, task })} showRecord={() => void reveal("record")} />{/if}

        <section id="section-found" class="block found" aria-labelledby="found-title">
          <h2 id="found-title" class="section-title" tabindex="-1">{answered ? "What it found" : "What it found so far"}</h2>
          {#if answer && answer.answer !== "not_yet"}
            <p class="reading-label">{answerAccepted ? "The agent's reading. A person accepted the analyses it rests on." : "The agent's reading, not checked by a person."}</p>
            <p class="answer-line">{answerWords[answer.answer]}</p>
          {:else}
            <p class="answer-line quiet">No answer yet.{stepWords ? ` ${stepWords}.` : ""}{answer ? " The agent has said it cannot tell yet." : ""}</p>
          {/if}
          {#if answer?.findings.length}
            <ol class="finds" aria-label="The agent's findings">
              {#each answer.findings as finding, i (i)}{@render boundFinding(finding, i + 1, "")}{/each}
            </ol>
          {/if}
          {#each answer?.tables ?? [] as table, i (i)}<AnswerTableView {table} {digests} source={tableAnalysis(table)} flag={findingFlag([artifactTask(table.ref)].filter(Boolean), reviews, "table")} />{/each}
          {#if answer?.parts.length}
            <dl class="answer-parts" aria-label="The parts of your question, as the agent reads them">
              {#each answer.parts as part, i (i)}
                <div class:open-part={part.state !== "supported" && part.state !== "unsupported"}><dt>{partStateWords[part.state]}</dt><dd>{part.part}{#if part.reason}<span class="part-reason">{part.reason}</span>{/if}</dd></div>
              {/each}
            </dl>
          {/if}
          {#if latestRun && answer && canWriteDown(latestRun.status)}
            <AnswerExport project={projectId} run={latestRun.run_id} title={project?.title ?? ""} canAsk={!inspection && can("contribute")} />
          {/if}
          {#if lead}
            {@const leadSource = figureSource(lead.task.task_id, lead.figure, made)}
            <figure class="lead" onmouseenter={() => point(leadSource)} onmouseleave={() => point(null)} onfocusin={() => point(leadSource)} onfocusout={() => point(null)}>
              <FigurePlate file={fromMember(lead.figure.display, lead.figure.title)} title={lead.figure.title} number={lead.number} width={640} lead
                load={(signal) => verifiedMember(lead.task.task_id, lead.pkg.artifact, lead.figure.display, signal)}
                onOpen={(preview, element) => openFigure(lead.task.task_id, lead.figure.display.path, preview, element)} />
              <figcaption class="small soft">From {stepIndex(lead.task.task_id) ? `step ${stepIndex(lead.task.task_id)}, ` : ""}{titleFor(lead.task.task_id)}.
                <button class="link small" class:kept={kept?.key === "lead"} onclick={() => pick("lead", leadSource)}>Where it came from</button></figcaption>
            </figure>
          {/if}
          {#if strip.length}
            <ul class="plates strip" aria-label={answered ? "More figures" : "Figures so far"}>
              {#each strip as item (`${item.task.task_id}:${item.figure.display.path}`)}
                <li><FigurePlate file={fromMember(item.figure.display, item.figure.title)} title={item.figure.title} number={item.number}
                  load={(signal) => verifiedMember(item.task.task_id, item.pkg.artifact, item.figure.display, signal)}
                  onOpen={(preview, element) => openFigure(item.task.task_id, item.figure.display.path, preview, element)} />
                  {#if isNew(item.task.task_id)}<span class="new">New</span>{/if}</li>
              {/each}
            </ul>
          {/if}
          {#if figureList.length || tableCount}
            <p class="small"><button class="link" onclick={() => void reveal("steps")}>All results, by step: {[figureList.length ? `${figureList.length} ${figureList.length === 1 ? "figure" : "figures"}` : "", tableCount ? `${tableCount} ${tableCount === 1 ? "table" : "tables"}` : ""].filter(Boolean).join(" and ")}</button></p>
          {/if}
          {#if answers.earlier.length}
            <details class="earlier-answers">
              <summary>Earlier findings</summary>
              <p class="small soft">{answer?.answer === "not_yet" ? "The agent has not restated these findings since. They are history, not its current answer." : "The agent's newer answer above replaces these findings."}</p>
              {#each answers.earlier as past, k (past.at)}
                <div class="earlier-answer">
                  <p class="small"><time datetime={past.at}>{when(past.at)}</time>. {answerWords[past.answer]}</p>
                  {#if past.findings.length}
                    <ol class="finds" aria-label={`The agent's findings from ${when(past.at)}`}>
                      {#each past.findings as finding, i (i)}{@render boundFinding(finding, i + 1, `e${k}:`)}{/each}
                    </ol>
                  {/if}
                  {#each past.tables as table, i (i)}<AnswerTableView {table} {digests} source={tableAnalysis(table)} flag={findingFlag([artifactTask(table.ref)].filter(Boolean), reviews, "table")} />{/each}
                </div>
              {/each}
            </details>
          {/if}

          {#if reading || suggestion || findings.length || readNotes.length || readPapers.length}
          <section id="section-findings" class="notes" aria-labelledby="findings-title">
            <h3 id="findings-title">The agent's notes</h3>
            {#if reading}<p class="agent-note"><span>{answered ? "The agent's summary." : "The agent's note at its last decision. It is not an answer."}</span> {reading}</p>{/if}
            {#if suggestion}<p class="agent-note"><span>What it suggests next.</span> {suggestion}</p>{/if}
            {#if findings.length}
              <ul class="lessons">
                {#each findings as finding (finding.statement)}
                  {@const tasks = [...new Set(finding.evidence_refs.map(taskFromRef).filter((t): t is string => !!t))]}
                  {@const shared = sharedAs(finding.statement, project!.project_id, labMemory?.notes ?? [])}
                  <li>
                    <p class="finding-label small">{lessonLabel(finding.kind)}</p>
                    <p class="lesson">{finding.statement}</p>
                    {#if tasks.length}<p class="small">It relies on {#each tasks as t, i (t)}{i ? ", " : ""}<button class="link" onclick={() => openTask(t)}>{titleFor(t)}</button>{/each}.</p>{/if}
                    {#if shared}
                      {@const status = noteStatus(shared)}
                      <p class="small shared-line"><StatusMark tone={status.tone} label={`Shared with the lab: ${status.label.toLowerCase()}`} wrap />
                        <a class="link" href="#/notes">See the lab's shared notes</a></p>
                    {:else if curate && finding.kind === "observation"}
                      {@const blocker = shareBlocker(finding, reviews, titleFor)}
                      {#if blocker}<p class="small soft">{blocker}</p>
                      {:else}<ShareNote project={project!.project_id} revision={project!.checkpoint?.revision ?? project!.revision} lesson={finding} labName={groupName} onShared={sharedNow} />{/if}
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
            {#if readNotes.length}
              <div class="read-notes">
                <p class="small">At its latest step the agent also read {readNotes.length === 1 ? "a note" : `${readNotes.length} notes`} shared in {groupName}:</p>
                <ul>
                  {#each readNotes as item (item.note_id)}
                    <li class="small">{#if item.note}“{item.note.statement}” <span class="soft">From {item.note.project_title ?? "another project"}.</span>
                      {:else}<span class="soft">A note that is no longer in the lab's list.</span>{/if}</li>
                  {/each}
                </ul>
              </div>
            {/if}
            {#if readPapers.length}
              <div class="read-notes read-papers">
                <p class="small">{`At its latest step the agent read passages from ${readPapers.length === 1 ? "a paper" : `${readPapers.length} papers`} in the library of ${groupName}${readPapers.some((item) => item.state === "commons") ? " and the Commons" : ""}:`}</p>
                <ul>
                  {#each readPapers as item (item.paper_id)}
                    <li class="small">{#if item.paper}<a class="link" href={href({ name: "paper", paper: item.paper_id })}>{item.paper.title?.trim() || "An untitled paper"}</a>{item.pages.length ? `, ${item.pages.join(", ")}` : ""}
                      {:else if item.state === "commons"}<a class="link" href={href({ name: "commons" })}>{item.title?.trim() || "A paper"}</a>{`, from the Commons${item.pages.length ? `, ${item.pages.join(", ")}` : ""}`}
                      {:else}<span class="soft">{`${paperReadWords(item).charAt(0).toUpperCase()}${paperReadWords(item).slice(1)}.`}</span>{/if}</li>
                  {/each}
                </ul>
                <p class="small soft">{lastTurn?.record.context_inputs?.literature?.via === "knowledge" ? "The lab's search found them from the question, by its words and meaning." : "A word search found them by the words they share with the question."} The agent reads them as background; no finding rests on a paper.</p>
              </div>
            {/if}
          </section>
          {/if}
        </section>

        <section id="section-next" class="block next" aria-labelledby="next-title">
          <h2 id="next-title" class="section-title" tabindex="-1">Your next step</h2>
          <div class="next-actions">
            {#if acceptable.length && !activeRun}<button class="button" onclick={() => (acceptOpen = true)}>Accept the result</button>{/if}
            {#if canAsk}<button class="button secondary" aria-expanded={composerOpen} aria-controls="ask-box" onclick={() => { composerOpen = !composerOpen; if (composerOpen) void tick().then(() => document.getElementById("ask-text")?.focus()); }}>{activeRun ? "Tell the agent" : "Ask for changes"}</button>{/if}
            <button class="button secondary" onclick={() => (packageOpen = true)}>Download everything</button>
            <button class="link" aria-expanded={rerunOpen} aria-controls="rerun-box" onclick={() => (rerunOpen = !rerunOpen)}>Rerun on your computer</button>
          </div>
          {#if rerunOpen}
            <div class="rerun" id="rerun-box">
              <p>Download everything gives one file with this project's complete record: every version of the question, every agent decision, each analysis record and the original result files with their checksums.</p>
              <p>Each R analysis inside carries its own README with the exact code, input data, parameters, seed, environment record and checksums. Extract it with <code>tar -xzf payload.tar.gz</code> and follow that README.</p>
              <p class="small soft">Each analysis worked on a sealed copy of its inputs; its page names the exact copy it read and that copy's checksum.</p>
            </div>
          {/if}
          {#if canAsk && composerOpen}
            <div class="ask" id="ask-box">
              <label for="ask-text">Tell the agent what to change or do next</label>
              <textarea id="ask-text" bind:value={ask} rows="3" maxlength={MAX_INSTRUCTION} placeholder="For example: Also test whether the effect depends on the delivery method, and show each group's raw values."></textarea>
              <div class="ask-row">
                <p class="small soft">{activeRun ? "Saves your instruction with the question. The agent reads it at its next decision; analyses already running are not stopped."
                  : "Saves a new version of the question with your instruction, then starts the agent again with the same methods."}</p>
                <button class="button" onclick={sendInstruction} disabled={asking || !ask.trim()}>{asking ? "Sending…" : activeRun ? "Send to the agent" : "Send and start again"}</button>
              </div>
              {#if askFailure}<p class="notice error" role="alert">{askFailure}</p>{/if}
            </div>
          {/if}
          <div class="row-actions">
            {#if !inspection && can("steer")}<button class="link small" onclick={() => (editOpen = true)}>Edit the question</button>{/if}
            {#if canRun && latestRun && !activeRun}<button class="link small" onclick={() => (startOpen = true)}>Continue with different methods</button>{/if}
            {#if canRun}<button class="link small" onclick={() => onRunIndividual(project!)}>Run one method yourself</button>{/if}
          </div>
          {#if instructions.length}
            <div class="instructions">
              <h3>Instructions to the agent</h3>
              <ol>
                {#each instructions as item (instructionKey(item))}
                  {@const who = personName(item.author_id)}
                  <li class:withdrawn={!!item.withdrawn}>
                    <p>{item.text}</p>
                    <p class="small soft">{who === "you" ? "You" : who}, {when(item.at)} · version {item.revision}</p>
                    <p class="small soft read">{#if item.withdrawn}{@const by = personName(item.withdrawn.author_id)}Withdrawn by {by.startsWith("A ") ? by.toLowerCase() : by}, {when(item.withdrawn.at)}, {item.read ? "after the agent read it" : "before the agent read it"}.{:else if item.read}The agent read it at its decision of {when(item.read.at)}.{:else}Not read yet: the agent has not decided anything since.{/if}</p>
                    {#if !item.withdrawn && canWithdraw}
                      {#if withdrawing === instructionKey(item)}
                        <div class="withdraw" role="group" aria-label="Withdraw this instruction">
                          <p class="small">Withdraw this instruction? The agent stops following it at its next decision, and it stays in the project's history.</p>
                          <div class="withdraw-row">
                            <button class="button secondary" onclick={() => withdraw(item)} disabled={withdrawBusy}>{withdrawBusy ? "Withdrawing…" : "Withdraw"}</button>
                            <button class="link" onclick={() => { withdrawing = ""; withdrawFailure = ""; }} disabled={withdrawBusy}>Keep it</button>
                          </div>
                          {#if withdrawFailure}<p class="notice error" role="alert">{withdrawFailure}</p>{/if}
                        </div>
                      {:else}
                        <button class="link small" onclick={() => { withdrawing = instructionKey(item); withdrawFailure = ""; }}>Withdraw</button>
                      {/if}
                    {/if}
                  </li>
                {/each}
              </ol>
            </div>
          {/if}
        </section>

        {#if orientation}
          {@const o = orientation.orientation}
          {@const ran = o.profiles.filter((p) => orientation!.step_keys.includes(p.action_key))}
          <section id="section-orientation" class="block" aria-labelledby="orientation-title">
            <h2 id="orientation-title" class="section-title" tabindex="-1">Before it planned</h2>
            <p>{o.question}</p>
            <dl class="facts">
              {#if o.sources.length}<div><dt>Data it expected to read</dt><dd>{#each o.sources as s (s.key)}<span class="fact-line">{profileName(s.key)} <span class="soft">{s.why}</span></span>{/each}</dd></div>{/if}
              {#if o.tools.length}<div><dt>Methods it expected to use</dt><dd>{#each o.tools as t (t.key)}<span class="fact-line">{profileName(t.key)} <span class="soft">{t.why}</span></span>{/each}</dd></div>{/if}
              {#if o.papers.length}<div><dt>Papers it drew on</dt><dd>{#each o.papers as p (p.ref)}<span class="fact-line">{p.establishes}</span>{/each}</dd></div>{/if}
              {#if o.earlier?.length}<div><dt>The lab's earlier work it drew on</dt><dd>{#each o.earlier as p (p.ref)}<span class="fact-line">{p.establishes}</span>{/each}</dd></div>{/if}
              {#if o.unknowns.length}<div><dt>What it did not know yet</dt><dd>{#each o.unknowns as u, i (i)}<span class="fact-line">{u}</span>{/each}</dd></div>{/if}
              {#if ran.length}<div><dt>First checks it proposed</dt><dd>{#each ran as p (p.action_key)}{@const step = research.plan.find((s) => s.action_key === p.action_key)}<span class="fact-line">{p.title} <span class="soft">({step ? stepStatus(step.status).label.toLowerCase() : "not run"})</span></span>{/each}</dd></div>{/if}
            </dl>
            <p class="small faint">The agent wrote this from what it was shown, before its first plan. Code checked that each data set, method, paper and earlier record it names was in front of it. It is the agent's reading, not a result.</p>
          </section>
        {/if}
        <section id="section-steps" class="block steps-block" aria-labelledby="steps-title">
          <h2 id="steps-title" class="section-title" tabindex="-1">How it got here {#if research.plan.length}<small>{research.plan.length === 1 ? "1 step" : `${research.plan.length} steps`}</small>{/if}</h2>
          {#if research.plan.length}
            <ol class="story grouped" aria-label="The agent's steps">
              {#if folded}
                <li class="story-fold">
                  <details>
                    <summary><span class="sn" aria-hidden="true"></span><span class="story-body"><span class="story-title">Steps 1 to {folded}</span>
                      <span class="story-detail">{tallyWords(stepLine(research.plan.slice(0, folded)).map((s) => s.mark)).join(", ")}{research.plan.slice(0, folded).some((s) => s.task_id && isNew(s.task_id)) ? "; some are new since you looked" : ""}</span></span></summary>
                    <ol class="story">
                      {#each research.plan.slice(0, folded) as step, i (step.action_key)}{@render stepRow(step.task_id ? taskFor(step.task_id) : undefined, i + 1, step.title, stepStatus(step.status), step)}{/each}
                    </ol>
                  </details>
                </li>
              {/if}
              {#each research.plan.slice(folded) as step, i (step.action_key)}{@render stepRow(step.task_id ? taskFor(step.task_id) : undefined, folded + i + 1, step.title, stepStatus(step.status), step)}{/each}
            </ol>
          {:else}
            <p class="soft">The agent has not planned a step yet.</p>
          {/if}
          {#if unplanned.length}
            <h3 class="sub">Also in this project</h3>
            <ol class="story grouped" aria-label="Analyses outside the plan">
              {#each unplanned as task (task.task_id)}{@render stepRow(task, null, titleFor(task.task_id), taskStatus(task.status))}{/each}
            </ol>
          {/if}
          {#if ledger}
            {@const gone = removedSteps(ledger, research.plan.map((s) => s.action_key), personName)}
            {#if gone.length}
              <h3 class="sub">Steps taken out of the plan</h3>
              <ul class="removed">
                {#each gone as step (step.key)}
                  <li><p class="step-title">{step.title}</p>
                    <p class="small faint">Taken out{step.by ? ` by ${step.by}` : ""}, <time datetime={step.at}>{when(step.at)}</time>{#if step.task_id}{" · "}<button class="link small" onclick={() => openTask(step.task_id!)}>Open its analysis</button>{/if}</p></li>
                {/each}
              </ul>
            {/if}
            {@const archived = archivedStepCount(ledger, research.plan.map((s) => s.action_key))}
            {#if archived}<p class="small faint">{ledger.truncated ? "At least " : ""}{archived} finished {archived === 1 ? "step was" : "steps were"} moved out of the plan to keep it within its limit. Their analyses and history stay in this project.</p>{/if}
          {/if}
          {#if research.plan.length}<p class="small faint">{[
            "Step titles and reasons are the agent's words unless a person revised them. Their statuses come from the system's analysis records; the dated list under each step is its saved history, with who or what changed it.",
            ...(ledger?.unrecorded_revisions ? ["Some recent changes are not in that history yet."] : []),
            ...(ledger?.truncated ? ["Only the newest 500 saved revisions are shown."] : []),
          ].join(" ")}</p>{/if}
          <a class="link" href={href({ name: "workbench", project: projectId })}>Every step and file, in the detailed view</a>
        </section>

        {#if !wide}<section class="block about" aria-labelledby="about-title">{@render about()}</section>{/if}
      {/if}

      <LabDataSection {project} labData={takesLabData} {openDataset} {notify} />

      <div class="share-line">
        <span>{sharing(project.visibility, groupName)}</span>
        <button class="link small" onclick={() => (manageOpen = true)}>{!inspection && can("manage") ? "Manage" : "Who can open it"}</button>
      </div>

      {#if research}
        <details class="record" id="section-record" bind:open={recordOpen}>
          <summary><svg class="disclosure" viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg>The full record: everything that happened, in order</summary>
          <section id="section-notebook" class="block" aria-label="Everything that happened, in order">
            <HistoryTimeline entries={history} {notes} {openTask} oldestFirst />
          </section>
        </details>
      {/if}

      <details class="technical">
        <summary><svg class="disclosure" viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg>Technical details</summary>
        <dl>
          <dt>Project ID</dt><dd>{project.project_id}</dd>
          <dt>Research group</dt><dd>{project.group_key}</dd>
          <dt>Saved version</dt><dd>{project.revision}</dd>
          <dt>Your permissions</dt><dd>{project.capabilities.join(", ")}</dd>
          {#if research}<dt>{(research.workspace.starting_points?.length ?? 0) > 1 ? "Starting points" : "Starting point"}</dt><dd>{research.workspace.starting_points?.map((p) => p.key).join(" ") || research.workspace.profile}</dd><dt>Input references</dt><dd>{research.workspace.input_refs.join(" ")}</dd>
            <dt>Image</dt><dd>{research.workspace.image_digest}</dd><dt>Source</dt><dd>{research.workspace.source_digest}</dd>{/if}
          {#each runs as { run } (run.run_id)}<dt>Work session</dt><dd>{run.run_id} · {run.status} · {run.code} · {run.turn}/{run.max_turns} decisions</dd>{/each}
          <dt>Analyses</dt><dd>{project.tasks.length} of {project.task_count - (project.lab_data?.length ?? 0) - (project.documents?.length ?? 0)} shown</dd>
        </dl>
      </details>
    </div>

    {#if wide && research}
      <aside class="margin" aria-label="Where it came from" aria-live="polite">
        {#if shownSource}{@render sourceNote(shownSource)}
          {#if kept && !pointed}<button class="link small back" onclick={() => (kept = null)}>Back to {answered ? "about this answer" : "about this project"}</button>{/if}
        {:else}{@render about()}{/if}
      </aside>
    {/if}
  </article>

  <Dialog bind:open={sourceOpen} title="Where it came from" sheet>{#if kept}{@render sourceNote(kept.source)}{/if}</Dialog>
  <Dialog bind:open={manageOpen} title="People and access" wide sheet>
    <section id="section-people" aria-label="People and access">
      <PeoplePanel projectId={project.project_id} manage={!inspection && can("manage")} {members} {audit} visibility={project.visibility} {groupName} onChanged={() => void load()} />
    </section>
  </Dialog>
  {#if research}<ResearchControls {project} {identity} {defaults} bind:startOpen bind:editOpen onChanged={changed} />{/if}
  {#if acceptOpen}<AcceptSheet bind:open={acceptOpen} analyses={acceptable} {openTask} onDone={accepted} />{/if}
  <EvidencePackage projectId={project.project_id} bind:open={packageOpen} />
{/if}

<style>
  .page-state { padding: 64px var(--gutter); width: 100%; max-width: 1080px; margin: 0 auto; display: grid; gap: 12px; justify-items: start; align-content: start; }
  .page-state[role="status"] { display: flex; align-items: flex-start; gap: 10px; color: var(--text-2); }
  .page-state[role="status"] .waveform { margin-top: 4px; }
  .page { --page-gap: 30px; padding: 30px var(--gutter) 24px; width: 100%; max-width: 1080px; margin: 0 auto; display: grid; grid-template-columns: minmax(0, 1fr); align-content: start; }
  .page.with-margin { grid-template-columns: minmax(0, 1fr) 300px; column-gap: 44px; }
  /* A column of flex items, so the status block's zero-height pin track can take back the gap it sits in. */
  .col { display: flex; flex-direction: column; gap: var(--page-gap); min-width: 0; }
  .inspection { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 12px 24px; margin-left: 16px; }
  .inspection p { max-width: 70ch; }
  .head { display: grid; gap: 8px; justify-items: start; }
  .running { font-size: var(--font-small); color: var(--text-2); }
  .running .lab-name, .running .who-can { font-weight: 600; color: var(--text); }
  h1 { font-size: clamp(28px, calc(3vw * var(--type-scale)), calc(38px * var(--type-scale))); letter-spacing: -0.022em; line-height: 1.1; min-width: 0; margin-top: 2px; }
  .question { font-family: var(--serif); font-size: var(--font-lede); line-height: 1.5; color: var(--text); max-width: 66ch; }
  .question.clamped { display: -webkit-box; -webkit-line-clamp: 3; line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }
  .more { margin-top: -2px; }
  .limits { margin-top: 6px; max-width: 70ch; }
  .block { display: grid; gap: 14px; min-width: 0; scroll-margin-top: calc(var(--bar-h) + 64px); }
  :global(#section-since) { scroll-margin-top: calc(var(--bar-h) + 64px); }

  /* What it found: the answer in the paper's serif, labeled as the agent's reading; values read from result files sit on
     a rule in the interface face, so data and the agent's words stay apart. */
  .reading-label { font-size: var(--font-xs); font-style: italic; font-weight: 600; color: var(--text-2); }
  .answer-line { font-family: var(--serif); font-size: calc(19px * var(--type-scale)); line-height: 1.45; color: var(--text); max-width: 66ch; }
  .answer-line.quiet { color: var(--text-2); font-size: calc(17px * var(--type-scale)); }
  .finds { list-style: none; margin: 0; padding: 0; max-width: 72ch; }
  .finds > li { display: grid; grid-template-columns: 26px minmax(0, 1fr); gap: 6px; padding: 12px 0; border-top: 1px solid var(--line); }
  .finds > li:last-child { border-bottom: 1px solid var(--line); }
  .fn { padding-top: 4px; font-size: var(--font-small); font-weight: 650; color: var(--mark); font-variant-numeric: tabular-nums; }
  .finding-body { display: grid; gap: 4px; justify-items: start; min-width: 0; }
  .finding { font-family: var(--serif); font-size: calc(18px * var(--type-scale)); line-height: 1.5; color: var(--text); }
  .finding-meta { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 14px; }
  .finding-meta .link, .story-open .link { text-align: start; }
  .val { display: inline; padding: 0 1px; margin: 0; border: 0; background: none; border-radius: 3px 3px 0 0; cursor: pointer; font: inherit;
    box-shadow: inset 0 -1.5px 0 var(--mark); transition: background-color 120ms var(--ease); }
  .val:hover, .val.kept { background: var(--hover); }
  .val:focus-visible { outline: 2px solid var(--focus); outline-offset: 1px; }
  .bound { font-family: var(--ui); font-size: 0.86em; font-variant-numeric: tabular-nums; font-weight: 650; color: var(--heading); white-space: nowrap; }
  .bound.missing { font-weight: 500; font-style: italic; color: var(--text-2); border-bottom: 1px dashed var(--line-2); }
  .answer-parts { display: grid; gap: 8px; margin: 0; max-width: 72ch; }
  .answer-parts div { display: grid; grid-template-columns: minmax(10rem, max-content) 1fr; gap: 2px 14px; padding-left: 14px; border-left: 2px solid var(--line-2); }
  .answer-parts div.open-part { border-left-color: var(--mark); }
  .answer-parts dt { font-size: var(--font-xs); font-weight: 650; color: var(--text-2); padding-top: 3px; }
  .answer-parts dd { margin: 0; font-family: var(--serif); font-size: calc(16px * var(--type-scale)); line-height: 1.55; color: var(--text); }
  .answer-parts .part-reason { display: block; font-family: var(--ui); font-size: var(--font-small); line-height: 1.5; color: var(--text-2); }
  .lead { margin: 4px 0 0; display: grid; gap: 8px; max-width: 620px; }
  .lead figcaption { display: flex; flex-wrap: wrap; gap: 4px 12px; align-items: baseline; }
  .link.kept { text-decoration-color: var(--mark); }
  .plates { list-style: none; margin: 4px 0 0; padding: 0; display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 190px)); gap: 16px 18px; width: 100%; }
  .strip { grid-template-columns: repeat(3, minmax(0, 1fr)); max-width: 620px; }
  .plates > li { display: grid; gap: 4px; align-content: start; }
  .new { margin-left: 6px; font-size: var(--font-xs); font-weight: 700; color: var(--heading); }
  .plates .new { margin-left: 0; }
  .earlier-answers { display: grid; gap: 8px; max-width: 72ch; }
  .earlier-answers summary { cursor: pointer; font-size: var(--font-small); font-weight: 650; color: var(--text-2); min-height: 32px; display: flex; align-items: center; }
  .earlier-answer { display: grid; gap: 6px; padding-top: 6px; }
  .earlier-answer .finding { color: var(--text-2); font-size: calc(16px * var(--type-scale)); }
  .notes { display: grid; gap: 10px; margin-top: 10px; padding-top: 16px; border-top: 1px solid var(--line); }
  .notes h3 { font-size: calc(15px * var(--type-scale)); color: var(--text-2); }
  .agent-note { font-family: var(--serif); font-size: calc(16px * var(--type-scale)); line-height: 1.55; color: var(--text); max-width: 72ch; }
  .agent-note span { font-family: var(--ui); font-size: var(--font-xs); font-weight: 650; color: var(--text-2); font-style: italic; margin-right: 4px; }
  .lessons { list-style: none; margin: 0; padding: 0; display: grid; gap: 14px; max-width: 72ch; }
  .lessons li { display: grid; gap: 4px; padding-left: 14px; border-left: 1px dashed var(--line-2); justify-items: start; }
  .finding-label { color: var(--text-3); font-style: italic; }
  .lesson { font-family: var(--serif); font-size: calc(16px * var(--type-scale)); line-height: 1.55; }
  .shared-line { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 14px; }
  .read-notes { display: grid; gap: 6px; max-width: 72ch; margin-top: 6px; padding-top: 12px; border-top: 1px solid var(--line); }
  .read-notes ul { margin: 0; padding-left: 18px; display: grid; gap: 4px; }

  /* Your next step: the decisions a scientist makes here, then the quieter ways to change the work. */
  .next-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
  .rerun { display: grid; gap: 8px; max-width: 72ch; padding: 14px 16px; border-radius: var(--r-3); background: var(--surface); box-shadow: var(--group-edge); }
  .ask { display: grid; gap: 10px; padding: 16px 18px; border-radius: var(--r-3); background: var(--surface); box-shadow: var(--group-edge); }
  .ask label { font-size: calc(16px * var(--type-scale)); }
  .ask-row { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px 20px; }
  .ask-row p { max-width: 60ch; }
  .row-actions { display: flex; gap: 6px 22px; flex-wrap: wrap; }
  .instructions { display: grid; gap: 10px; padding-top: 4px; }
  .instructions h3 { font-size: calc(16px * var(--type-scale)); }
  .instructions ol { margin: 0; padding-left: 20px; display: grid; gap: 10px; }
  .instructions li > p:first-child { font-family: var(--serif); font-size: calc(16px * var(--type-scale)); }
  .instructions li { display: grid; gap: 2px; justify-items: start; }
  .instructions li.withdrawn > p:first-child { color: var(--text-3); text-decoration: line-through; text-decoration-thickness: 1px; }
  .withdraw { display: grid; gap: 8px; margin-top: 4px; padding: 10px 12px; border-left: 2px solid var(--line-2); max-width: 70ch; }
  .withdraw-row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; }

  /* How it got here: each step on one line that opens in place. */
  .story { list-style: none; margin: 0; padding: 0; }
  .story-step + .story-step, .story-fold + .story-step { border-top: 1px solid var(--line); }
  .story summary { list-style: none; display: grid; grid-template-columns: 26px minmax(0, 1fr) auto; gap: 10px; align-items: start; padding: 13px 16px; cursor: pointer; min-height: 44px; }
  .story summary::-webkit-details-marker { display: none; }
  .story summary:hover { background: var(--row-hover); }
  .story summary:focus-visible { outline: 2px solid var(--focus); outline-offset: -2px; }
  .sn { font-weight: 650; color: var(--text-3); font-variant-numeric: tabular-nums; padding-top: 1px; }
  .story-body { display: grid; gap: 2px; min-width: 0; }
  .story-title { font-weight: 600; color: var(--heading); overflow-wrap: anywhere; }
  .story-detail { font-size: var(--font-small); color: var(--text-2); overflow-wrap: anywhere; }
  .story-mark { font-size: var(--font-small); font-weight: 600; white-space: nowrap; padding-top: 1px; }
  .story-open { display: grid; gap: 10px; justify-items: start; padding: 2px 16px 16px 52px; }
  .story-fold > details > .story { border-top: 1px solid var(--line); background: var(--canvas); }
  .step-history { list-style: none; margin: 0; padding: 0; display: grid; gap: 3px; max-width: 72ch; }
  .step-history li { display: grid; grid-template-columns: minmax(124px, max-content) minmax(0, 1fr); gap: 12px; color: var(--text-2); }
  .step-history time { color: var(--text-3); font-variant-numeric: tabular-nums; }
  .earlier-reasons { display: grid; gap: 6px; max-width: 72ch; }
  .earlier-reasons summary { cursor: pointer; font-size: var(--font-small); font-weight: 650; color: var(--text-2); min-height: 32px; display: flex; align-items: center; padding: 0; }
  .step-checks { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; max-width: 72ch; }
  .step-checks li { display: grid; gap: 2px; }
  .review-note { font-family: var(--serif); font-size: calc(15px * var(--type-scale)); white-space: pre-line; overflow-wrap: anywhere; }
  .load-failed { display: grid; gap: 4px; justify-items: start; }
  .sub { font-size: calc(15px * var(--type-scale)); color: var(--text-2); margin-top: 6px; }
  .removed { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); }
  .removed li { display: grid; gap: 4px; justify-items: start; padding: 12px 0 12px 16px; border-bottom: 1px solid var(--line); }
  .step-title { font-weight: 650; color: var(--heading); }

  /* Facts as label and value, for the margin, the sheet and the question's limits. */
  .facts { margin: 0; display: grid; gap: 11px; }
  .facts dt { font-size: var(--font-xs); font-weight: 650; color: var(--text-3); }
  .facts dd { margin: 2px 0 0; font-size: var(--font-small); line-height: 1.45; color: var(--text); overflow-wrap: anywhere; }
  .facts dd.mono { font-family: var(--mono); font-size: var(--font-xs); }
  .fact-line { display: block; }
  .about-title { font-size: calc(15px * var(--type-scale)); font-weight: 650; color: var(--heading); }
  .about { padding: 16px 18px; border-radius: var(--r-3); background: var(--surface); box-shadow: var(--group-edge); }
  .hint { font-size: var(--font-xs); color: var(--text-3); }
  /* Watching is optional: the action reads as a quiet link beside the two buttons, with a full-size target. */
  .watch-toggle { min-height: var(--control-h); padding: 0 4px; font-size: calc(14px * var(--type-scale)); }
  .compact { min-height: 34px; padding: 0 14px; }
  .margin { position: sticky; top: calc(var(--bar-h) + 20px); align-self: start; display: grid; gap: 14px; align-content: start; min-width: 0;
    max-height: calc(100vh - var(--bar-h) - 40px); overflow: auto; padding: 4px 0 12px 22px; border-left: 1px solid var(--line); }
  .source { display: grid; gap: 12px; justify-items: start; animation: rise 200ms var(--ease); }
  .back { justify-self: start; }
  .source-value { font-size: calc(34px * var(--type-scale)); font-weight: 650; line-height: 1.05; letter-spacing: -0.01em; font-variant-numeric: tabular-nums; color: var(--heading); }
  .source-what { font-size: var(--font-small); color: var(--text-2); }
  @keyframes rise { from { opacity: 0; transform: translateY(6px); } to { opacity: 1; transform: none; } }
  @media (prefers-reduced-motion: reduce) { .source { animation: none; } .val { transition: none; } }

  .share-line { display: flex; flex-wrap: wrap; align-items: baseline; gap: 6px 14px; padding-top: 14px; border-top: 1px solid var(--line); font-size: var(--font-small); color: var(--text-2); }
  .record > summary, details.technical > summary { cursor: pointer; min-height: 44px; display: flex; align-items: center; gap: 8px; font-weight: 650; color: var(--text-2); list-style: none; }
  .record > summary::-webkit-details-marker, details.technical > summary::-webkit-details-marker { display: none; }
  .disclosure { width: 14px; height: 14px; flex: none; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; transition: transform var(--t-fast) var(--ease); }
  details[open] > summary > .disclosure { transform: rotate(90deg); }
  @media (prefers-reduced-motion: reduce) { .disclosure { transition: none; } }
  .record[open] > summary { margin-bottom: 8px; }
  details.technical { margin-top: -10px; }
  @media (max-width: 640px) {
    .page { --page-gap: 24px; padding-top: 20px; }
    .answer-parts div { grid-template-columns: minmax(0, 1fr); }
    .strip { grid-template-columns: repeat(2, minmax(0, 1fr)); }
    .plates { grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
    .story summary { grid-template-columns: 22px minmax(0, 1fr); padding: 12px 14px; }
    .story-mark { grid-column: 2; }
    .story-open { padding: 2px 14px 14px 46px; }
    .next-actions .button { flex: 1 1 auto; }
    .ask-row .button { flex: 1 1 auto; }
    .about { padding: 14px; }
  }
</style>
