// An analysis's working copy: the run volumes that held its input copy and its outputs while it ran.
// They are scratch, kept until a date and then cleared by the operator's daily sweep; the results and the analysis
// package are the record and stay. The line says when the working copy goes, or when it went.
export interface WorkingCopy { task_id: string; kept_until: string; cleared_at: string | null; volumes: number }

const DAY = 86_400_000;

function day(iso: string, now: Date): string {
  const at = new Date(iso);
  const sameYear = at.getUTCFullYear() === now.getUTCFullYear();
  return at.toLocaleDateString("en-US", { month: "long", day: "numeric", ...(sameYear ? {} : { year: "numeric" }), timeZone: "UTC" });
}

export function workingCopyLine(copy: WorkingCopy | undefined, now = new Date()): string | null {
  if (!copy || Number.isNaN(Date.parse(copy.kept_until))) return null;
  const stays = "Results and the package stay.";
  if (copy.cleared_at) return `Working copy cleared ${day(copy.cleared_at, now)}. ${stays}`;
  const left = Math.ceil((Date.parse(copy.kept_until) - now.getTime()) / DAY);
  if (left <= 0) return `Working copy kept until ${day(copy.kept_until, now)}; the next daily sweep clears it. ${stays}`;
  return `Working copy kept until ${day(copy.kept_until, now)} (in ${left} ${left === 1 ? "day" : "days"}). ${stays}`;
}
