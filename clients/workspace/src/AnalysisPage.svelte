<script lang="ts">
  import { packageHolds } from "./package_view.ts";
  import { onDestroy, tick, untrack } from "svelte";
  import AnalysisDialog from "./AnalysisDialog.svelte";
  import AnalysisRecord from "./AnalysisRecord.svelte";
  import AnalysisResults from "./AnalysisResults.svelte";
  import AnalysisStatement from "./AnalysisStatement.svelte";
  import FigureEditor from "./FigureEditor.svelte";
  import FigureRecord from "./FigureRecord.svelte";
  import { chartName, figureGroups, figureRoot, shownVersion, versionStatus } from "./figure.ts";
  import { href } from "./route.ts";
  import { broker, BrokerError, packageContents, verifiedArtifact, verifiedMember, watchTask } from "./api.ts";
  import { sharing } from "./context.ts";
  import { isPackage, mergeEvents, terminal, type Artifact, type FigureCatalog, type FigureControls, type FigureVersion, type FigureVersionSummary, type GeneratedCode as Generated, type Identity, type Lineage, type PackageMember, type ReviewDecision, type StepLineage, type Task, type TaskEvent, type TaskQuery, type TaskReview } from "./contracts.ts";
  import { loadTaskEvidence, type TaskEvidence } from "./evidence.ts";
  import FileView from "./FileView.svelte";
  import QuickLook from "./QuickLook.svelte";
  import { artifactResult, memberResult, sameValues, type QuickItem, type ResultFile } from "./results.ts";
  import GeneratedCode from "./GeneratedCode.svelte";
  import QueryRecord from "./QueryRecord.svelte";
  import { activeTask, bytes, eventLabel, fileTitle, taskStatus, when } from "./language.ts";
  import { live } from "./live.ts";
  import { fileRole } from "./file_role.ts";
  import LineageRecord from "./LineageRecord.svelte";
  import LiveLog from "./LiveLog.svelte";
  import MethodRecord from "./MethodRecord.svelte";
  import { byPerson, coverageSentence, decisions, decisionWords, MAX_REVIEW_NOTE, notAPerson, reviewClaim, reviewMark, reviewProblem } from "./person_review.ts";
  import { library, papersCited, type PaperRead } from "./library.ts";
  import { chosenFigure, pageTitle, projectIndex } from "./project_index.svelte.ts";
  import { projects, type Project } from "./projects.ts";
  import { workingCopyLine, type WorkingCopy } from "./working_copy.ts";
  import RecordedSteps from "./RecordedSteps.svelte";
  import { holdInView } from "./scroll.ts";
  import StatusMark from "./StatusMark.svelte";
  import StepCheck from "./StepCheck.svelte";
  import { checksFor, type StepCheck as Checked } from "./step_checks.ts";
  import { fromArtifact, fromMember, MAX_TABLE_BYTES, saveBlob, saveName } from "./viewer.ts";
  import { MAX_FIGURE_DATA_BYTES, type FigureDataSource } from "./figure_data.ts";
  import { groupsIn, strictText, tableRead } from "./artifact_content.ts";
  import type { FigureHandle } from "./views/types.ts";
  import { analyses, analysisName, analysisStatus, type AnalysisCatalog, type AnalysisSettings, type AnalysisVersionSummary,
    type TemplateAnalysis } from "./template_analysis.ts";

  let { taskId, projectId, identity, openProject, openTask, notify }: {
    taskId: string; projectId?: string; identity: Identity; openProject: (id: string) => void;
    openTask: (task: string, project?: string) => void; notify: (message: string, error?: boolean) => void;
  } = $props();
  let task = $state<Task | null>(null);
  let events = $state<TaskEvent[]>([]);
  let evidence = $state<TaskEvidence | null>(null);
  let generated = $state<Generated | null>(null);
  let query = $state<TaskQuery | null>(null);
  // Where the analysis came from, read separately so a failed read shows as its own state.
  let lineage = $state<Lineage | StepLineage | null>(null);
  let lineagePhase = $state<"idle" | "loading" | "failed" | "ready">("idle");
  // The library papers the agent cited when it planned this analysis, from the decision that asked
  // for it; empty when it cited none or the records cannot be read.
  let cited = $state<PaperRead[]>([]);
  // This analysis's result checks as the agent read them in its run's decisions.
  let stepChecks = $state<Checked[]>([]);
  // People's reviews of the analysis, newest first, also read on their own.
  let reviews = $state<TaskReview[]>([]);
  let reviewsPhase = $state<"idle" | "loading" | "failed" | "ready">("idle");
  let decision = $state<ReviewDecision | null>(null);
  let note = $state("");
  let reviewing = $state(false);
  let reviewFailure = $state("");
  let reviewKey = "", reviewBody = "";
  let project = $state<Project | null>(null);
  // Figures drawn by R from result tables: this page's own request when it is a figure version, the
  // versions made from the same analysis, and the render service's charts. All three are extra context; the analysis
  // stands without them.
  let figure = $state<FigureVersion | null>(null);
  let figureExplore = $state(false);
  let figureVersions = $state<FigureVersionSummary[]>([]);
  let catalog = $state<FigureCatalog | null>(null);
  let editor = $state<{ sourceTask: string; table: { path: string; title: string; columns: string[]; numeric: boolean[] };
    initial: { feature: string; controls: FigureControls; title: string; previous: string | null } | null } | null>(null);
  let editorOpen = $state(false);
  // Reviewed R analyses: this page's own request when it is one, the analyses made from this one's results, the runs
  // of this page's analysis, and the catalog. All are extra context; the analysis stands without them.
  let analysis = $state<TemplateAnalysis | null>(null);
  let madeHere = $state<AnalysisVersionSummary[]>([]);
  let runs = $state<AnalysisVersionSummary[]>([]);
  let analysisCatalog = $state<AnalysisCatalog | null>(null);
  let analyzer = $state<{ sourceTask: string; table: { path: string; title: string; columns: string[]; numeric: boolean[] };
    initial: { feature: string; settings: AnalysisSettings; title: string; previous: string | null } | null; marked: string[] } | null>(null);
  let analyzerOpen = $state(false);
  // The live log of a running step, closed until the reader asks for it (Glass Box results rule 6).
  let watchingLog = $state(false);
  // The working copy's date, read once the project is known; absent until the daily sweep has seen it.
  let workingCopy = $state<WorkingCopy | undefined>(undefined);
  // Where the analysis sits, from recorded associations only: the project named by the
  // route, else the project whose task list contains it. `complete` says every project the
  // reader can open was checked, so "not in a project" is a finding, not a guess.
  let placement = $state<{ checked: boolean; complete: boolean }>({ checked: false, complete: false });
  // Evidence is read separately from the task, so a failed read cannot hide behind a
  // finished status and never changes the task record shown.
  let evidencePhase = $state<"idle" | "loading" | "failed" | "ready">("idle");
  let coverage = $state<"available" | "pending" | "unavailable" | "">("");
  let failure = $state("");
  let selectedFile = $state("");
  let cancelling = $state(false);
  let controller: AbortController | null = null;
  let stopWatch: (() => void) | null = null;

  const step = $derived(project?.checkpoint?.state.plan.find((s) => s.task_id === taskId));
  const title = $derived(step?.title ?? task?.objective ?? "Analysis");
  $effect(() => { if (task) pageTitle.value = title; });
  let highlight = $state<string[]>([]);
  let paper = $state<HTMLElement | undefined>(undefined);
  // Lets go of a figure or method entry the page is keeping in view while figures above it draw.
  let hold: (() => void) | null = null;
  async function showMethod(paths: string[]) {
    highlight = paths;
    await choose("how");
    await tick(); await tick();
    const target = document.querySelector<HTMLElement>("section[aria-labelledby='a-method'] li.change-bar") ?? document.getElementById("a-method");
    if (target && paper) { hold?.(); hold = holdInView(target, paper); focusOn(target); }
  }
  // A link inside the page hides the tab it sat in, so focus moves with the reader to what the link opened.
  function focusOn(target: HTMLElement) {
    if (!target.hasAttribute("tabindex")) target.tabIndex = -1;
    target.focus({ preventScroll: true });
  }
  // A figure version is known from the project's contents before its own record loads, so its status reads in the
  // editor's words ("Not drawn") from the first paint.
  const figureTask = $derived(!!figure || !!project?.tasks.find((t) => t.task_id === taskId)?.figure);
  const analysisTask = $derived(!!analysis || !!project?.tasks.find((t) => t.task_id === taskId)?.template_analysis);
  const status = $derived(task ? (figureTask ? versionStatus(task.status) : analysisTask ? analysisStatus(task.status, analysis?.receipts.at(-1))
    : taskStatus(task.status)) : null);
  const labName = $derived(identity.groups.find((g) => g.key === (project?.group_key ?? task?.group))?.name ?? project?.group_key ?? task?.group ?? "");
  const profileName = $derived(identity.profiles.find((p) => p.key === task?.profile)?.name ?? task?.profile ?? "");
  const files = $derived.by(() => {
    const out: { key: string; title: string; detail: string; artifact: Artifact; member?: import("./contracts.ts").PackageMember }[] = [];
    for (const artifact of evidence?.artifacts ?? []) {
      out.push({ key: artifact.id, title: artifact.label, detail: `${isPackage(artifact) ? "Analysis package" : fileRole(artifact.relative_path)} · ${bytes(artifact.size_bytes)}${artifact.relative_path.includes("/recovered/") ? " · recovered by the operator" : ""}`, artifact });
      const pkg = evidence?.packages.find((p) => p.artifact.id === artifact.id);
      for (const member of pkg?.contents.listing.members ?? [])
        out.push({ key: `${artifact.id}/${member.path}`, title: fileTitle(member.path), detail: `${fileRole(member.path, member.kind)} · ${bytes(member.size_bytes)}`, artifact, member });
    }
    return out;
  });
  // The analysis's sections are tabs under its head; a link inside the page opens the tab that holds its target.
  type Tab = "results" | "how" | "files" | "review" | "activity";
  let tab = $state<Tab>("results");
  const tabs = $derived<{ id: Tab; label: string; count?: number }[]>([
    { id: "results", label: "Results" },
    { id: "how", label: "How it was made" },
    { id: "files", label: "Files", count: files.length },
    { id: "review", label: "Review", count: reviews.length },
    { id: "activity", label: "Activity" },
  ]);
  async function choose(next: Tab, focus = false) {
    tab = next;
    await tick();
    if (focus) document.getElementById(`atab-${next}`)?.focus();
  }
  function tabKeys(event: KeyboardEvent) {
    const ids = tabs.map((t) => t.id), i = ids.indexOf(tab);
    const next = event.key === "ArrowRight" ? ids[(i + 1) % ids.length] : event.key === "ArrowLeft" ? ids[(i - 1 + ids.length) % ids.length]
      : event.key === "Home" ? ids[0] : event.key === "End" ? ids.at(-1) : undefined;
    if (!next) return;
    event.preventDefault();
    void choose(next, true);
  }
  let tablist = $state<HTMLElement>();
  let mark = $state({ x: 0, w: 0 });
  $effect(() => {
    void tab; void tabs.length;
    const list = tablist;
    if (!list) return;
    const place = () => { const el = list.querySelector<HTMLElement>(`#atab-${tab}`); if (el) mark = { x: el.offsetLeft, w: el.offsetWidth }; };
    place();
    const observer = new ResizeObserver(place);
    observer.observe(list);
    return () => observer.disconnect();
  });
  const shown = $derived(files.find((f) => f.key === selectedFile) ?? files[0]);
  // Each file in its result frame (Glass Box, results rule 1): a package member reads its pages and declared views
  // through the broker, and files of the same package that hold the same numbers are its other views.
  const results = $derived(new Map(files.filter((f) => f.member || !isPackage(f.artifact)).map((f) => [f.key,
    f.member ? memberResult(taskId, f.artifact, f.member, f.title, () => save(f)) : artifactResult(taskId, f.artifact, () => save(f))] as const)));
  const groupOf = (key: string): ResultFile[] => {
    const subject = results.get(key), file = files.find((f) => f.key === key);
    if (!subject || !file) return [];
    return sameValues(subject, files.filter((f) => f.artifact.id === file.artifact.id && f.key !== key).flatMap((f) => results.get(f.key) ?? []));
  };
  const stepNumber = $derived((project?.checkpoint?.state.plan.findIndex((s) => s.task_id === taskId) ?? -1) + 1);
  const madeWords = $derived(`${stepNumber ? `Step ${stepNumber}, ` : ""}${title}${profileName ? ` · ${profileName}` : ""}`);
  const quickItems = $derived<QuickItem[]>(files.map((f) => ({ key: f.key, title: f.title, made: madeWords, result: results.get(f.key) ?? null, group: groupOf(f.key),
    note: results.has(f.key) ? undefined : { text: `The complete retained analysis package (${bytes(f.artifact.size_bytes)}): ${packageHolds(evidence?.packages.find((p) => p.artifact.id === f.artifact.id)?.view)}. Extract it and follow its README to rerun the analysis on your computer.`,
      action: "Download the analysis package", run: () => save(f) } })));
  let quick = $state<number | null>(null);
  function quickLook(key: string) {
    const at = files.findIndex((f) => f.key === key);
    if (at >= 0) quick = at;
  }
  // Quick Look closes on the file the reader ended on; that file becomes the one shown and keeps the focus. The browser
  // returns focus to the file that opened the sheet as it closes, so the move to the last file waits a frame.
  async function quickClosed(key: string) {
    quick = null;
    selectedFile = key;
    await tick();
    requestAnimationFrame(() => document.querySelector<HTMLElement>(`.file-list button[data-key="${CSS.escape(key)}"]`)?.focus());
  }
  // Space opens the focused file in Quick Look; the up and down arrows move along the list.
  function listKeys(event: KeyboardEvent) {
    const button = (event.target as HTMLElement).closest<HTMLElement>("button[data-key]");
    if (!button) return;
    if (event.key === " ") { event.preventDefault(); quickLook(button.dataset.key!); return; }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const buttons = [...(button.closest("ul")?.querySelectorAll<HTMLElement>("button[data-key]") ?? [])];
    const next = buttons[buttons.indexOf(button) + (event.key === "ArrowDown" ? 1 : -1)];
    if (next) { event.preventDefault(); next.focus(); }
  }
  // Review is a project capability; an administrator's inspection only reads.
  const canReview = $derived(!!task && terminal(task.status) && !!project && project.capabilities.includes("review") && !project.inspection_expires_at);
  const acceptable = $derived((task?.status === "completed" || task?.status === "review_required") && (evidence?.artifacts.length ?? lineage?.outputs.length ?? 0) > 0);
  const newestMark = $derived(reviews[0] ? reviewMark(reviews[0], identity.principal.id) : null);
  const notePrompt: Record<ReviewDecision, string> = {
    accepted: "For example: Checked the code against the question and the summary table against the input.",
    needs_changes: "For example: Compare each test diet with the control, not with each other.",
    rejected: "For example: The input is the pilot file, not the final data set.",
  };

  async function load() {
    controller?.abort(); stopWatch?.(); stopWatch = null;
    const c = new AbortController();
    controller = c;
    failure = "";
    try {
      const [t, history] = await Promise.all([broker.task(taskId, c.signal), broker.events(taskId, 0, c.signal)]);
      if (c.signal.aborted) return;
      task = t; events = history.events;
      void broker.generatedCode(taskId, c.signal).then((code) => { if (!c.signal.aborted) generated = code; }).catch(() => {});
      void broker.taskQuery(taskId, c.signal).then((q) => { if (!c.signal.aborted) query = q; }).catch(() => {});
      void readFigures(c.signal);
      void broker.figureCatalog(c.signal).then((k) => { if (!c.signal.aborted) catalog = k; }).catch(() => {});
      void readAnalyses(c.signal);
      void analyses.catalog(c.signal).then((k) => { if (!c.signal.aborted) analysisCatalog = k; }).catch(() => {});
      void place(c.signal, readLineage(c.signal));
      void readReviews(c.signal);
      void broker.scientificHistory(taskId, null, c.signal).then((page) => { if (!c.signal.aborted) coverage = page.coverage; }).catch(() => { if (!c.signal.aborted) coverage = "unavailable"; });
      const attemptOf = `${t.status}:${t.attempt}`;
      if (!activeTask(t.status) && (evidencePhase !== "ready" || evidenceOf !== attemptOf)) void readEvidence(c.signal, attemptOf);
      if (!terminal(t.status)) {
        let window_ = history.events;
        stopWatch = watchTask(taskId, history.events.at(-1)?.sequence ?? 0, {
          event: (event) => { if (c.signal.aborted) return; window_ = mergeEvents(window_, [event]); events = window_.slice(-200); },
          connected: () => {}, disconnected: () => {},
          complete: () => { if (!c.signal.aborted) void load(); },
          unavailable: () => {},
        });
      }
    } catch (cause) {
      if (c.signal.aborted) return;
      failure = cause instanceof BrokerError && (cause.code === "record_unavailable" || cause.code === "access_required")
        ? "This analysis is not available to your account." : "The analysis could not be loaded. Its saved record is unchanged.";
    }
  }
  // The broker's own association names the analysis's project. A listing of projects is bounded
  // and can be stale, so it is never taken as proof that there is none; an unanswered read says so.
  async function place(signal: AbortSignal, read: Promise<Lineage | StepLineage | null>) {
    try {
      const id = projectId || (await read)?.task.project_id;
      if (signal.aborted) return;
      if (id === undefined) { placement = { checked: true, complete: false }; return; }
      if (id === null) { project = null; placement = { checked: true, complete: true }; return; }
      const p = await projects.get(id, signal);
      if (signal.aborted) return;
      projectIndex.remember(p);
      project = p; placement = { checked: true, complete: true };
      void projects.workingCopies(id, signal).then((all) => { if (!signal.aborted) workingCopy = all.find((w) => w.task_id === taskId); }).catch(() => {});
    } catch { if (!signal.aborted) placement = { checked: true, complete: false }; }
  }
  // The files are read once per finished attempt; a later read of the page keeps them in view while it runs.
  let evidenceOf = "";
  async function readEvidence(signal: AbortSignal, of = evidenceOf) {
    if (evidencePhase !== "ready") evidencePhase = "loading";
    try {
      const loaded = await loadTaskEvidence(taskId, signal);
      if (signal.aborted) return;
      evidence = loaded; evidencePhase = "ready"; evidenceOf = of;
    } catch { if (!signal.aborted && evidencePhase !== "ready") evidencePhase = "failed"; }
  }
  function retryEvidence() { if (controller) void readEvidence(controller.signal); }
  async function readFigures(signal: AbortSignal) {
    try {
      const own = await broker.taskFigure(taskId, signal);
      if (signal.aborted) return;
      figure = own;
      if (!own) figureExplore = false;
      const versions = await broker.figureVersions(own?.source_task ?? taskId, signal);
      if (!signal.aborted) figureVersions = versions;
    } catch { /* A broker without figure versions answers nothing here; the page stands without them. */ }
  }
  // Figures are offered in a project the reader can add to, in a lab whose figure method is enabled.
  const canFigure = $derived(!!project && !project.inspection_expires_at && project.capabilities.includes("contribute")
    && !!catalog?.groups.includes(project.group_key));
  const figures = $derived(figureGroups(figureVersions));
  const ownVersions = $derived(figure ? figures.find((g) => g.some((v) => v.task_id === taskId)) ?? [] : []);
  // A figure version names the exact table and source package in its render request. The companion
  // opens only that member, after both the package and member digests agree with the record.
  async function loadFigureInput(record: FigureVersion, signal: AbortSignal) {
    const sameDigest = (a: string, b: string) => a.replace(/^sha256:/, "") === b.replace(/^sha256:/, "");
    const artifacts = (await broker.artifacts(record.source_task, signal)).artifacts;
    const artifact = artifacts.find((candidate) => isPackage(candidate) && sameDigest(candidate.sha256, record.source_package_sha256));
    if (!artifact) throw new Error("source_package_unavailable");
    const contents = await packageContents(record.source_task, artifact, signal);
    if (!sameDigest(contents.sha256, artifact.sha256)) throw new Error("source_package_changed");
    const member = contents.listing.members.find((candidate) => candidate.path === record.table_path);
    if (!member || !sameDigest(member.sha256, record.table_sha256)) throw new Error("source_table_changed");
    if (member.size_bytes > MAX_FIGURE_DATA_BYTES) throw new Error("source_table_too_large");
    const blob = await verifiedMember(record.source_task, artifact, member, signal);
    return { file: { path: member.path, media_type: member.media_type, size_bytes: member.size_bytes, sha256: member.sha256 }, blob };
  }
  const companionFor = (record: FigureVersion): FigureDataSource => ({
    record,
    figure_paths: record.receipts.filter((receipt) => receipt.status === "completed").flatMap((receipt) => receipt.files?.map((file) => file.path) ?? []),
    figure_package_sha256: record.receipts.findLast((receipt) => receipt.status === "completed")?.package_sha256 ?? null,
    load: (signal) => loadFigureInput(record, signal),
  });
  const figureCompanion = $derived(figure ? companionFor(figure) : null);
  function makeFigure(member: PackageMember, read: { columns: string[]; numeric: boolean[] }) {
    editor = { sourceTask: taskId, table: { path: member.path, title: fileTitle(member.path), ...read }, initial: null };
    editorOpen = true;
  }
  function editFigure() {
    if (!figure) return;
    editor = { sourceTask: figure.source_task,
      table: { path: figure.table_path, title: fileTitle(figure.table_path), columns: figure.table_columns, numeric: figure.table_columns.map(() => false) },
      initial: { feature: figure.feature, controls: figure.controls, title: figure.title, previous: taskId } };
    editorOpen = true;
  }
  // The manuscript mark belongs to the figure, which the broker names by its first version.
  async function markFigure(version: string | null) {
    if (!project || !ownVersions.length) return;
    const root = figureRoot(ownVersions);
    try {
      if (version) await broker.markManuscript(project.project_id, root, version);
      else await broker.clearManuscript(project.project_id, root);
      notify(version ? "This version is marked as the one in the manuscript." : "The manuscript mark is cleared.");
    } catch (cause) {
      notify(cause instanceof BrokerError && cause.reason === "version_not_drawn" ? "Only a drawn version can be marked." : "The mark could not be changed just now.", true);
    }
    if (controller) await readFigures(controller.signal);
  }
  async function readAnalyses(signal: AbortSignal) {
    try {
      const own = await analyses.detail(taskId, signal).catch(() => null);
      if (signal.aborted) return;
      analysis = own;
      const [made, siblings] = await Promise.all([analyses.versions(taskId, signal).catch(() => []),
        own ? analyses.versions(own.source_task, signal).catch(() => []) : Promise.resolve([])]);
      if (signal.aborted) return;
      madeHere = made;
      runs = own ? figureGroups(siblings).find((g) => g.some((v) => v.task_id === taskId)) ?? [] : [];
    } catch { /* A broker without template analyses answers nothing here; the page stands without them. */ }
  }
  // A reviewed R analysis is offered in a project the reader can add to, in a lab whose template-analysis method is
  // enabled, on a finished analysis's tables.
  const canAnalyze = $derived(!!project && !project.inspection_expires_at && project.capabilities.includes("contribute")
    && !!analysisCatalog?.groups.includes(project.group_key));
  // A template analysis's results table is named by the analysis, so an analysis of it reads "Summary statistics of
  // Tolerance test areas of ...", not "of Results".
  const tableName = (path: string) => analysisPaths.some((f) => f.path === path && f.role === "table") ? title : fileTitle(path);
  function analyzeTable(member: PackageMember, read: { columns: string[]; numeric: boolean[] }) {
    analyzer = { sourceTask: taskId, table: { path: member.path, title: tableName(member.path), ...read }, initial: null, marked: [] };
    analyzerOpen = true;
  }
  // The settings of this page's analysis, for its next run; `marked` are the settings a refusal named. The source
  // table's column types come from the broker's summary of it, so the list says which analyses fit.
  async function changeAnalysis(marked: string[]) {
    if (!analysis) return;
    const own = analysis;
    analyzer = { sourceTask: own.source_task,
      table: { path: own.table_path, title: fileTitle(own.table_path), columns: own.table_columns, numeric: await sourceNumeric(own) },
      initial: { feature: own.feature, settings: own.settings, title: own.title, previous: taskId }, marked };
    analyzerOpen = true;
  }
  // Which of the source table's columns hold numbers, from the broker's column summary; when it cannot be read, every
  // column counts as a number so no analysis is wrongly listed as not fitting.
  async function sourceNumeric(own: TemplateAnalysis): Promise<boolean[]> {
    try {
      const pkg = (await broker.artifacts(own.source_task)).artifacts.find(isPackage);
      const summary = pkg ? await broker.summary(own.source_task, pkg.id, own.table_path) : null;
      const types = new Map(summary?.columns.map((c) => [c.name, c.type]) ?? []);
      if (types.size) return own.table_columns.map((c) => types.get(c) === "number" || types.get(c) === "integer");
    } catch { /* The list falls back to every column. */ }
    return own.table_columns.map(() => true);
  }
  // A step a statement offers: the comparison a tolerance test's table is made for, on this analysis's own results
  // table, or the per-animal summary a refusal asks for, on the table this analysis read.
  async function analyzeNext(step: { table: string; feature: string; settings: AnalysisSettings; title: string }, from: "results" | "source") {
    if (from === "source") {
      if (!analysis) return;
      const own = analysis;
      analyzer = { sourceTask: own.source_task,
        table: { path: own.table_path, title: fileTitle(own.table_path), columns: own.table_columns, numeric: await sourceNumeric(own) },
        initial: { feature: step.feature, settings: step.settings, title: step.title, previous: null }, marked: [] };
      analyzerOpen = true;
      return;
    }
    const member = evidence?.packages.flatMap((p) => p.contents.listing.members).find((m) => m.path === step.table);
    const columns = analysis?.receipts.at(-1)?.files?.some((f) => f.path === step.table) && member ? await tableColumns(member) : null;
    if (!columns) { notify("The results table could not be read just now. Open it under Files and use Analyze there.", true); return; }
    analyzer = { sourceTask: taskId, table: { path: step.table, title: tableName(step.table), ...columns },
      initial: { feature: step.feature, settings: step.settings, title: step.title, previous: null }, marked: [] };
    analyzerOpen = true;
  }
  // A results table's columns and which hold numbers, read from its checked bytes as the table view reads them.
  async function tableColumns(member: PackageMember): Promise<{ columns: string[]; numeric: boolean[] } | null> {
    const pkg = evidence?.packages.find((p) => p.contents.listing.members.includes(member));
    if (!pkg) return null;
    try {
      const read = tableRead(await strictText(await verifiedMember(taskId, pkg.artifact, member), MAX_TABLE_BYTES), ",");
      const number = /^-?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/;
      return { columns: read.columns, numeric: read.columns.map((_, c) => read.rows.slice(0, 200).every((r) => r[c] === "" || r[c] === "NA" || number.test(r[c]))) };
    } catch { return null; }
  }
  const analysisPaths = $derived(analysis?.receipts.at(-1)?.files ?? []);
  // The analyses made from these results, one row for each analysis with its newest run first.
  const madeAnalyses = $derived(figureGroups(madeHere));
  // A result table of this analysis opens in the figure editor from its views, as the 3D view's
  // "Draw this angle in R" does. The broker draws figures from a finished analysis's results tables only.
  const figureFor = $derived.by(() => {
    if (!canFigure || figure || task?.status !== "completed" || !catalog) return null;
    const charts = catalog.features.map((f) => f.id);
    return (file: ResultFile): FigureHandle | null => /^results\/[^/]+\.(csv|tsv)$/.test(file.file.path) ? {
      charts,
      draw: (spec) => {
        editor = { sourceTask: taskId, table: { path: file.file.path, title: fileTitle(file.file.path), columns: spec.columns, numeric: spec.numeric },
          initial: { feature: spec.feature, controls: spec.controls, title: fileTitle(file.file.path), previous: null } };
        editorOpen = true;
      },
    } : null;
  });
  // The groups of a column of the editor's table, read from its checked bytes, for colours per group.
  async function groupsOf(column: string, signal: AbortSignal): Promise<{ values: string[]; more: boolean }> {
    const current = editor;
    if (!current) throw new Error("no_table");
    const source = current.sourceTask === taskId && evidence ? evidence : await loadTaskEvidence(current.sourceTask, signal);
    for (const pkg of source.packages) {
      const member = pkg.contents.listing.members.find((m) => m.path === current.table.path);
      if (!member) continue;
      const text = await strictText(await verifiedMember(current.sourceTask, pkg.artifact, member, signal), MAX_TABLE_BYTES);
      return groupsIn(text, column, current.table.path.toLowerCase().endsWith(".tsv") ? "\t" : ",");
    }
    throw new Error("table_not_found");
  }
  const versionLink = (task: string) => href(project ? { name: "analysis", project: project.project_id, task } : { name: "analysis", task });
  async function readLineage(signal: AbortSignal): Promise<StepLineage | null> {
    if (!lineage) lineagePhase = "loading";
    try {
      const read = await broker.stepLineage(taskId, signal);
      if (signal.aborted) return null;
      lineage = read; lineagePhase = "ready";
      void readCitations(read, signal);
      return read;
    } catch { if (!signal.aborted && !lineage) lineagePhase = "failed"; return null; }
  }
  async function readCitations(read: Lineage | StepLineage, signal: AbortSignal) {
    const { origin, run_id: run, turn } = read.origin;
    if (origin !== "research_run" || !run || !turn) return;
    try {
      const turns = (await projects.turns(run, signal)).turns;
      if (!signal.aborted) stepChecks = checksFor(turns, taskId);
      const decision = turns.find((t) => t.turn === turn);
      if (!decision?.record.proposal.actions.some((a) => a.literature_refs?.length)) return;
      const lab = project?.group_key ?? task?.group;
      const list = lab ? await library.list(lab, signal).catch(() => null) : null;
      if (!signal.aborted) cited = papersCited(decision, taskId, list?.papers ?? null);
    } catch { /* The citation is extra context; the lineage above stands without it. */ }
  }
  async function readReviews(signal: AbortSignal) {
    if (reviewsPhase !== "ready") reviewsPhase = "loading";
    try {
      const read = await broker.reviews(taskId, signal);
      if (signal.aborted) return;
      reviews = read; reviewsPhase = "ready";
    } catch { if (!signal.aborted && reviewsPhase !== "ready") reviewsPhase = "failed"; }
  }
  function retryReviews() { if (controller) void readReviews(controller.signal); }
  // The same decision and note keep one request key, so a repeated send saves one review.
  async function sendReview() {
    if (!task || reviewing) return;
    const problem = reviewProblem(decision, note, acceptable);
    if (problem || !decision) { reviewFailure = problem; return; }
    const clean = note.trim();
    const body = JSON.stringify({ decision, clean });
    if (body !== reviewBody) { reviewBody = body; reviewKey = crypto.randomUUID(); }
    reviewing = true; reviewFailure = "";
    try {
      const saved = await broker.review(task.id, { decision, note: clean || null, request_id: reviewKey });
      reviews = [saved, ...reviews.filter((r) => r.review_id !== saved.review_id)];
      reviewsPhase = "ready"; decision = null; note = ""; reviewBody = "";
      notify("Your review is saved with this analysis.");
    } catch (cause) {
      reviewFailure = cause instanceof BrokerError && cause.code === "access_required" ? "Your account can no longer review this project. Your note is still here."
        : cause instanceof BrokerError && cause.code === "request_conflict" ? "The analysis changed while you were reviewing it. Your note is still here; reload the page, check the results again and send it."
        : "The review was not saved. Your note is still here.";
    } finally { reviewing = false; }
  }
  function retryLineage() {
    if (!controller) return;
    const read = readLineage(controller.signal);
    if (!project && !placement.complete) void place(controller.signal, read);
  }
  async function showSection(id: "a-code" | "a-query" | "a-files" | "a-review") {
    await choose(id === "a-files" ? "files" : id === "a-review" ? "review" : "how");
    const target = document.getElementById(id);
    if (target && paper) { hold?.(); hold = holdInView(target, paper); focusOn(target); }
  }

  // A figure chosen on the project's contents page: its drawn plate stands in until the
  // verified figures are ready, then the page moves to that figure and marks it.
  const preview = $derived(chosenFigure.task === taskId ? chosenFigure.preview : "");
  $effect(() => {
    if (evidencePhase !== "ready" || chosenFigure.task !== taskId) return;
    const path = chosenFigure.path;
    untrack(() => { chosenFigure.task = ""; chosenFigure.path = ""; chosenFigure.preview = ""; });
    void tick().then(() => {
      const target = [...document.querySelectorAll<HTMLElement>("figure.figure[data-path]")].find((el) => el.dataset.path === path);
      if (!target) return;
      target.classList.add("change-bar");
      if (paper) { hold?.(); hold = holdInView(target, paper, "center"); }
    });
  });
  let unwatch: (() => void) | null = null;
  $effect(() => { void taskId; untrack(() => { hold?.(); hold = null; task = null; evidence = null; evidencePhase = "idle"; generated = null; lineage = null; lineagePhase = "idle"; cited = [];
    reviews = []; reviewsPhase = "idle"; decision = null; note = ""; reviewFailure = ""; reviewBody = ""; project = null; placement = { checked: false, complete: false }; coverage = ""; selectedFile = "";
    figure = null; figureExplore = false; figureVersions = []; editor = null; editorOpen = false; analysis = null; madeHere = []; runs = []; analyzer = null; analyzerOpen = false;
    watchingLog = false; tab = "results"; evidenceOf = ""; void load();
    // Every change to this analysis reads it again (live.ts): a review, a figure or analysis made from it, its status.
    unwatch?.(); unwatch = live().watch([`task:${taskId}`], () => load(), { gapMs: 1_500 }); }); });
  const templatePackage = $derived(evidence?.packages.find((p) => p.view.template) ?? null);
  onDestroy(() => { controller?.abort(); stopWatch?.(); hold?.(); unwatch?.(); });

  // The saved output file the live log names, read from the analysis's own list so it is checked like any file.
  async function saveOutput(artifact: string) {
    try {
      const found = (await broker.artifacts(taskId)).artifacts.find((a) => a.id === artifact);
      if (found) await save({ artifact: found });
      else notify("The saved output is not listed yet. Try again in a moment.", true);
    } catch { notify("The saved output could not be read just now.", true); }
  }
  async function save(file: { artifact: Artifact; member?: import("./contracts.ts").PackageMember }) {
    try {
      const blob = file.member ? await verifiedMember(taskId, file.artifact, file.member) : await verifiedArtifact(taskId, file.artifact);
      saveBlob(blob, file.member ? saveName(file.member.path) : saveName(file.artifact.relative_path) + (isPackage(file.artifact) && !/\.[a-z0-9]+$/i.test(file.artifact.relative_path) ? ".tar.gz" : ""));
      notify("Downloaded after checking its recorded checksum.");
    } catch (cause) {
      notify(cause instanceof BrokerError && cause.code === "use_verified_cli" ? "This file is larger than the browser can check. Use the verified command-line download." : "The file did not match its recorded checksum or is unavailable. Nothing was saved.", true);
    }
  }
  async function cancel() {
    if (!task || terminal(task.status) || cancelling) return;
    cancelling = true;
    try { const result = await broker.cancel(task.id); task = { ...task, status: result.status }; notify("Stop requested. Files already saved are kept."); }
    catch { notify("The analysis could not be stopped. Reload and try again.", true); }
    finally { cancelling = false; }
  }
</script>

{#if failure && !task}
  <div class="page-state"><h1>Analysis unavailable</h1><p>{failure}</p><a class="link" href="#/">Back to your projects</a></div>
{:else if !task}
  {#if preview}
    <div class="page"><figure class="chosen" style="view-transition-name: chosen-figure"><img src={preview} alt="" /><figcaption class="soft small" role="status">Opening this figure and the rest of the analysis…</figcaption></figure></div>
  {:else}
    <div class="page-state" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Opening the analysis…</div>
  {/if}
{:else}
  <article class="page" bind:this={paper}>
    <header class="head">
      <p class="running">
        <span class="where">{labName}</span><span class="sep" aria-hidden="true">/</span>
        {#if project}<button class="link" onclick={() => openProject(project!.project_id)}>{project.title}</button>
        {:else if placement.checked && placement.complete}<span>Not part of a project</span>
        {:else if placement.checked}<span>Its project could not be looked up just now</span>
        {:else}<span class="faint">Finding its project…</span>{/if}
        {#if project || placement.checked}
          <span class="sep" aria-hidden="true">·</span>
          <span class="who-can">{project ? sharing(project.visibility, labName)
            : placement.complete ? "Only the person who requested it can open it" : "Who can open it could not be looked up just now"}</span>
        {/if}
      </p>
      <h1>{title}</h1>
      {#if step && task.objective && task.objective !== title}<p class="abstract"><span class="lead">Request.</span> {task.objective}</p>{/if}
      <div class="now">
        {#if status}<StatusMark tone={status.tone} label={status.label} />{/if}
        {#if newestMark}<StatusMark tone={newestMark.tone} label={newestMark.label} wrap />{/if}
        <span class="soft small">{profileName} · requested {when(task.submitted_at)}{#if task.finished_at}{" · finished "}{when(task.finished_at)}{/if}</span>
        {#if !terminal(task.status)}<button class="button secondary" onclick={cancel} disabled={cancelling || task.status === "cancelling"}>{task.status === "cancelling" ? "Stopping…" : "Stop this analysis"}</button>{/if}
      </div>
    </header>

    <div class="tabbar">
      <div role="tablist" aria-label="Sections of this analysis" bind:this={tablist}>
        {#each tabs as t (t.id)}
          <button role="tab" id={`atab-${t.id}`} aria-selected={tab === t.id} aria-controls={`apanel-${t.id}`} tabindex={tab === t.id ? 0 : -1}
            onclick={() => choose(t.id)} onkeydown={tabKeys}>{t.label}{#if t.count}<span class="count">{t.count}</span>{/if}</button>
        {/each}
        <span class="tab-mark" aria-hidden="true" style:transform={`translateX(${mark.x}px)`} style:width={`${mark.w}px`}></span>
      </div>
    </div>

    <div id="apanel-results" class="tab-panel" role="tabpanel" aria-labelledby="atab-results" hidden={tab !== "results"}>
      {#if analysis}
        <AnalysisStatement {taskId} {analysis} catalog={analysisCatalog} status={task.status} {evidence} canChange={canAnalyze && terminal(task.status)}
          onChange={changeAnalysis} onNext={analyzeNext} />
      {/if}
      {#if evidencePhase === "failed"}
        <section class="block" aria-labelledby="a-results"><h2 id="a-results" class="panel-title visually-hidden">Results</h2>
          <div class="load-failed change-bar" role="alert">
            <p>This analysis is recorded as {status?.label.toLowerCase() ?? "done"}, but its files could not be read just now. Nothing about the analysis changed.</p>
            <button class="button secondary" onclick={retryEvidence}>Try again</button>
          </div>
        </section>
      {:else if evidence?.packages.length}
        <StepCheck checks={stepChecks} />
        <section class="block" aria-labelledby="a-results"><h2 id="a-results" class="panel-title visually-hidden">Results</h2>
          {#each evidence.packages as pkg (pkg.artifact.id)}<AnalysisResults {taskId} {pkg} onError={(m) => notify(m, true)} onShowMethod={showMethod}
            writer={figure ? "render" : analysis ? "template" : generated ? (generated.configuration ? "configured" : "model") : null}
            figureTitle={figure || analysis ? title : ""} onFigure={canFigure && !figure && task.status === "completed" ? makeFigure : undefined}
            onAnalyze={canAnalyze && !figure && task.status === "completed" ? analyzeTable : undefined}
            leaveOut={analysisPaths.filter((f) => f.role === "statistics").map((f) => f.path)}
            tableTitles={Object.fromEntries(analysisPaths.filter((f) => f.role === "table").map((f) => [f.path, "Results"]))}
            companion={figureCompanion} exploreOpen={figureCompanion ? figureExplore : undefined}
            onExploreChange={figureCompanion ? (open) => (figureExplore = open) : undefined} />{/each}
          {#if !figure && figures.length}
            <div class="made">
              <h3>Figures made from these results</h3>
              <ol>
                {#each figures as versions (versions[0].task_id)}
                  {@const v = shownVersion(versions)}
                  <li><a class="t" href={versionLink(v.task_id)}>{v.title || `${chartName(catalog, v.feature)} of ${fileTitle(v.table_path)}`}</a>
                    <span class="d">{chartName(catalog, v.feature)} of {fileTitle(v.table_path)} · {versions.length === 1 ? "one version" : `${versions.length} versions`} · {when(versions[0].created_at)}{v.manuscript?.version === v.task_id ? " · the version in the manuscript" : ""}</span></li>
                {/each}
              </ol>
            </div>
          {/if}
          {#if madeAnalyses.length}
            <div class="made">
              <h3>Analyses made from these results</h3>
              <ol>
                {#each madeAnalyses as group (group[0].task_id)}
                  {@const v = group[0]}
                  <li><a class="t" href={versionLink(v.task_id)}>{v.title || `${analysisName(analysisCatalog, v.feature)} of ${fileTitle(v.table_path)}`}</a>
                    <span class="d">{analysisName(analysisCatalog, v.feature)} of {fileTitle(v.table_path)} · {group.length === 1 ? "one run" : `${group.length} runs`} · {when(v.created_at)} · {analysisStatus(v.status).label}</span></li>
                {/each}
              </ol>
            </div>
          {/if}
        </section>
      {:else if preview && evidencePhase !== "ready"}
        <section class="block" aria-labelledby="a-results"><h2 id="a-results" class="panel-title visually-hidden">Results</h2>
          <figure class="chosen" style="view-transition-name: chosen-figure"><img src={preview} alt="" /><figcaption class="soft small" role="status">Checking this figure against its recorded checksum…</figcaption></figure>
        </section>
      {:else if evidencePhase === "loading"}
        <p class="soft" role="status">Opening the retained files…</p>
      {:else if activeTask(task.status)}
        <p class="soft">This analysis is still running. Its results appear here when it finishes.</p>
      {/if}
      <!-- The log starts closed (Glass Box results rule 6) and stays open after the step ends, so its check can be read. -->
      {#if task.status === "running" || watchingLog}
        <section class="block watch-log" aria-label="Live output">
          {#if watchingLog}
            <LiveLog task={taskId} running={activeTask(task.status)} system={events.map((e) => ({ at: e.occurred_at, text: eventLabel(e.code) }))} onSave={saveOutput} />
            <button class="link small" onclick={() => (watchingLog = false)}>Close the log</button>
          {:else}
            <button class="button secondary" onclick={() => (watchingLog = true)}>Watch the log</button>
          {/if}
        </section>
      {/if}

      {#if analysis}
        <section class="block" aria-labelledby="a-analysis"><h2 id="a-analysis" class="panel-title">Analysis settings</h2>
          <AnalysisRecord {analysis} catalog={analysisCatalog} status={task.status} versions={runs} current={taskId} project={project?.project_id}
            sourceTitle={project?.checkpoint?.state.plan.find((s) => s.task_id === analysis?.source_task)?.title}
            canChange={canAnalyze && terminal(task.status)} onChange={() => changeAnalysis([])} />
        </section>
      {/if}
      {#if figure}
        <section class="block" aria-labelledby="a-figure"><h2 id="a-figure" class="panel-title">Figure settings</h2>
          <FigureRecord {figure} {catalog} status={task.status} versions={ownVersions} current={taskId} project={project?.project_id}
            sourceTitle={project?.checkpoint?.state.plan.find((s) => s.task_id === figure?.source_task)?.title}
            canEdit={canFigure && terminal(task.status)} onEdit={editFigure} reader={identity.principal.id}
            canMark={!!project && !project.inspection_expires_at && project.capabilities.includes("steer")} onMark={markFigure}
            companionAvailable={!!figureCompanion} companionOpen={figureExplore} onCompanionOpen={(open) => {
              figureExplore = open;
              if (open) choose("results");
            }} />
        </section>
      {/if}
    </div>

    <div id="apanel-how" class="tab-panel" role="tabpanel" aria-labelledby="atab-how" hidden={tab !== "how"}>
      <section class="block" aria-labelledby="a-lineage"><h2 id="a-lineage" class="panel-title">Where this came from</h2>
        {#if lineage}
          <LineageRecord {lineage} reader={identity.principal.id} {reviews} papers={cited} onShow={showSection}
            reviewLink={reviews.length ? (reviews.length === 1 ? "See the review" : "See the reviews") : canReview ? "Review this analysis" : ""} />
        {:else if lineagePhase === "failed"}
          <div class="load-failed change-bar" role="alert">
            <p>Where this analysis came from could not be read just now. Nothing about the analysis changed.</p>
            <button class="button secondary" onclick={retryLineage}>Try again</button>
          </div>
        {:else}
          <p class="soft" role="status">Reading where this analysis came from…</p>
        {/if}
      </section>
      {#if query}
        <section class="block" aria-labelledby="a-query"><h2 id="a-query" class="panel-title">The database query</h2>
          <QueryRecord {query}
            onDownload={() => { saveBlob(new Blob([`${query!.admitted_sql}\n`], { type: "text/plain" }), "query.sql"); notify("Downloaded the query."); }} />
        </section>
      {/if}

      {#if generated}
        <section class="block" aria-labelledby="a-code"><h2 id="a-code" class="panel-title">{generated.configuration ? "The recipe and its settings" : "Model-written code"}</h2>
          <GeneratedCode {taskId} code={generated} pkg={templatePackage}
            onDownload={() => { saveBlob(new Blob([generated!.content], { type: "text/plain" }), "block.R"); notify(generated!.configuration ? "Downloaded the configured recipe." : "Downloaded the model-written block."); }} />
        </section>
      {/if}

      {#if evidence?.packages.length}
        <section class="block" aria-labelledby="a-method"><h2 id="a-method" class="panel-title">How this was done</h2>
          {#each evidence.packages as pkg (pkg.artifact.id)}<MethodRecord {taskId} {pkg} onError={(m) => notify(m, true)} onRecordPage {highlight} />{/each}
        </section>
      {/if}
    </div>

    {#if quick !== null}<QuickLook items={quickItems} bind:index={quick} onclose={quickClosed} figure={figureFor} />{/if}
    <div id="apanel-files" class="tab-panel" role="tabpanel" aria-labelledby="atab-files" hidden={tab !== "files"}>
      {#if files.length}
        <section class="block" aria-labelledby="a-files"><h2 id="a-files" class="panel-title">All files</h2>
          <p class="small soft">Every retained file, including each file inside the analysis package. Downloads are checked against their recorded checksums first. Press Space on a file to open it larger.</p>
          {#if workingCopyLine(workingCopy)}<p class="small soft working-copy">{workingCopyLine(workingCopy)}</p>{/if}
          <div class="files">
            <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
            <ul class="file-list" aria-label="Retained files" onkeydown={listKeys}>
              {#each files as file (file.key)}
                <li><button data-key={file.key} class:current={shown?.key === file.key} class:member={!!file.member} onclick={() => (selectedFile = file.key)} aria-current={shown?.key === file.key}>
                  <span class="t">{file.title}</span><span class="d">{file.detail}</span></button></li>
              {/each}
            </ul>
            <div class="file-reader">
              {#if shown}
                {#if shown.member}
                  {@const member = shown.member}
                  <FileView file={fromMember(member, shown.title)} load={(signal) => verifiedMember(taskId, shown.artifact, member, signal)} title={shown.title} onDownload={() => save(shown)}
                    result={results.get(shown.key) ?? null} group={groupOf(shown.key)} onExpand={() => quickLook(shown.key)} figure={figureFor} />
                {:else if isPackage(shown.artifact)}
                  <div class="package-note"><strong>{shown.artifact.label}</strong>
                    <p class="small">The complete retained analysis package ({bytes(shown.artifact.size_bytes)}): {packageHolds(evidence?.packages.find((p) => p.artifact.id === shown.artifact.id)?.view)}. Extract it and follow its README to rerun the analysis on your computer.</p>
                    <button class="button" onclick={() => save(shown)}>Download the analysis package</button></div>
                {:else}
                  <FileView file={fromArtifact(shown.artifact)} load={(signal) => verifiedArtifact(taskId, shown.artifact, signal)} title={shown.title} onDownload={() => save(shown)}
                    result={results.get(shown.key) ?? null} onExpand={() => quickLook(shown.key)} />
                {/if}
              {/if}
            </div>
          </div>
        </section>
      {:else}
        <section class="block" aria-labelledby="a-files"><h2 id="a-files" class="panel-title">All files</h2>
          {#if evidencePhase === "failed"}
            <div class="load-failed change-bar" role="alert">
              <p>The files of this analysis could not be read just now. Nothing about the analysis changed.</p>
              <button class="button secondary" onclick={retryEvidence}>Try again</button>
            </div>
          {:else if evidencePhase === "ready"}<p class="soft">No files were retained for this analysis.</p>
          {:else if activeTask(task.status)}<p class="soft">Its files appear here when the analysis finishes.</p>
          {:else}<p class="soft" role="status">Opening the retained files…</p>{/if}
        </section>
      {/if}

      {#if coverage === "available" || coverage === "pending"}
        <section class="block" aria-labelledby="a-steps"><h2 id="a-steps" class="panel-title">Recorded steps</h2>
          <RecordedSteps {task} artifacts={evidence?.artifacts ?? []} onDownload={(a) => save({ artifact: a })} />
        </section>
      {/if}
    </div>

    <div id="apanel-review" class="tab-panel" role="tabpanel" aria-labelledby="atab-review" hidden={tab !== "review"}>
      <section class="block" aria-labelledby="a-review"><h2 id="a-review" class="panel-title visually-hidden">Review</h2>
        <div class="review">
          <p class="claim">{reviewClaim}</p>
          {#if reviewsPhase === "failed"}
            <div class="load-failed change-bar" role="alert">
              <p>The reviews of this analysis could not be read just now. Nothing about the analysis changed.</p>
              <button class="button secondary" onclick={retryReviews}>Try again</button>
            </div>
          {:else if reviews.length}
            <ol class="reviews" aria-label="Reviews of this analysis, newest first">
              {#each reviews as review, i (review.review_id)}
                {@const mark = reviewMark(review, identity.principal.id)}
                <li class:earlier={i > 0}>
                  <p class="who"><StatusMark tone={mark.tone} label={mark.label} wrap /><time datetime={review.created_at}>{when(review.created_at)}</time></p>
                  {#if review.note}<p class="note">{review.note}</p>{/if}
                  <p class="small soft">{coverageSentence(review)}</p>
                  {#if !byPerson(review)}<p class="small soft">{notAPerson}</p>{/if}
                </li>
              {/each}
            </ol>
          {:else if reviewsPhase === "ready"}
            <p class="soft">{!project && placement.complete ? "Only an analysis in a project can be reviewed, and this one is not part of a project."
              : "No one has reviewed this analysis yet."}</p>
          {:else}
            <p class="soft" role="status">Reading the reviews…</p>
          {/if}

          {#if canReview && reviewsPhase !== "failed"}
            <form class="review-form" onsubmit={(e) => { e.preventDefault(); void sendReview(); }}>
              <fieldset>
                <legend>{reviews.length ? "Record a new review" : "Your review"}</legend>
                {#each decisions as key (key)}
                  {@const off = key === "accepted" && !acceptable}
                  <label class="decision" class:off>
                    <input type="radio" name="review-decision" value={key} bind:group={decision} disabled={off} />
                    <span><strong>{decisionWords[key].choice}</strong> <span class="hint">{off ? "Only an analysis that finished with result files can be accepted." : decisionWords[key].hint}</span></span>
                  </label>
                {/each}
              </fieldset>
              <label for="review-note">{decision && decision !== "accepted" ? "Note" : "Note, optional when you accept"}</label>
              <textarea id="review-note" bind:value={note} rows="3" maxlength={MAX_REVIEW_NOTE} placeholder={notePrompt[decision ?? "accepted"]}></textarea>
              <div class="review-row">
                <p class="small soft">Saved with your name and the time, and tied to the result files by their checksums. A review cannot be edited; a later one is added above it.</p>
                <button class="button" type="submit" disabled={reviewing || !decision}>{reviewing ? "Saving…" : "Record your review"}</button>
              </div>
              {#if reviewFailure}<p class="notice error" role="alert">{reviewFailure}</p>{/if}
            </form>
          {:else if project && !project.inspection_expires_at && project.capabilities.includes("review") && !terminal(task.status)}
            <p class="small soft">You can review this analysis when it finishes.</p>
          {:else if project && !project.capabilities.includes("review") && reviewsPhase === "ready"}
            <p class="small soft">People with review access to this project can record a review here.</p>
          {/if}
        </div>
      </section>
    </div>

    <div id="apanel-activity" class="tab-panel" role="tabpanel" aria-labelledby="atab-activity" hidden={tab !== "activity"}>
      <section class="block" aria-labelledby="a-activity"><h2 id="a-activity" class="panel-title visually-hidden">Activity</h2>
        <ol class="activity">
          {#each events as event (event.sequence)}
            <li><time datetime={event.occurred_at}>{when(event.occurred_at)}</time><span>{eventLabel(event.code)}</span></li>
          {/each}
        </ol>
        <p class="small soft">Service activity shows how the work moved through the cluster. It is not scientific evidence.</p>
      </section>
    </div>

    <details class="technical">
      <summary>Technical details</summary>
      <dl>
        <dt>Analysis ID</dt><dd>{task.id}</dd><dt>Method profile</dt><dd>{task.profile}</dd><dt>Research group</dt><dd>{task.group}</dd>
        <dt>Attempt</dt><dd>{task.attempt} of {task.max_task_attempts}</dd><dt>Result class</dt><dd>{task.result_class ?? "none"}</dd>
        <dt>Error code</dt><dd>{task.error_code ?? "none"}</dd><dt>Worker</dt><dd>{task.logical_agent ?? "not assigned"}</dd>
        {#each events as event (event.sequence)}<dt>Event {event.sequence}</dt><dd>{event.kind} · {event.state} · {event.code}</dd>{/each}
      </dl>
    </details>
  </article>
  {#if analyzer && project}
    <AnalysisDialog bind:open={analyzerOpen} project={project.project_id} sourceTask={analyzer.sourceTask} table={analyzer.table} initial={analyzer.initial}
      marked={analyzer.marked} onStarted={(id) => openTask(id, project?.project_id)} {notify} />
  {/if}
  {#if editor && project}
    <FigureEditor bind:open={editorOpen} project={project.project_id} sourceTask={editor.sourceTask} table={editor.table} initial={editor.initial} {groupsOf}
      onOpenVersion={(id) => openTask(id, project?.project_id)} onDrawn={() => { if (controller) void readFigures(controller.signal); }} {notify} />
  {/if}
{/if}

<style>
  .page-state { padding: 64px var(--gutter); width: 100%; max-width: 1080px; margin: 0 auto; display: grid; gap: 12px; justify-items: start; }
  .page-state[role="status"] { display: flex; align-items: center; gap: 10px; color: var(--text-2); }
  .page { padding: 34px var(--gutter) 24px; width: 100%; max-width: 1080px; margin: 0 auto; display: grid; grid-template-columns: minmax(0, 1fr); gap: 24px; align-content: start; }
  .tab-panel { display: grid; gap: 40px; padding-top: 10px; min-width: 0; }
  .tab-panel[hidden] { display: none; }
  .panel-title { font-size: calc(19px * var(--type-scale)); letter-spacing: -0.01em; }
  .head { display: grid; gap: 14px; justify-items: start; max-width: 920px; }
  .running { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 8px; font-size: var(--font-small); color: var(--text-2); }
  .running .link { font-weight: 600; }
  .running .where, .running .who-can { font-weight: 600; color: var(--text); }
  .running .sep { color: var(--text-3); }
  .load-failed { display: grid; gap: 10px; justify-items: start; margin-left: 16px; max-width: 70ch; }
  .chosen { display: grid; gap: 8px; max-width: 760px; }
  .chosen img { width: 100%; background: #ffffff; box-shadow: inset 0 0 0 1px var(--line); }
  h1 { font-size: clamp(28px, calc(3.2vw * var(--type-scale)), calc(40px * var(--type-scale))); line-height: 1.1; letter-spacing: -0.02em; }
  .abstract { font-family: var(--serif); font-size: var(--font-lede); line-height: 1.55; max-width: 66ch; color: var(--text); }
  .lead { font-family: var(--ui); font-weight: 700; font-size: 0.78em; color: var(--heading); margin-right: 4px; }
  .now { display: flex; flex-wrap: wrap; align-items: center; gap: 12px 20px; width: 100%; padding-top: 14px; border-top: 1px solid var(--line); }
  .now .button { margin-left: auto; }
  .block { display: grid; gap: 18px; scroll-margin-top: calc(var(--bar-h) + 60px); }
  .watch-log { gap: 10px; }
  .watch-log > button { justify-self: start; }
  .files { display: grid; grid-template-columns: minmax(220px, 300px) minmax(0, 1fr); gap: 28px; align-items: start; }
  .file-list { list-style: none; margin: 0; padding: 0; max-height: 640px; overflow: auto; border-top: 2px solid var(--heading); position: sticky; top: calc(var(--bar-h) + 16px); }
  .file-list button { position: relative; width: 100%; display: grid; gap: 2px; padding: 9px 10px 9px 12px; text-align: left; border-bottom: 1px solid var(--line); }
  .file-list button.member { padding-left: 26px; }
  .file-list button:hover { background: var(--hover-wash); }
  .file-list button.current { background: var(--surface-2); }
  .file-list button.current::before { content: ""; position: absolute; left: 0; top: 6px; bottom: 6px; width: 3px; background: var(--mark); }
  .t { font-weight: 600; font-size: calc(14px * var(--type-scale)); color: var(--heading); }
  .d { font-size: var(--font-xs); line-height: 1.4; color: var(--text-3); overflow-wrap: anywhere; }
  .file-reader { min-width: 0; display: grid; gap: 12px; }
  .package-note { display: grid; gap: 10px; justify-items: start; padding-top: 12px; border-top: 2px solid var(--heading); }
  .package-note strong { color: var(--heading); font-size: calc(16px * var(--type-scale)); }
  .activity { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); max-width: 920px; }
  .activity li { display: grid; grid-template-columns: 150px minmax(0, 1fr); gap: 16px; padding: 9px 0; border-bottom: 1px solid var(--line); font-size: var(--font-small); }
  .activity time { color: var(--text-3); font-variant-numeric: tabular-nums; }
  details.technical { max-width: 920px; }
  .made { display: grid; gap: 8px; max-width: 920px; }
  .made h3 { font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  .made ol { list-style: none; margin: 0; padding: 0; border-top: 2px solid var(--heading); }
  .made li { display: grid; gap: 2px; padding: 9px 0; border-bottom: 1px solid var(--line); }
  .made .t { font-weight: 600; color: var(--heading); }
  .made .d { font-size: var(--font-small); color: var(--text-3); }
  /* A person's review: the decisions read like a referee's record, newest first, under the page's heading rule. */
  .review { display: grid; gap: 18px; max-width: 920px; }
  .claim { font-size: var(--font-small); color: var(--text-2); max-width: 76ch; }
  .reviews { list-style: none; margin: 0; padding: 0; border-top: 2px solid var(--heading); }
  .reviews li { display: grid; gap: 4px; justify-items: start; padding: 12px 0 14px; border-bottom: 1px solid var(--line); }
  .reviews .who { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 14px; font-weight: 650; }
  .reviews li.earlier .who { font-weight: 500; }
  .reviews time { font-size: var(--font-small); color: var(--text-3); font-variant-numeric: tabular-nums; }
  .reviews .note { font-family: var(--serif); font-size: calc(16px * var(--type-scale)); line-height: 1.55; max-width: 72ch; white-space: pre-line; overflow-wrap: anywhere; }
  .review-form { display: grid; gap: 10px; padding-top: 16px; border-top: 1px solid var(--line); }
  .review-form fieldset { border: 0; margin: 0; padding: 0; display: grid; gap: 2px; min-width: 0; }
  .review-form legend { padding: 0; margin-bottom: 6px; font-weight: 700; font-size: calc(16px * var(--type-scale)); color: var(--heading); }
  .decision { position: relative; display: grid; grid-template-columns: 18px minmax(0, 1fr); gap: 12px; align-items: start; padding: 9px 10px 9px 12px; cursor: pointer; }
  .decision input { margin-top: 2px; }
  .decision:hover { background: var(--hover-wash); }
  .decision:has(input:checked) { background: var(--surface-2); }
  .decision:has(input:checked)::before { content: ""; position: absolute; left: 0; top: 6px; bottom: 6px; width: 3px; background: var(--mark); }
  .decision.off { cursor: default; }
  .decision.off:hover { background: none; }
  .decision.off strong { color: var(--text-3); }
  .decision strong { color: var(--heading); margin-right: 6px; }
  .decision .hint { font-size: var(--font-small); color: var(--text-2); }
  .review-row { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px 20px; }
  .review-row p { max-width: 60ch; }
  @media (max-width: 860px) {
    .files { grid-template-columns: 1fr; }
    .file-list { position: static; max-height: 320px; }
  }
  @media (max-width: 640px) {
    .page { padding-top: 22px; gap: 20px; }
    .activity li { grid-template-columns: 1fr; gap: 2px; }
    .review-row .button { flex: 1 1 auto; }
  }
</style>
