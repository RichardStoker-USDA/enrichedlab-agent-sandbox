<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { BrokerError, broker, verifiedArtifact, verifiedMember } from "./api.ts";
  import { groupsIn, strictText } from "./artifact_content.ts";
  import type { Artifact, FigureCatalog, FigureControls, Identity, PackageMember } from "./contracts.ts";
  import { loadTaskEvidence, type TaskEvidence } from "./evidence.ts";
  import AnalysisDialog from "./AnalysisDialog.svelte";
  import FigureEditor from "./FigureEditor.svelte";
  import FileView from "./FileView.svelte";
  import {
    cellsLine, cellText, columnChanges, columnContents, detailFrom, figureStart, guardWords, intakeFailure, labData, lettersWords, listWords, NAME, outputsOf,
    preparationRefusal, rowsLine, rowsWords, ruleFor, setLong, setWide, specProblems, specRefusalWords, startingSpec, suggestedRecode, toggleColumn,
    toggleRow, typeWords, unreadWords, type CensorRule, type DatasetDetail, type OutputType, type Preparation, type ProfileColumn, type ProfileSheet,
    type SheetDetail, type Spec, type Unread,
  } from "./lab_data.ts";
  import { bytes, when } from "./language.ts";
  import { live } from "./live.ts";
  import { pageTitle } from "./project_index.svelte.ts";
  import { projects, type Project } from "./projects.ts";
  import { memberResult, type ResultFile } from "./results.ts";
  import SheetGrid from "./SheetGrid.svelte";
  import StatusMark from "./StatusMark.svelte";
  import { fromMember, MAX_TABLE_BYTES, saveBlob, saveName } from "./viewer.ts";
  import type { FigureHandle } from "./views/types.ts";
  import { analyses, analysisName, analysisStatus, labTableStart, type AnalysisCatalog, type AnalysisSettings,
    type AnalysisVersionSummary } from "./template_analysis.ts";

  // A table the lab sent ("the sheet as the lab wrote it"): the upload guard's decision, the sheet in its own rows
  // and columns with the broker's reading drawn on it, each choice the profile asks for with a starting answer, and the
  // preparations the person approved, each with a receipt whose counts add up and the prepared table in its result
  // frame. The broker's own code reads and writes every cell; the page sends only the person's choices.
  let { projectId, datasetId, identity, openProject, openTask, notify }: {
    projectId: string; datasetId: string; identity: Identity; openProject: (id: string) => void;
    openTask: (task: string, project: string) => void; notify: (text: string, error?: boolean) => void;
  } = $props();

  let project = $state<Project | null>(null);
  let detail = $state<DatasetDetail | null>(null);
  let phase = $state<"loading" | "ready" | "failed">("loading");
  let failure = $state("");
  let sheetDetail = $state<SheetDetail | null>(null);
  let detailState = $state<"idle" | "loading" | "ready" | "failed">("idle");
  let spec = $state<Spec | null>(null);
  // The preparation the choices came from, which a new preparation revises; "" for the profile's starting reading.
  let revises = $state("");
  let title = $state("");
  let selected = $state("");
  let sending = $state(false);
  let sendFailure = $state("");
  let starting = $state(false);
  let catalog = $state<FigureCatalog | null>(null);
  let alive = true;
  const controller = new AbortController();

  const intakeReceipt = $derived(detail?.intake?.receipts.at(-1) ?? null);
  const profile = $derived(intakeReceipt?.status === "profiled" ? intakeReceipt.profile : null);
  const sheet = $derived<ProfileSheet | null>(profile?.sheets.find((s) => s.index === (spec?.sheet ?? 0)) ?? profile?.sheets[0] ?? null);
  const canChange = $derived(!!project && !project.inspection_expires_at && project.capabilities.includes("contribute"));
  const guard = $derived(detail ? guardWords(detail.guard) : null);
  const shown = $derived(detail?.preparations.find((p) => p.task_id === selected) ?? detail?.preparations[0] ?? null);
  const resultFirst = $derived(!!detail?.preparations.length);

  async function load(first = false) {
    try {
      const [d, p] = await Promise.all([labData.get(projectId, datasetId, controller.signal),
        first || !project ? projects.get(projectId, controller.signal) : Promise.resolve(project)]);
      if (!alive) return;
      const finished = (detail?.preparations ?? []).filter((x) => x.receipts.length).map((x) => x.task_id);
      detail = d; project = p; phase = "ready";
      pageTitle.value = d.file_name;
      const read = d.intake?.receipts.at(-1);
      if (read?.status === "profiled" && detailState === "idle") void readDetail(d.intake!.task_id);
      // A preparation that finished while the page watched is shown.
      const done = d.preparations.find((x) => x.receipts.length && !finished.includes(x.task_id) && x.task_id === selected);
      if (done && !first) announce(done);
      schedule();
    } catch (cause) {
      if (!alive || controller.signal.aborted) return;
      if (phase === "loading" || first) {
        phase = "failed";
        failure = cause instanceof BrokerError && cause.status === 404 ? "This file is not in the project, or your account cannot open it."
          : cause instanceof BrokerError && cause.reason === "person_required" ? "Only a person can open the lab's data."
          : "The file's record could not be read just now.";
      } else schedule();
    }
  }
  function announce(p: Preparation) {
    const r = p.receipts.at(-1)!;
    notify(r.status === "prepared" ? `Prepared: ${r.table_rows ?? 0} rows in ${r.table_path ?? "the table"}.` : preparationRefusal(r.error_code, r.summary), r.status !== "prepared");
  }
  // While the file is read or a table is prepared, the page reads the record again, more slowly as it waits.
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pause = 1200;
  const busy = (d: DatasetDetail) => (!!d.intake && !d.intake.receipts.length)
    || d.preparations.some((p) => !p.receipts.length && p.status !== "failed" && p.status !== "completed");
  function schedule() {
    if (timer) clearTimeout(timer);
    timer = null;
    if (!alive || !detail || !busy(detail)) { pause = 1200; return; }
    timer = setTimeout(() => { if (document.hidden) schedule(); else void load(); }, pause);
    pause = Math.min(Math.round(pause * 1.4), 6000);
  }
  async function readDetail(intakeTask: string) {
    detailState = "loading";
    try {
      const evidence = await loadTaskEvidence(intakeTask, controller.signal);
      for (const pkg of evidence.packages) {
        const member = pkg.contents.listing.members.find((m) => m.path === "profile_detail.json");
        if (!member) continue;
        const text = await strictText(await verifiedMember(intakeTask, pkg.artifact, member, controller.signal), 16 * 1024 * 1024);
        const parsed = detailFrom(JSON.parse(text));
        sheetDetail = parsed?.sheets.find((s) => s.index === (spec?.sheet ?? profile?.sheets[0]?.index ?? 0)) ?? parsed?.sheets[0] ?? null;
        break;
      }
      detailState = sheetDetail ? "ready" : "failed";
    } catch {
      if (controller.signal.aborted) return;
      detailState = "failed";
    }
    if (!spec) beginChoices();
  }
  // The choices start from the newest preparation, which a new one revises, or from the profile's reading.
  function beginChoices() {
    if (!detail || !profile?.sheets.length) return;
    const newest = detail.preparations[0];
    if (newest) {
      spec = structuredClone($state.snapshot(newest.spec) as Spec);
      title = newest.title ?? "";
      revises = newest.task_id;
    } else {
      spec = startingSpec(profile.sheets[0], detail.file_name, sheetDetail);
      revises = "";
    }
  }
  function restart() {
    if (!sheet || !detail) return;
    spec = startingSpec(sheet, detail.file_name, sheetDetail);
    notify("The choices are back to the sheet's reading.");
  }
  function useChoices(p: Preparation) {
    spec = structuredClone($state.snapshot(p.spec) as Spec);
    title = p.title ?? "";
    revises = p.task_id;
    void tick().then(() => document.getElementById("choices-title")?.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "start" }));
  }
  function chooseSheet(index: number) {
    const next = profile?.sheets.find((s) => s.index === index);
    if (!next || !detail) return;
    spec = startingSpec(next, detail.file_name, null);
    detailState = "idle";
    if (detail.intake) void readDetail(detail.intake.task_id);
  }

  onMount(() => {
    void load(true);
    void broker.figureCatalog(controller.signal).then((c) => { if (alive) catalog = c; }).catch(() => {});
    void analyses.catalog(controller.signal).then((c) => { if (alive) analysisCatalog = c; }).catch(() => {});
  });
  // A change to this lab file reads it again (live.ts), beside the page's own timer while work on it runs.
  const unwatch = live().watch([`dataset:${untrack(() => datasetId)}`], () => load(), { gapMs: 2_000 });
  onDestroy(() => { alive = false; controller.abort(); if (timer) clearTimeout(timer); unwatch(); });

  async function startReading() {
    if (!detail || starting) return;
    starting = true;
    try {
      await labData.startIntake(projectId, datasetId);
      await load();
    } catch (cause) {
      notify(cause instanceof BrokerError && cause.reason === "group_queue_full" ? "The lab's queue is still full. Try again in a minute."
        : cause instanceof BrokerError && cause.reason === "dataset_not_allowed" ? "The upload guard did not take this file, so it cannot be read." : "Reading could not start just now.", true);
    } finally { starting = false; }
  }

  // The choices, as the profile asks for them.
  const columnOf = (letter: string) => sheet?.columns.find((c) => c.letter === letter) ?? null;
  const detailOf = (letter: string) => sheetDetail?.columns.find((c) => c.letter === letter) ?? null;
  const issue = (code: string) => sheet?.issues.find((i) => i.code === code) ?? null;
  const longLetters = $derived(new Set(spec?.long?.columns ?? []));
  const wide = $derived(issue("wide_time_points"));
  const summaryRows = $derived((sheet?.table?.summary_rows ?? []).filter((r) => !!spec && r >= spec.rows.first && r <= spec.rows.last));
  const summaryOut = $derived(!!spec && summaryRows.length > 0 && summaryRows.every((r) => spec!.drop_rows.some(([a, b]) => r >= a && r <= b)));
  const summaryLabels = $derived(summaryRows.map((r) => (sheetDetail?.preview[r - 1] ?? []).find((c) => c.trim())?.trim() ?? "").filter(Boolean));
  const tokens = $derived.by(() => {
    const found = new Map<string, { count: number; letters: string[] }>();
    for (const c of sheet?.columns ?? []) for (const [token, count] of Object.entries(c.missing)) {
      if (token === "") continue;
      const was = found.get(token) ?? { count: 0, letters: [] };
      found.set(token, { count: was.count + count, letters: [...was.letters, c.letter] });
    }
    return [...found.entries()].map(([token, v]) => ({ token, ...v }));
  });
  const emptyCells = $derived((sheet?.columns ?? []).reduce((s, c) => s + (c.missing[""] ?? 0), 0));
  type Target = { key: string; label: string; integer: boolean; counts: string; noLimit: boolean; rule: CensorRule | null; limit: number | null; name: string };
  const censorTargets = $derived.by((): Target[] => {
    if (!spec || !sheet) return [];
    const counted = (c: ProfileColumn) => (c.censored.below_limit ?? 0) + (c.censored.above_limit ?? 0) + (c.censored.not_detected ?? 0) > 0;
    const say = (cols: ProfileColumn[]) => {
      const sum = (k: "below_limit" | "above_limit" | "not_detected") => cols.reduce((s, c) => s + (c.censored[k] ?? 0), 0);
      const examples = cols.flatMap((c) => detailOf(c.letter)?.examples.censored_values ?? []).slice(0, 3).map(cellText);
      return [sum("below_limit") ? `${sum("below_limit")} below a limit` : "", sum("above_limit") ? `${sum("above_limit")} above a limit` : "",
        sum("not_detected") ? `${sum("not_detected")} not detected` : ""].filter(Boolean).join(", ") + (examples.length ? `: ${examples.join(", ")}` : "");
    };
    const out: Target[] = [];
    for (const c of spec.columns) {
      const p = columnOf(c.from);
      if (!p || !counted(p) || (c.type !== "number" && c.type !== "integer")) continue;
      out.push({ key: c.from, label: `Column ${c.from}, ${c.name}`, integer: c.type === "integer", counts: say([p]), noLimit: (p.censored.not_detected ?? 0) > 0,
        rule: c.censored?.rule ?? null, limit: c.censored?.limit ?? null, name: c.name });
    }
    if (spec.long) {
      const parts = spec.long.columns.map(columnOf).filter((c): c is ProfileColumn => !!c && counted(c));
      const v = spec.long.value;
      if (parts.length && (v.type === "number" || v.type === "integer"))
        out.push({ key: "long", label: `${v.name}, ${lettersWords(spec.long.columns)}`, integer: v.type === "integer", counts: say(parts),
          noLimit: parts.some((c) => (c.censored.not_detected ?? 0) > 0), rule: v.censored?.rule ?? null, limit: v.censored?.limit ?? null, name: v.name });
    }
    return out;
  });
  const spellings = $derived.by(() => (sheet?.columns ?? []).filter((c) => c.issues.includes("category_spellings") && spec?.columns.some((s) => s.from === c.letter
    && (s.type === "category" || s.type === "text"))).map((c) => {
    const counts = new Map<string, number>();
    for (const [text, count] of detailOf(c.letter)?.values ?? []) counts.set(text.trim(), (counts.get(text.trim()) ?? 0) + count);
    const rule = spec!.columns.find((s) => s.from === c.letter)!;
    return { letter: c.letter, name: rule.name, values: [...counts.entries()].map(([text, count]) => ({ text, count, to: rule.recode?.[text] ?? text })) };
  }));
  const textInNumbers = $derived.by(() => !spec ? [] : (sheet?.columns ?? []).filter((c) => c.issues.includes("text_in_numbers") || c.issues.includes("mixed_types"))
    .map((c) => ({ letter: c.letter, name: c.name, examples: (detailOf(c.letter)?.examples.text_in_numbers ?? []).slice(0, 3), rule: ruleFor(spec!, c.letter) }))
    .filter((x) => x.rule && (x.rule.type === "number" || x.rule.type === "integer")));
  const commas = $derived((sheet?.columns ?? []).filter((c) => c.issues.includes("decimal_commas") && !!spec && !!ruleFor(spec, c.letter)));
  const idColumn = $derived((sheet?.columns ?? []).find((c) => /\b(id|mouse|animal|subject|sample|participant)\b/i.test(c.name) && !!spec?.columns.some((s) => s.from === c.letter)) ?? null);
  const keyNames = $derived(idColumn && spec ? [spec.columns.find((s) => s.from === idColumn.letter)!.name, ...(spec.long ? [spec.long.name] : [])] : []);
  const repeatedIds = $derived(idColumn?.issues.includes("duplicate_ids") ? (detailOf(idColumn.letter)?.examples.duplicate_ids ?? []) : []);
  const OTHER: Record<string, string> = {
    two_header_rows: "The names come from two rows, read together.",
    units_in_names: "Some units are written in the names, such as “(g)”; they are kept as units.",
    hidden_sheet: "This sheet is hidden in the workbook.",
    formulas_without_values: "Some formulas have no saved values, so those cells are empty here.",
    merged_cells: "Some cells are merged; each value sits in the first cell of its group.",
    error_cells: "Some cells hold spreadsheet errors, such as #DIV/0!; a preparation stops on them unless their column writes them as missing.",
    duplicate_names: "Two columns have the same name in the sheet; their suggested names differ.",
    blank_name: "A column has no name; it is named by its letter.",
    date_orders_mixed: "Some dates are written day first and others month first.",
    date_order_ambiguous: "Some dates could be day first or month first.",
    ambiguous_commas: "Some numbers have a comma that could be a decimal mark or a thousands separator.",
    thousands_separators: "Some numbers have thousands separators.",
    percent_signs: "Some numbers end in a percent sign.",
    numbers_as_text: "Some numbers are stored as text in the workbook.",
    ids_not_all_checked: "The ID column was too long to check whole for repeats.",
  };
  const otherNotes = $derived((sheet?.issues ?? []).filter((i) => i.code in OTHER).map((i) => `${OTHER[i.code]}${i.columns.length ? ` (${lettersWords(i.columns)})` : ""}`));
  const choiceCount = $derived(censorTargets.length + (tokens.length ? 1 : 0) + spellings.length + (wide ? 1 : 0) + (summaryRows.length ? 1 : 0)
    + textInNumbers.length + commas.length + (keyNames.length ? 1 : 0));

  // The edits: each changes the choices in place; the sheet and the counts follow.
  function rowClicked(row: number) { if (spec && canChange) toggleRow(spec, row); }
  function columnClicked(letter: string) { if (spec && sheet && canChange) toggleColumn(spec, sheet, letter); }
  function setSummary(out: boolean) {
    if (!spec) return;
    for (const r of summaryRows) if (out !== spec.drop_rows.some(([a, b]) => r >= a && r <= b)) toggleRow(spec, r);
  }
  function setToken(token: string, on: boolean) {
    if (!spec) return;
    spec.missing = on ? [...new Set([...spec.missing, token])].sort() : spec.missing.filter((t) => t !== token);
  }
  function setRule(key: string, rule: CensorRule) {
    const target = key === "long" ? spec?.long?.value : spec?.columns.find((c) => c.from === key);
    if (target) target.censored = { ...(target.censored ?? {}), rule };
  }
  function setLimit(key: string, value: string) {
    const target = key === "long" ? spec?.long?.value : spec?.columns.find((c) => c.from === key);
    if (!target?.censored) return;
    const n = Number(value);
    if (value.trim() && Number.isFinite(n) && n > 0) target.censored = { ...target.censored, limit: n };
    else { const { limit: _, ...rest } = target.censored; target.censored = rest; }
  }
  function setRecode(letter: string, from: string, to: string) {
    const rule = spec?.columns.find((c) => c.from === letter);
    if (!rule) return;
    const next = { ...(rule.recode ?? {}) };
    if (!to.trim() || to === from) delete next[from]; else next[from] = to;
    if (Object.keys(next).length) rule.recode = next; else delete rule.recode;
  }
  function suggestSpellings(letter: string) {
    const rule = spec?.columns.find((c) => c.from === letter);
    if (!rule) return;
    const recode = suggestedRecode(detailOf(letter));
    if (Object.keys(recode).length) rule.recode = recode; else delete rule.recode;
  }
  function keepSpellings(letter: string) { const rule = spec?.columns.find((c) => c.from === letter); if (rule) delete rule.recode; }
  function setLayout(long: boolean) {
    if (!spec || !sheet || !wide) return;
    if (long && !spec.long) setLong(spec, sheet, wide.columns);
    if (!long && spec.long) setWide(spec, sheet);
  }
  function setOther(letter: string, missing: boolean) {
    const rule = spec ? ruleFor(spec, letter) : null;
    if (!rule) return;
    if (missing) rule.other = "missing"; else delete rule.other;
  }
  function setComma(letter: string, on: boolean) {
    const rule = spec ? ruleFor(spec, letter) : null;
    if (!rule) return;
    if (on) rule.decimal = "comma"; else delete rule.decimal;
  }
  function setKey(on: boolean) { if (spec) spec.key = on ? [...keyNames] : []; }
  function setType(letter: string, type: OutputType) {
    const rule = spec ? ruleFor(spec, letter) : null;
    if (!rule) return;
    rule.type = type;
    const numeric = type === "number" || type === "integer", text = type === "text" || type === "category";
    if (!numeric) { delete rule.censored; delete rule.decimal; delete rule.thousands; delete rule.strip_percent; }
    if (type === "integer" && rule.censored && rule.censored.rule !== "missing" && rule.censored.rule !== "limit") rule.censored = { ...rule.censored, rule: "missing" };
    if (!text) delete rule.recode;
    if (type !== "logical") { delete rule.true_values; delete rule.false_values; }
    const column = columnOf(letter);
    if (numeric && column && (column.censored.below_limit || column.censored.above_limit || column.censored.not_detected) && !rule.censored) rule.censored = { rule: "missing" };
  }
  function renameKey(from: string, to: string) { if (spec) spec.key = spec.key.map((k) => (k === from ? to : k)); }
  function setName(letter: string, name: string) {
    const rule = spec?.columns.find((c) => c.from === letter);
    if (!rule) return;
    const was = rule.name;
    rule.name = name.trim().toLowerCase();
    renameKey(was, rule.name);
  }
  function setUnit(letter: string, unit: string) {
    const rule = spec ? ruleFor(spec, letter) : null;
    if (!rule) return;
    if (unit.trim()) rule.unit = unit.trim().slice(0, 32); else delete rule.unit;
  }
  function setLongName(which: "point" | "value", name: string) {
    if (!spec?.long) return;
    const clean = name.trim().toLowerCase();
    if (which === "point") { renameKey(spec.long.name, clean); spec.long.name = clean; } else spec.long.value.name = clean;
  }

  const TYPES: [OutputType, string][] = [["text", "Text"], ["category", "Category"], ["number", "Number"], ["integer", "Whole number"], ["logical", "TRUE or FALSE"],
    ["date", "Date"], ["datetime", "Date and time"], ["time", "Time"]];
  const RULES: [CensorRule, string][] = [["missing", "Write as missing"], ["limit", "Use the limit"], ["half_limit", "Half the limit"], ["limit_over_sqrt2", "The limit over √2"]];
  const outputs = $derived(spec ? outputsOf(spec) : []);
  const problems = $derived(spec ? specProblems(spec) : []);
  const keptRows = $derived.by(() => {
    if (!spec || !sheetDetail || !sheet?.table) return null;
    let kept = 0;
    for (let r = spec.rows.first; r <= spec.rows.last; r++) {
      if (spec.drop_rows.some(([a, b]) => r >= a && r <= b)) continue;
      if (r > sheetDetail.preview.length) return null;
      if ((sheetDetail.preview[r - 1] ?? []).some((c) => c.trim())) kept++;
    }
    return kept;
  });
  const reading = $derived.by(() => {
    const t = sheet?.table;
    if (!spec || !t) return [];
    const lines: string[] = [];
    const named = spec.header_rows.length ? `, with names in ${rowsWords(spec.header_rows)}${spec.units_row ? ` and units in row ${spec.units_row}` : ""}` : "";
    lines.push(`The table is ${rowsWords(Array.from({ length: spec.rows.last - spec.rows.first + 1 }, (_, i) => spec!.rows.first + i))}${named}.`);
    const quote = (texts: string[]) => texts.map((x) => cellText(x.length > 90 ? `${x.slice(0, 90)}…` : x)).join(", ");
    if (t.title_rows.length) lines.push(`${cap(rowsWords(t.title_rows))} ${t.title_rows.length === 1 ? "is a title" : "are titles"}${sheetDetail?.title_text.length ? `: ${quote(sheetDetail.title_text)}` : ""}.`);
    if (t.note_rows.length) lines.push(`${cap(rowsWords(t.note_rows))} ${t.note_rows.length === 1 ? "is a note" : "are notes"} below the table${sheetDetail?.note_text.length ? `: ${quote(sheetDetail.note_text)}` : ""}.`);
    // A quoted sentence keeps its own end: no second period after it.
    return lines.map((line) => line.replace(/([.!?…])”\.$/, "$1”"));
  });
  const cap = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

  // Approval: one request id for one set of choices, so sending the same choices again after a lost answer is one
  // preparation.
  let pending: { body: string; id: string } | null = null;
  async function prepareTable() {
    if (!spec || !detail || sending || problems.length) return;
    const body = { spec: $state.snapshot(spec) as Spec, title: title.trim().slice(0, 200), previous: revises || null };
    const text = JSON.stringify(body);
    const id = pending?.body === text ? pending.id : crypto.randomUUID();
    pending = { body: text, id };
    sending = true; sendFailure = "";
    try {
      const task = await labData.prepare(projectId, datasetId, { ...body, request_id: id });
      pending = null;
      selected = task; revises = task;
      await load();
      await tick();
      const target = document.getElementById("prepared-title");
      target?.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "start" });
      target?.focus({ preventScroll: true });
    } catch (cause) {
      sendFailure = prepareProblem(cause);
    } finally { sending = false; }
  }
  function prepareProblem(cause: unknown): string {
    if (!(cause instanceof BrokerError)) return "The choices could not be sent just now. Nothing was prepared.";
    if (cause.reason === "dataset_preparation_invalid" && cause.place && spec) return specRefusalWords(cause.place.path, cause.place.word, spec);
    switch (cause.reason) {
      case "dataset_not_profiled": return "The file is still being read. Prepare it once the reading is done.";
      case "idempotency_conflict": pending = null; return "This request was used for other choices. Select Prepare again.";
      case "dataset_previous_invalid": revises = ""; return "The earlier preparation these choices revise is not this file's. Select Prepare again to start a new line of versions.";
      case "dataset_method_unavailable": return "Lab data is not set up for this lab yet.";
      case "person_required": return "Only a person can prepare the lab's data.";
      case "dataset_not_found": case "project_not_found": return "This file is not in the project any more, or your account cannot change it.";
      default: return cause.status === 0 ? "The choices could not be sent. Check the connection and try again; sending the same choices again makes one table."
        : "The choices were not taken. Nothing was prepared.";
    }
  }

  // The prepared table of the preparation shown, from its checked package.
  type Prepared = { task: string; evidence: TaskEvidence; artifact: Artifact; member: PackageMember; copy: PackageMember | null; result: ResultFile };
  let prepared = $state<Prepared | null>(null);
  let preparedState = $state<"idle" | "loading" | "ready" | "failed">("idle");
  $effect(() => {
    const p = shown;
    const path = p?.receipts.at(-1)?.status === "prepared" ? p.receipts.at(-1)!.table_path : null;
    if (!p || !path) { prepared = null; preparedState = "idle"; return; }
    if (prepared?.task === p.task_id) return;
    void readPrepared(p.task_id, path);
  });
  async function readPrepared(task: string, path: string) {
    preparedState = "loading";
    try {
      const evidence = await loadTaskEvidence(task, controller.signal);
      for (const pkg of evidence.packages) {
        const member = pkg.contents.listing.members.find((m) => m.path === path);
        if (!member) continue;
        const copy = pkg.contents.listing.members.find((m) => m.path.startsWith("exports/") && m.path.endsWith(".spreadsheet.csv")) ?? null;
        const result = memberResult(task, pkg.artifact, member, path.replace(/^.*\//, ""), () => void saveMember(task, pkg.artifact, member));
        if (shown?.task_id !== task) return;
        prepared = { task, evidence, artifact: pkg.artifact, member, copy, result };
        preparedState = "ready";
        return;
      }
      preparedState = "failed";
    } catch { if (!controller.signal.aborted) preparedState = "failed"; }
  }
  async function saveMember(task: string, artifact: Artifact, member: PackageMember) {
    try {
      saveBlob(await verifiedMember(task, artifact, member), saveName(member.path));
      notify("Downloaded after checking its recorded checksum.");
    } catch { notify("The file did not match its recorded checksum or is unavailable. Nothing was saved.", true); }
  }
  async function savePackage(p: Prepared) {
    try {
      saveBlob(await verifiedArtifact(p.task, p.artifact), `${saveName(p.member.path).replace(/\.csv$/, "")}_package.tar.gz`);
      notify("Downloaded the package after checking its recorded checksum.");
    } catch { notify("The package did not match its recorded checksum or is unavailable. Nothing was saved.", true); }
  }

  // Figures from the prepared table, in a lab whose figure method is enabled.
  const canFigure = $derived(canChange && !!project && !!catalog?.groups.includes(project.group_key));
  let editorOpen = $state(false);
  let editor = $state<{ sourceTask: string; table: { path: string; title: string; columns: string[]; numeric: boolean[] };
    initial: { feature: string; controls: FigureControls; title: string; previous: string | null } | null } | null>(null);
  function tableColumns(p: Preparation) {
    const outs = p.receipts.at(-1)?.summary?.outputs ?? outputsOf(p.spec);
    return { columns: outs.map((o) => o.name), numeric: outs.map((o) => o.type === "number" || o.type === "integer") };
  }
  function makeFigure() {
    if (!shown || !prepared) return;
    const name = shown.title || prepared.member.path.replace(/^.*\//, "");
    editor = { sourceTask: prepared.task, table: { path: prepared.member.path, title: name, ...tableColumns(shown) },
      initial: catalog ? figureStart(shown.spec, catalog.features, shown.title ?? "") : null };
    editorOpen = true;
  }
  // Reviewed R analyses of the prepared table, in a lab whose template-analysis method is enabled. A table in the
  // long layout starts on the tolerance test, read per animal.
  let analysisCatalog = $state<AnalysisCatalog | null>(null);
  const canAnalyze = $derived(canChange && !!project && !!analysisCatalog?.groups.includes(project.group_key));
  let analyzerOpen = $state(false);
  let analyzer = $state<{ sourceTask: string; table: { path: string; title: string; columns: string[]; numeric: boolean[] };
    initial: { feature: string; settings: AnalysisSettings; title: string; previous: string | null } | null } | null>(null);
  function analyze() {
    if (!shown || !prepared) return;
    // The table's own title, or its name as the broker writes it into an analysis's name.
    const name = shown.title?.trim() || shown.spec.table_name.replaceAll("_", " ");
    analyzer = { sourceTask: prepared.task, table: { path: prepared.member.path, title: name, ...tableColumns(shown) },
      initial: labTableStart(shown.spec, analysisCatalog, name) };
    analyzerOpen = true;
  }
  // The analyses made from the shown preparation's table, newest first.
  let madeFrom = $state<AnalysisVersionSummary[]>([]);
  $effect(() => {
    const task = prepared?.task;
    madeFrom = [];
    if (!task) return;
    const c = new AbortController();
    void analyses.versions(task, c.signal).then((list) => { if (!c.signal.aborted) madeFrom = list; }).catch(() => {});
    return () => c.abort();
  });
  const figureFor = $derived.by(() => {
    if (!canFigure || !catalog || !prepared || !shown) return null;
    const charts = catalog.features.map((f) => f.id);
    const own = prepared, p = shown;
    return (file: ResultFile): FigureHandle | null => file.file.path === own.member.path ? {
      charts,
      draw: (choice) => {
        editor = { sourceTask: own.task, table: { path: own.member.path, title: p.title || own.member.path.replace(/^.*\//, ""), columns: choice.columns, numeric: choice.numeric },
          initial: { feature: choice.feature, controls: choice.controls, title: p.title || "", previous: null } };
        editorOpen = true;
      },
    } : null;
  });
  async function groupsOf(column: string, signal: AbortSignal) {
    if (!prepared) throw new Error("no_table");
    const text = await strictText(await verifiedMember(prepared.task, prepared.artifact, prepared.member, signal), MAX_TABLE_BYTES);
    return groupsIn(text, column, ",");
  }
  const STATUS: Record<string, string> = { queued: "Waiting for a compute slot", leased: "Starting", starting: "Starting", running: "Being prepared", completed: "Prepared", failed: "Not prepared" };
  function prepWords(p: Preparation): { tone: "done" | "active" | "failed" | "quiet"; label: string } {
    const r = p.receipts.at(-1);
    if (!r) return { tone: p.status === "failed" ? "failed" : "active", label: STATUS[p.status] ?? "Waiting" };
    return r.status === "prepared" ? { tone: "done", label: `Prepared · ${r.table_rows ?? 0} rows` } : { tone: "failed", label: "Not prepared" };
  }
  // Where a cell no rule read is decided: its missing word, its column's rule for limits, or the column itself.
  function fixFor(f: Unread, s: Spec): { selector: string; label: string } {
    const letter = f.column.split(",")[0];
    const inLong = !!s.long?.columns.includes(letter);
    const text = f.text.trim();
    if (tokens.some((t) => t.token === text)) return { selector: `#choice-missing input[data-token="${CSS.escape(text)}"]`, label: `Choose how ${cellText(text)} is read` };
    if ((f.reason === "censored_without_rule" || f.reason === "limit_unknown") && document.getElementById(`choice-limit-${inLong ? "long" : letter}`))
      return { selector: `#choice-limit-${inLong ? "long" : letter} input`, label: `Choose a rule for column ${letter}` };
    return { selector: `#column-${inLong ? s.long!.columns[0] : letter} :is(input, select)`, label: `Change column ${letter}` };
  }
  function fixFrom(p: Preparation, f: Unread) {
    if (revises !== p.task_id) useChoices(p);
    void tick().then(() => {
      if (!spec) return;
      const el = document.querySelector<HTMLElement>(fixFor(f, spec).selector);
      el?.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "center" });
      el?.focus({ preventScroll: true });
    });
  }
  function leaveOut(p: Preparation, row: number) {
    if (revises !== p.task_id) useChoices(p);
    void tick().then(() => { if (spec) toggleRow(spec, row); notify(`Row ${row} is left out. Prepare the table again to use it.`); });
  }
</script>

{#snippet resultSection()}
  {#if detail?.preparations.length}
    <section class="block" aria-labelledby="prepared-title">
      <h2 id="prepared-title" class="section-title" tabindex="-1">{detail.preparations.length === 1 ? "The prepared table" : "Prepared tables"}</h2>
      {#if detail.preparations.length > 1}
        <ol class="versions">
          {#each detail.preparations as p, i (p.task_id)}
            {@const w = prepWords(p)}
            <li><button class="version" aria-current={shown?.task_id === p.task_id ? "true" : undefined} onclick={() => (selected = p.task_id)}>
              <span class="v-title">{p.title || p.spec.table_name}</span>
              <span class="v-meta"><StatusMark tone={w.tone} label={w.label} /> · {when(p.created_at)}{i === 0 ? " · newest" : ""}</span>
            </button></li>
          {/each}
        </ol>
      {/if}
      {#if shown}
        {@const r = shown.receipts.at(-1)}
        {@const w = prepWords(shown)}
        <div class="receipt" aria-live="polite">
          <p class="r-head"><StatusMark tone={w.tone} label={w.label} /> <span class="soft small">{shown.title ? `${shown.title} · ` : ""}{shown.spec.table_name} · asked {when(shown.created_at)}</span></p>
          {#if !r}
            <p class="soft">The broker's own code is reading the sheet with these choices and counting every change. This takes a moment.</p>
          {:else if r.status === "prepared" && r.summary}
            <dl class="facts">
              <div><dt>Table</dt><dd><span class="mono">{r.table_path}</span>, {r.table_rows ?? 0} rows, {r.summary.outputs.length} columns</dd></div>
              <div><dt>Rows</dt><dd>{rowsLine(r.summary, shown.spec)}</dd></div>
              {#if r.summary.cells}<div><dt>Cells</dt><dd>{cellsLine(r.summary.cells)}</dd></div>{/if}
            </dl>
            <h3 class="sub">What changed, column by column</h3>
            <ul class="changes">
              {#each r.summary.columns as c (c.name + c.from)}
                <li><span class="mono name">{c.name}</span> <span class="faint small">{c.from.includes(",") ? lettersWords(c.from.split(",")) : `column ${c.from}`}</span>
                  <span class="what">{columnChanges(c).join(" · ")}</span></li>
              {/each}
            </ul>
            {#if r.summary.read_as_missing.length}
              <p class="small soft">Cells no rule read, written as missing as their column says: {r.summary.read_as_missing.slice(0, 5).map(unreadWords).join(" ")}</p>
            {/if}
            <p class="small soft">Nothing changed that these counts do not show. The research lead sees this table's columns and counts, never its rows.</p>
            <div class="actions">
              {#if canAnalyze && prepared}<button class="button" onclick={analyze}>Analyze</button>{/if}
              {#if canFigure && prepared}<button class={canAnalyze ? "button secondary" : "button"} onclick={makeFigure}>Make a figure</button>{/if}
              {#if prepared}<button class="button secondary" onclick={() => void savePackage(prepared!)}>Download the package</button>{/if}
              {#if prepared?.copy}<button class="button secondary" onclick={() => void saveMember(prepared!.task, prepared!.artifact, prepared!.copy!)}>Download a copy for Excel</button>{/if}
              {#if canChange && revises !== shown.task_id}<button class="link small" onclick={() => useChoices(shown!)}>Change these choices</button>{/if}
              <button class="link small" onclick={() => openTask(shown!.task_id, projectId)}>The full record</button>
            </div>
            {#if madeFrom.length}
              <div class="made">
                <h3>Analyses of this table</h3>
                <ol>
                  {#each madeFrom as v (v.task_id)}
                    <li><button class="link" onclick={() => openTask(v.task_id, projectId)}>{v.title || analysisName(analysisCatalog, v.feature)}</button>
                      <span class="d">{analysisName(analysisCatalog, v.feature)} · {when(v.created_at)} · {analysisStatus(v.status).label}</span></li>
                  {/each}
                </ol>
              </div>
            {/if}
            {#if preparedState === "loading"}<p class="soft small" role="status">Opening the prepared table…</p>
            {:else if preparedState === "failed"}<p class="notice error" role="alert">The prepared table could not be opened from its checked package.</p>
            {:else if prepared}
              {#key prepared.task}
                <FileView file={fromMember(prepared.member, prepared.member.path.replace(/^.*\//, ""))} load={(signal) => verifiedMember(prepared!.task, prepared!.artifact, prepared!.member, signal)}
                  title={shown.title || prepared.member.path.replace(/^.*\//, "")} onDownload={() => void saveMember(prepared!.task, prepared!.artifact, prepared!.member)}
                  result={prepared.result} figure={figureFor} />
              {/key}
            {/if}
          {:else}
            <p class="refusal">{preparationRefusal(r.error_code, r.summary)}</p>
            {#if r.summary?.failures.length}
              <ul class="unread">
                {#each r.summary.failures as f, i (i)}
                  <li><span>{unreadWords(f)}</span>{#if canChange}<button class="link small" onclick={() => fixFrom(shown!, f)}>{fixFor(f, shown!.spec).label}</button>{/if}</li>
                {/each}
              </ul>
              {#if r.summary.failure_count > r.summary.failures.length}<p class="small soft">The first {r.summary.failures.length} of {r.summary.failure_count} are listed.</p>{/if}
            {/if}
            {#if r.summary?.repeated_keys.length}
              <ul class="unread">
                {#each r.summary.repeated_keys as k, i (i)}
                  <li><span>Row {k.row} has the same {listWords(shown.spec.key)} as row {k.first_row}.</span>
                    {#if canChange}<button class="link small" onclick={() => leaveOut(shown!, k.row)}>Leave row {k.row} out</button>{/if}</li>
                {/each}
              </ul>
            {/if}
            {#if canChange && revises !== shown.task_id}<button class="link small" onclick={() => useChoices(shown!)}>Change these choices</button>{/if}
          {/if}
        </div>
      {/if}
    </section>
  {/if}
{/snippet}

{#snippet choicesSection()}
  {#if spec && sheet}
    <section class="block" aria-labelledby="sheet-title">
      <h2 id="sheet-title" class="section-title">The sheet as the lab wrote it</h2>
      {#if (profile?.sheets.length ?? 0) > 1}
        <label class="sheet-pick small">Sheet
          <select value={spec.sheet} onchange={(e) => chooseSheet(Number((e.currentTarget as HTMLSelectElement).value))} disabled={!canChange}>
            {#each profile!.sheets as s (s.index)}<option value={s.index}>{s.name ?? `Sheet ${s.index + 1}`}{s.hidden ? " (hidden)" : ""}</option>{/each}
          </select>
        </label>
      {/if}
      {#each reading as line, i (i)}<p class="reading">{line}</p>{/each}
      {#if sheetDetail}
        <SheetGrid preview={sheetDetail.preview} {sheet} {spec} onRow={rowClicked} onColumn={columnClicked} />
      {:else if detailState === "loading"}<p class="soft small" role="status">Opening the sheet…</p>
      {:else}<p class="soft small">The sheet's first rows could not be opened from the checked package. The choices below still apply.</p>{/if}
    </section>

    <section class="block" aria-labelledby="choices-title">
      <h2 id="choices-title" class="section-title" tabindex="-1">Choices before preparing</h2>
      <p class="soft">{choiceCount ? `${choiceCount === 1 ? "One choice is" : `${choiceCount} choices are`} filled in from the sheet. Change any of them; the broker applies them as written.` : "The sheet asks for no choices."}</p>
      <fieldset class="choices" disabled={!canChange}>
        <legend class="visually-hidden">Choices</legend>
        {#if summaryRows.length}
          <div class="choice">
            <p class="ask">{cap(rowsWords(summaryRows))} {summaryRows.length === 1 ? "is a summary row" : "are summary rows"}{summaryLabels.length ? ` (${listWords(summaryLabels.map(cellText))})` : ""}, not data.</p>
            <label class="check"><input type="checkbox" checked={summaryOut} onchange={(e) => setSummary((e.currentTarget as HTMLInputElement).checked)} /> Leave {summaryRows.length === 1 ? "it" : "them"} out</label>
          </div>
        {/if}
        {#if wide && spec}
          <div class="choice">
            <p class="ask">One measure is written across {lettersWords(wide.columns)}: {listWords(wide.columns.map((l) => columnOf(l)?.name ?? l).map(cellText))}.</p>
            <div class="options" role="radiogroup" aria-label="Layout">
              <label class="radio"><input type="radio" name="layout" checked={!!spec.long} onchange={() => setLayout(true)} /> One row for each point, {wide.columns.length} for each row of the sheet</label>
              <label class="radio"><input type="radio" name="layout" checked={!spec.long} onchange={() => setLayout(false)} /> Keep one column for each point</label>
            </div>
            {#if spec.long}
              <div class="names">
                <label>Points column <input class="mono" value={spec.long.name} oninput={(e) => setLongName("point", (e.currentTarget as HTMLInputElement).value)} spellcheck="false" autocomplete="off" /></label>
                <label>Values column <input class="mono" value={spec.long.value.name} oninput={(e) => setLongName("value", (e.currentTarget as HTMLInputElement).value)} spellcheck="false" autocomplete="off" /></label>
              </div>
              <p class="small soft">Points: {listWords(spec.long.values.map(String))}. Each of the table's rows becomes {spec.long.values.length} rows.</p>
            {/if}
          </div>
        {/if}
        {#each censorTargets as t (t.key)}
          <div class="choice" id={`choice-limit-${t.key}`}>
            <p class="ask">{t.label}: {t.counts}.</p>
            <div class="options" role="radiogroup" aria-label={`Values against a limit in ${t.label}`}>
              {#each RULES as [rule, label] (rule)}
                {@const off = t.integer && (rule === "half_limit" || rule === "limit_over_sqrt2")}
                <label class="radio" class:off><input type="radio" name={`rule-${t.key}`} checked={(t.rule ?? "missing") === rule} disabled={off} onchange={() => setRule(t.key, rule)} /> {label}</label>
              {/each}
            </div>
            {#if t.integer}<p class="small faint">Half the limit and the limit over √2 need the column read as numbers, not whole numbers.</p>{/if}
            {#if t.noLimit && t.rule && t.rule !== "missing"}
              <label class="inline small">Limit for values written without one <input type="number" min="0" step="any" value={t.limit ?? ""} oninput={(e) => setLimit(t.key, (e.currentTarget as HTMLInputElement).value)} /></label>
            {/if}
            <p class="small soft">Beside it, <span class="mono">{t.name}_censored</span> says below, above or not detected, and <span class="mono">{t.name}_limit</span> keeps the limit. A value above a limit keeps the limit under every rule but missing.</p>
          </div>
        {/each}
        {#if tokens.length}
          <div class="choice" id="choice-missing">
            <p class="ask">Missing is written as {listWords(tokens.map((t) => `${cellText(t.token)} (${t.count} ${t.count === 1 ? "cell" : "cells"} in ${lettersWords(t.letters)})`))}{emptyCells ? `, and ${emptyCells} ${emptyCells === 1 ? "cell is" : "cells are"} empty` : ""}.</p>
            <div class="options">
              {#each tokens as t (t.token)}
                <label class="check"><input type="checkbox" data-token={t.token} checked={spec.missing.includes(t.token)} onchange={(e) => setToken(t.token, (e.currentTarget as HTMLInputElement).checked)} /> Read <span class="mono">{cellText(t.token)}</span> as missing</label>
              {/each}
            </div>
            <p class="small faint">Empty cells are always missing.</p>
          </div>
        {/if}
        {#each spellings as s (s.letter)}
          <div class="choice">
            <p class="ask">Column {s.letter}, {s.name}, writes one value in more than one way. Choose how each is written in the table.</p>
            {#if s.values.length}
              <div class="spellings">
                {#each s.values as v (v.text)}
                  <span class="as-written"><span class="mono">{cellText(v.text)}</span> <span class="faint small">{v.count}</span></span>
                  <input class="mono" aria-label={`Write ${v.text} as`} value={v.to} oninput={(e) => setRecode(s.letter, v.text, (e.currentTarget as HTMLInputElement).value)} spellcheck="false" autocomplete="off" />
                {/each}
              </div>
              <div class="row-links"><button class="link small" type="button" onclick={() => suggestSpellings(s.letter)}>Use the most common spelling</button>
                <button class="link small" type="button" onclick={() => keepSpellings(s.letter)}>Keep them as written</button></div>
            {:else}<p class="small soft">The spellings show once the sheet opens.</p>{/if}
            <p class="small faint">Spaces around a value are trimmed before it is matched.</p>
          </div>
        {/each}
        {#each textInNumbers as x (x.letter)}
          <div class="choice" id={`text-${x.letter}`}>
            <p class="ask">Column {x.letter}, {x.name}, has text among its numbers{x.examples.length ? `: ${listWords(x.examples.map(cellText))}` : ""}.</p>
            <div class="options" role="radiogroup" aria-label={`Text in column ${x.letter}`}>
              <label class="radio"><input type="radio" name={`other-${x.letter}`} checked={x.rule?.other !== "missing"} onchange={() => setOther(x.letter, false)} /> Stop and name those cells</label>
              <label class="radio"><input type="radio" name={`other-${x.letter}`} checked={x.rule?.other === "missing"} onchange={() => setOther(x.letter, true)} /> Write them as missing</label>
            </div>
          </div>
        {/each}
        {#each commas as c (c.letter)}
          <div class="choice">
            <p class="ask">Column {c.letter}, {c.name}, writes decimals with a comma{(detailOf(c.letter)?.examples.decimal_commas ?? []).length ? `, such as ${cellText(detailOf(c.letter)!.examples.decimal_commas[0])}` : ""}.</p>
            <label class="check"><input type="checkbox" checked={ruleFor(spec, c.letter)?.decimal === "comma"} onchange={(e) => setComma(c.letter, (e.currentTarget as HTMLInputElement).checked)} /> Read the comma as the decimal mark</label>
          </div>
        {/each}
        {#if keyNames.length}
          <div class="choice">
            <p class="ask">{repeatedIds.length ? `${idColumn?.name} repeats: ${listWords(repeatedIds.map(cellText))} ${repeatedIds.length === 1 ? "is" : "are"} in more than one row.` : `${idColumn?.name} names each row.`}</p>
            <label class="check"><input type="checkbox" checked={spec.key.length > 0} onchange={(e) => setKey((e.currentTarget as HTMLInputElement).checked)} /> Stop if the same {listWords(keyNames.map((k) => k))} appears twice</label>
          </div>
        {/if}
        {#if otherNotes.length}
          <div class="choice"><p class="ask">Also in the sheet</p><ul class="notes">{#each otherNotes as note, i (i)}<li class="small soft">{note}</li>{/each}</ul></div>
        {/if}
      </fieldset>
    </section>

    <section class="block" aria-labelledby="columns-title">
      <h2 id="columns-title" class="section-title">Columns</h2>
      <p class="soft small">Names, units and types are the profile's guesses until you prepare the table.</p>
      <ol class="columns">
        {#each sheet.columns as c (c.letter)}
          {@const rule = spec.columns.find((s) => s.from === c.letter) ?? null}
          {@const inLong = longLetters.has(c.letter)}
          {#if inLong && spec.long && spec.long.columns[0] !== c.letter}
            <!-- The time points' columns are one entry, under their first column. -->
          {:else if inLong && spec.long}
            <li class="column long" id={`column-${c.letter}`}>
              <span class="letters mono">{spec.long.columns[0]}–{spec.long.columns[spec.long.columns.length - 1]}</span>
              <div class="fields">
                <p class="small">One row for each point: <span class="mono">{spec.long.name}</span> holds {listWords(spec.long.values.map(String))}, <span class="mono">{spec.long.value.name}</span> the values.</p>
                <label class="field">Read as
                  <select value={spec.long.value.type} disabled={!canChange} onchange={(e) => setType(c.letter, (e.currentTarget as HTMLSelectElement).value as OutputType)}>
                    {#each TYPES.filter(([t]) => t === "number" || t === "integer") as [t, label] (t)}<option value={t}>{label}</option>{/each}
                  </select></label>
                <label class="field">Unit <input value={spec.long.value.unit ?? ""} disabled={!canChange} oninput={(e) => setUnit(c.letter, (e.currentTarget as HTMLInputElement).value)} autocomplete="off" /></label>
              </div>
            </li>
          {:else}
            <li class="column" class:unread={!rule} id={`column-${c.letter}`}>
              <span class="letters mono">{c.letter}</span>
              <div class="fields">
                <label class="check small"><input type="checkbox" checked={!!rule} disabled={!canChange} onchange={() => columnClicked(c.letter)} /> Read</label>
                {#if rule}
                  <label class="field">Name <input class="mono" class:bad={!NAME.test(rule.name)} value={rule.name} disabled={!canChange} oninput={(e) => setName(c.letter, (e.currentTarget as HTMLInputElement).value)}
                    spellcheck="false" autocomplete="off" aria-describedby={`from-${c.letter}`} /></label>
                  <span class="from faint small" id={`from-${c.letter}`}>{c.name_from === "letter" ? `column ${c.letter} has no name` : `${cellText(c.name)} in the header`}</span>
                  <label class="field">Read as
                    <select value={rule.type} disabled={!canChange} onchange={(e) => setType(c.letter, (e.currentTarget as HTMLSelectElement).value as OutputType)}>
                      {#each TYPES as [t, label] (t)}<option value={t}>{label}</option>{/each}
                    </select></label>
                  <label class="field">Unit <input value={rule.unit ?? ""} disabled={!canChange} oninput={(e) => setUnit(c.letter, (e.currentTarget as HTMLInputElement).value)} autocomplete="off"
                    aria-describedby={`unit-${c.letter}`} /></label>
                  {#if c.unit}<span class="from faint small" id={`unit-${c.letter}`}>{c.unit_from === "units_row" ? "from the units row" : "from the header"}</span>{/if}
                {:else}
                  <span class="soft small">{cellText(c.name)}, not read</span>
                {/if}
              </div>
              <p class="contents small soft">{columnContents(c)}</p>
            </li>
          {/if}
        {/each}
      </ol>
    </section>

    {#if canChange}
      <section class="block prepare" aria-labelledby="prepare-title">
        <h2 id="prepare-title" class="section-title">Prepare the table</h2>
        <div class="names">
          <label>Table name <input class="mono" class:bad={!NAME.test(spec.table_name)} bind:value={spec.table_name} spellcheck="false" autocomplete="off" maxlength="63" /></label>
          <label>Title <input bind:value={title} maxlength="200" autocomplete="off" placeholder="Optional" /></label>
        </div>
        <p class="small soft">The table is saved as <span class="mono">results/lab_data/{spec.table_name}.csv</span>{keptRows !== null ? `, with ${spec.long ? keptRows * spec.long.columns.length : keptRows} rows` : ""} and {outputs.length} columns:
          <span class="mono out">{outputs.map((o) => o.name).join(", ")}</span>.</p>
        {#each problems as p (p.place)}<p class="notice error">{p.words}</p>{/each}
        {#if sendFailure}<p class="notice error" role="alert">{sendFailure}</p>{/if}
        <div class="actions">
          <button class="button" onclick={() => void prepareTable()} disabled={sending || problems.length > 0}>{sending ? "Sending the choices…" : "Prepare the table"}</button>
          <button class="link small" onclick={restart}>Start again from the sheet's reading</button>
        </div>
        <p class="small faint">The broker's own code applies these choices and counts every change; no model reads a value. The research lead sees the prepared table's columns and counts, never its rows.</p>
      </section>
    {/if}
  {/if}
{/snippet}

{#if phase === "loading"}
  <section class="page-state" role="status"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> Opening the lab's file…</section>
{:else if phase === "failed" || !detail}
  <section class="page-state"><h1>The file is unavailable</h1><p>{failure}</p>
    <button class="button secondary" onclick={() => openProject(projectId)}>Back to the project</button></section>
{:else}
  <article class="page">
    <header class="head">
      <p class="eyebrow">Lab data{project ? ` · ${project.title}` : ""}</p>
      <h1>{detail.file_name}</h1>
      <p class="meta small soft">{bytes(detail.size_bytes)} · sent {when(detail.created_at)}{detail.principal_id === identity.principal.id ? " by you" : ""}</p>
      {#if guard}
        <p class="guard"><StatusMark tone={guard.taken ? "done" : "failed"} label={guard.text} wrap /></p>
        {#each guard.notes as note, i (i)}<p class="small soft">Noted by the upload guard: {note}.</p>{/each}
      {/if}
    </header>

    {#if guard?.taken && !profile}
      <section class="block state">
        {#if !detail.intake}
          <p>{detail.intake_waiting ? "Waiting to be read: the lab's queue was full when it came in." : "Waiting to be read."}</p>
          {#if canChange}<button class="button" onclick={() => void startReading()} disabled={starting}>{starting ? "Starting…" : "Start reading"}</button>{/if}
        {:else if !intakeReceipt}
          <p class="reading-now"><span class="waveform" aria-hidden="true"><i></i><i></i><i></i></span> {detail.intake.status === "running" ? "Reading the sheet: where the table is, each column's name, unit and type, and what to decide." : "Waiting for a compute slot to read the sheet."}</p>
        {:else}
          <p class="notice error">{intakeFailure(intakeReceipt.error_code)}</p>
        {/if}
      </section>
    {/if}

    {#if resultFirst}{@render resultSection()}{@render choicesSection()}{:else}{@render choicesSection()}{@render resultSection()}{/if}

    <details class="technical">
      <summary><svg class="disclosure" viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg>Technical details</summary>
      <dl>
        <dt>Dataset</dt><dd class="mono">{detail.dataset_id}</dd>
        <dt>File SHA-256</dt><dd class="mono">{detail.sha256}</dd>
        <dt>Media type</dt><dd>{detail.media_type}</dd>
        <dt>Upload guard</dt><dd>{detail.guard.outcome} · policy {detail.guard.policy} · intake {detail.guard.intake}</dd>
        {#if detail.grid_sha256}<dt>Grid SHA-256</dt><dd class="mono">{detail.grid_sha256}</dd>{/if}
        {#if detail.intake}<dt>Reading</dt><dd class="mono">{detail.intake.task_id}</dd>{/if}
        {#if profile}<dt>Source</dt><dd>{profile.source.type}{profile.source.encoding ? ` · ${profile.source.encoding}` : ""}{profile.source.delimiter ? ` · delimiter ${JSON.stringify(profile.source.delimiter)}` : ""}</dd>{/if}
        {#if shown}<dt>Preparation</dt><dd class="mono">{shown.task_id} · {shown.spec_sha256}</dd>{/if}
        {#if sheet && sheet.columns_omitted}<dt>Columns profiled</dt><dd>{sheet.columns.length} shown, {sheet.columns_omitted} more</dd>{/if}
        <dt>Types</dt><dd>{(sheet?.columns ?? []).map((c) => `${c.letter} ${typeWords(c.type)}`).join(", ")}</dd>
      </dl>
    </details>
  </article>
  {#if analyzer && project}
    <AnalysisDialog bind:open={analyzerOpen} project={project.project_id} sourceTask={analyzer.sourceTask} table={analyzer.table} initial={analyzer.initial}
      onStarted={(id) => openTask(id, projectId)} {notify} />
  {/if}
  {#if editor && project}
    <FigureEditor bind:open={editorOpen} project={project.project_id} sourceTask={editor.sourceTask} table={editor.table} initial={editor.initial} {groupsOf}
      onOpenVersion={(id) => openTask(id, projectId)} onDrawn={() => notify("The figure version is saved in the project.")} {notify} />
  {/if}
{/if}

<style>
  .made { display: grid; gap: 6px; }
  .made h3 { font-size: calc(15px * var(--type-scale)); color: var(--heading); }
  .made ol { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); }
  .made li { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 14px; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .made .link { font-weight: 600; }
  .made .d { font-size: var(--font-small); color: var(--text-3); }
  .page-state { padding: 64px var(--gutter); width: 100%; max-width: 1080px; margin: 0 auto; display: grid; gap: 12px; justify-items: start; align-content: start; }
  .page-state[role="status"] { display: flex; align-items: flex-start; gap: 10px; color: var(--text-2); }
  .page { padding: 30px var(--gutter) 40px; width: 100%; max-width: 1180px; margin: 0 auto; display: flex; flex-direction: column; gap: 34px; min-width: 0; }
  .head { display: grid; gap: 6px; justify-items: start; }
  .eyebrow { font-size: var(--font-xs); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase; color: var(--text-2); }
  h1 { font-size: clamp(26px, calc(2.8vw * var(--type-scale)), calc(34px * var(--type-scale))); letter-spacing: -0.02em; line-height: 1.12; color: var(--heading);
    overflow-wrap: anywhere; }
  .guard { margin-top: 4px; }
  .block { display: grid; gap: 14px; min-width: 0; scroll-margin-top: calc(var(--bar-h) + 24px); }
  .state p { max-width: 70ch; }
  .reading-now { display: flex; gap: 10px; align-items: flex-start; color: var(--text-2); }
  .reading { max-width: 78ch; color: var(--text); }
  .sheet-pick { display: inline-flex; gap: 8px; align-items: center; }
  .sub { font-size: var(--font-body); font-weight: 650; color: var(--heading); margin-top: 6px; }
  .mono { font-family: var(--mono); font-size: 0.94em; }

  /* Choices: one sentence that says what the sheet does, then the control that decides it. */
  .choices { border: 0; padding: 0; margin: 0; display: grid; gap: 0; min-width: 0; }
  .choice { display: grid; gap: 8px; padding: 14px 0; border-top: 1px solid var(--line); min-width: 0; }
  .choice:last-child { border-bottom: 1px solid var(--line); }
  .ask { max-width: 78ch; }
  .options { display: flex; flex-wrap: wrap; gap: 6px 18px; }
  .radio, .check { display: inline-flex; gap: 8px; align-items: center; min-height: 32px; }
  .radio.off { color: var(--text-3); }
  .inline { display: inline-flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .inline input { width: 9em; }
  .names { display: flex; flex-wrap: wrap; gap: 10px 18px; }
  .names label { display: grid; gap: 4px; font-size: var(--font-small); color: var(--text-2); }
  .names input { min-width: min(18em, 100%); }
  .spellings { display: grid; grid-template-columns: max-content minmax(8em, 16em); gap: 6px 14px; align-items: center; }
  .as-written { display: inline-flex; gap: 8px; align-items: baseline; }
  .row-links { display: flex; flex-wrap: wrap; gap: 4px 16px; }
  .notes { display: grid; gap: 4px; padding-left: 1.1em; }

  /* Columns: the letter on the left, the choices for it beside, what the data rows hold under them. */
  .columns { list-style: none; padding: 0; margin: 0; display: grid; }
  .column { display: grid; grid-template-columns: 3.4em minmax(0, 1fr); gap: 4px 12px; padding: 10px 0; border-top: 1px solid var(--line); scroll-margin-top: calc(var(--bar-h) + 80px); }
  .column:last-child { border-bottom: 1px solid var(--line); }
  .column.unread .letters { color: var(--text-3); }
  .letters { font-weight: 650; color: var(--heading); padding-top: 6px; }
  .fields { display: flex; flex-wrap: wrap; gap: 6px 16px; align-items: center; min-width: 0; }
  .field { display: inline-flex; gap: 8px; align-items: center; font-size: var(--font-small); color: var(--text-2); white-space: nowrap; }
  .field input { width: 12em; max-width: 100%; }
  .field select { max-width: 12em; }
  .from { margin-left: -8px; }
  .contents { grid-column: 2; }
  input.bad { box-shadow: inset 0 0 0 1px var(--failed); }

  .prepare .out { overflow-wrap: anywhere; }
  .actions { display: flex; flex-wrap: wrap; gap: 10px 14px; align-items: center; }

  /* Preparations and the receipt. */
  .versions { list-style: none; padding: 0; margin: 0; display: grid; gap: 2px; }
  .version { display: grid; gap: 2px; width: 100%; text-align: left; font: inherit; color: inherit; background: none; border: 0; padding: 8px 10px; border-radius: var(--r-2); cursor: pointer; }
  .version:hover { background: var(--row-hover); }
  .version[aria-current="true"] { background: var(--track); box-shadow: inset 2px 0 0 var(--mark); }
  .v-title { font-weight: 600; color: var(--heading); }
  .v-meta { font-size: var(--font-small); color: var(--text-2); }
  .receipt { display: grid; gap: 12px; min-width: 0; }
  .r-head { display: flex; flex-wrap: wrap; gap: 6px 12px; align-items: baseline; }
  .facts { display: grid; gap: 6px; }
  .facts div { display: grid; grid-template-columns: 6em minmax(0, 1fr); gap: 12px; }
  .facts dt { color: var(--text-2); font-size: var(--font-small); padding-top: 1px; }
  .facts dd { min-width: 0; overflow-wrap: anywhere; }
  .changes { list-style: none; padding: 0; margin: 0; display: grid; gap: 6px; }
  .changes li { display: flex; flex-wrap: wrap; gap: 2px 10px; align-items: baseline; }
  .changes .name { font-weight: 600; color: var(--heading); }
  .changes .what { flex-basis: 100%; color: var(--text); font-size: var(--font-small); }
  .refusal { font-weight: 600; color: var(--failed); }
  .unread { list-style: none; padding: 0; margin: 0; display: grid; gap: 6px; }
  .unread li { display: flex; flex-wrap: wrap; gap: 4px 14px; align-items: baseline; }

  .technical { font-size: var(--font-small); color: var(--text-2); }
  .technical summary { cursor: pointer; display: inline-flex; gap: 6px; align-items: center; }
  .technical dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 4px 14px; margin-top: 10px; }
  .technical dd { overflow-wrap: anywhere; }
  .disclosure { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 1.6; }
  .technical[open] .disclosure { transform: rotate(90deg); }
  @media (max-width: 640px) {
    .page { padding-top: 22px; gap: 28px; }
    .spellings { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); }
    .column { grid-template-columns: 2.6em minmax(0, 1fr); }
    .from { margin-left: 0; }
  }
</style>
