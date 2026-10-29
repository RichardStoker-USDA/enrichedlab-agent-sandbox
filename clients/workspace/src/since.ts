// "Since you last looked" (Glass Box, long runs rule 1): what the records hold that is newer than the reader's own last
// look. Every row is counted from records (analysis states, retained packages and their checksum lists, the methods
// check of each decision, instructions and the decision that read them, the agent's answer); nothing is estimated.
import type { Answer } from "./findings.ts";

export interface SinceInput {
  /** The reader's last look, as the broker recorded it before this visit. */
  at: string;
  plan: { title: string; task_id: string | null }[];
  tasks: { task_id: string; status: string; updated_at: string }[];
  /** One retained package each, with its members counted by kind and whether they match its checksum list. */
  files: { task_id: string; created_at: string; figures: number; tables: number; code: number; logs: number; other: number; intact: boolean }[];
  turns: { turn: number; created_at: string; outcome: string; review: string; proposed: number }[];
  instructions: { text: string; read: { turn: number; at: string } | null }[];
  answer: { answer: Answer; at: string; findings: number; parts: { state: string }[] } | null;
}
export type SinceKey = "answer" | "steps" | "stopped" | "files" | "checks" | "instructions";
export interface SinceRow { key: SinceKey; title: string; detail: string; tone: "done" | "failed" | "attention" | "quiet" }

const after = (value: string | null | undefined, at: string) => !!value && Date.parse(value) > Date.parse(at);
const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
export const list = (items: string[]) => items.length <= 1 ? items.join("") : `${items.slice(0, -1).join(", ")} and ${items.at(-1)}`;

/** Step numbers as a reader says them: "Step 3", "Steps 9 to 13", "Steps 2, 4 and 7". */
export function stepNumbers(numbers: number[]): string {
  const sorted = [...new Set(numbers)].sort((a, b) => a - b);
  if (!sorted.length) return "";
  if (sorted.length === 1) return `Step ${sorted[0]}`;
  const run = sorted.length > 2 && sorted.every((n, i) => i === 0 || n === sorted[i - 1] + 1);
  return run ? `Steps ${sorted[0]} to ${sorted.at(-1)}` : `Steps ${list(sorted.map(String))}`;
}

const kindWords: [keyof SinceInput["files"][number], string, string][] = [
  ["figures", "figure", "figures"], ["tables", "table", "tables"], ["code", "code file", "code files"], ["logs", "log", "logs"], ["other", "other file", "other files"],
];

const answerTitle: Record<Answer, string> = {
  answered: "It says the results answer your question",
  partly_answered: "It says the results answer part of your question",
  not_answered: "It says the results do not answer your question",
  not_yet: "No answer yet",
};

export function sinceLooked(input: SinceInput): SinceRow[] {
  const { at } = input;
  const rows: SinceRow[] = [];
  const stepOf = (task: string) => input.plan.findIndex((s) => s.task_id === task) + 1;
  const describe = (tasks: string[]) => {
    const steps = tasks.map(stepOf).filter((n) => n > 0);
    if (steps.length === 1 && tasks.length === 1) return `Step ${steps[0]}: ${input.plan[steps[0] - 1].title}.`;
    const named = stepNumbers(steps);
    const loose = tasks.length - steps.length;
    return [named, loose ? `${plural(loose, "analysis", "analyses")} run outside the plan` : ""].filter(Boolean).join(" and ") + ".";
  };

  const finished = input.tasks.filter((t) => (t.status === "completed" || t.status === "review_required") && after(t.updated_at, at));
  if (finished.length) {
    const ids = new Set(finished.map((t) => t.task_id));
    const made = input.files.filter((f) => ids.has(f.task_id));
    const broken = made.filter((f) => !f.intact).length;
    const allSteps = finished.every((t) => stepOf(t.task_id) > 0);
    const files = !made.length ? "" : broken ? ` ${plural(broken, "package does", "packages do")} not match ${broken === 1 ? "its" : "their"} checksum list.`
      : ` Every file ${finished.length === 1 ? "it" : "they"} made matches its checksum.`;
    rows.push({ key: "steps", title: allSteps ? plural(finished.length, "step finished", "steps finished") : plural(finished.length, "analysis finished", "analyses finished"),
      detail: describe(finished.map((t) => t.task_id)) + files, tone: broken ? "attention" : "done" });
  }
  const stopped = input.tasks.filter((t) => t.status === "failed" && after(t.updated_at, at));
  if (stopped.length) {
    const allSteps = stopped.every((t) => stepOf(t.task_id) > 0);
    rows.push({ key: "stopped", title: `${allSteps ? plural(stopped.length, "step", "steps") : plural(stopped.length, "analysis", "analyses")} did not finish`,
      detail: `${describe(stopped.map((t) => t.task_id))} ${stopped.length === 1 ? "Its record says why." : "Their records say why."}`, tone: "failed" });
  }

  const fresh = input.files.filter((f) => after(f.created_at, at));
  const total = fresh.reduce((n, f) => n + f.figures + f.tables + f.code + f.logs + f.other, 0);
  if (total) {
    const kinds = kindWords.map(([key, one, many]) => [fresh.reduce((n, f) => n + (f[key] as number), 0), one, many] as const)
      .filter(([n]) => n > 0).map(([n, one, many]) => plural(n, one, many));
    rows.push({ key: "files", title: plural(total, "new file", "new files"), detail: list(kinds), tone: "quiet" });
  }

  const decided = input.turns.filter((t) => after(t.created_at, at));
  const agreed = decided.filter((t) => t.proposed > 0 && t.review === "proceed").length;
  const asked = decided.filter((t) => t.proposed > 0 && t.review !== "proceed" && t.outcome !== "revise").length;
  const sentBack = decided.filter((t) => t.outcome === "revise").length;
  const intact = fresh.filter((f) => f.intact).length, broken = fresh.length - intact;
  if (agreed + asked + sentBack + fresh.length) {
    const passed = agreed + intact;
    const title = [passed ? plural(passed, "check passed", "checks passed") : "", sentBack ? `${sentBack} sent back and redone` : "",
      asked ? `${asked} asked for a person's review` : "", broken ? `${plural(broken, "file check", "file checks")} failed` : ""].filter(Boolean).join("; ");
    const detail = [
      agreed + asked + sentBack ? `The methods check ${list([agreed ? `agreed with ${plural(agreed, "proposal", "proposals")}` : "",
        asked ? `asked for a person's review of ${asked === 1 ? "one" : asked}` : "", sentBack ? `sent ${sentBack === 1 ? "one" : sentBack} back to the agent` : ""].filter(Boolean))}.` : "",
      fresh.length ? (broken ? `${plural(broken, "result package does", "result packages do")} not match ${broken === 1 ? "its" : "their"} checksum list.`
        : fresh.length === 1 ? "The new result package matches its checksum list."
        : `${fresh.length === 2 ? "Both" : `All ${fresh.length}`} new result packages match their checksum lists.`) : "",
    ].filter(Boolean).join(" ");
    rows.push({ key: "checks", title: title.charAt(0).toUpperCase() + title.slice(1), detail, tone: broken ? "attention" : "done" });
  }

  const read = input.instructions.filter((i) => i.read && after(i.read.at, at));
  if (read.length) {
    const quote = (text: string) => `“${text.length > 160 ? `${text.slice(0, 157).trimEnd()}…` : text}”`;
    rows.push(read.length === 1
      ? { key: "instructions", title: `It read an instruction at decision ${read[0].read!.turn}`, detail: quote(read[0].text), tone: "quiet" }
      : { key: "instructions", title: `It read ${read.length} instructions`, detail: `The newest: ${quote(read.at(-1)!.text)}`, tone: "quiet" });
  }

  // The answer leads when it is new; otherwise it closes the list with where the answer stands.
  if (rows.length || after(input.answer?.at, at)) {
    const settled = (input.answer?.parts ?? []).filter((p) => p.state === "supported" || p.state === "unsupported").length;
    const parts = input.answer?.parts.length ?? 0;
    const partWords = parts ? `It has settled ${settled} of the question's ${parts} parts.` : "";
    if (input.answer && after(input.answer.at, at)) {
      const findings = input.answer.findings ? `${plural(input.answer.findings, "finding", "findings")}, each value read from a result file.` : "";
      rows.unshift({ key: "answer", title: answerTitle[input.answer.answer], detail: [findings, partWords].filter(Boolean).join(" ") || "It states no findings.",
        tone: input.answer.answer === "not_yet" ? "quiet" : "attention" });
    } else if (!input.answer || input.answer.answer === "not_yet") {
      rows.push({ key: "answer", title: "No answer yet", detail: [partWords, "Findings come with its answer."].filter(Boolean).join(" "), tone: "quiet" });
    } else {
      rows.push({ key: "answer", title: "No new answer", detail: "Its earlier answer still stands.", tone: "quiet" });
    }
  }
  return rows;
}
