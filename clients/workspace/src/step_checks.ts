import type { Tone } from "./language.ts";

// A result's check against what its step said the result must show, as the research agent read it
// in one decision: code checks every row of the stored table, and a result that fails supports nothing later.
export type CheckState = "passed" | "failed" | "not_checkable";
export interface StepCheck {
  key: string; task_id: string; ref: string; state: CheckState; table?: string; reasons?: string[]; reasons_omitted?: number;
}
export interface StepChecks { checks: StepCheck[]; omitted: number }

/** The newest check of each of a task's results across a run's decisions. */
export function checksFor(turns: { turn: number; record: { step_checks?: StepChecks } }[], taskId: string): StepCheck[] {
  const seen = new Map<string, StepCheck>();
  for (const t of [...turns].sort((a, b) => b.turn - a.turn))
    for (const c of t.record.step_checks?.checks ?? [])
      if (c.task_id === taskId && !seen.has(c.ref)) seen.set(c.ref, c);
  return [...seen.values()];
}

export const checkTone = (state: CheckState): Tone => state === "passed" ? "done" : state === "failed" ? "failed" : "attention";
export const checkLabel = (state: CheckState) => state === "passed" ? "Checked" : state === "failed" ? "Check failed" : "Not fully checked";

export function checkSentence(state: CheckState): string {
  switch (state) {
    case "passed": return "Every row of this result matches what the step said it must show.";
    case "failed": return "This result does not match what the step said it must show, so no finding or later step rests on it.";
    case "not_checkable": return "Part of what the step said this result must show could not be checked.";
  }
}
