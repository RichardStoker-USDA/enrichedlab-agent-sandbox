// The live status line (Glass Box rule 2): what the system is doing in the current step, in words, and how long that
// step has run. The words map recorded states only. Nothing here estimates a finish time or a percentage, and a state
// the records do not hold is not named.
/** A queued analysis's place as the broker records it: the runnable analyses submitted before it (null when it cannot
 *  start as things stand) and whether its lab's or its person's slots are all in use now. */
export interface TaskQueue { ahead: number | null; lab_full: boolean; person_full: boolean }
export interface ActiveTask { status: string; submitted_at: string; queue?: TaskQueue | null }
export interface RunState { status: string; code: string | null; updated_at: string }

/** A span of time as a scientist reads it at a glance: minutes, then hours, then days. */
export function duration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 60_000) return "under a minute";
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60), rest = minutes % 60;
  if (hours < 24) return rest ? `${hours} h ${rest} min` : `${hours} h`;
  const days = Math.floor(hours / 24), left = hours % 24;
  return `${days} ${days === 1 ? "day" : "days"}${left ? ` ${left} h` : ""}`;
}

const taskWords: Record<string, string> = {
  queued: "Waiting for a free compute slot",
  leased: "Starting the analysis",
  starting: "Starting the analysis",
  running: "Running the analysis",
  cancelling: "Stopping the analysis",
};

/** Where a queued analysis stands, in the broker's terms: how many analyses submitted earlier are ahead of it, or that
 *  it cannot start as things stand, and which limit holds it back now. A broker that does not record the queue says so
 *  through these words instead of leaving the wait unexplained. */
export function placeWords(queue: TaskQueue | null | undefined): string {
  if (!queue) return "Its place in line is not recorded yet.";
  const place = queue.ahead === null ? "It cannot start as things stand: its access, method or attempts hold it back."
    : queue.ahead === 0 ? "It is next in line."
    : queue.ahead === 1 ? "1 analysis submitted earlier is ahead of it."
    : `${queue.ahead.toLocaleString()} analyses submitted earlier are ahead of it.`;
  return [place, queue.lab_full ? "The lab's compute slots are all in use." : "",
    queue.person_full ? "The person who asked for it is using all of their slots." : ""].filter(Boolean).join(" ");
}

/** What the system does now and for how long: the oldest running analysis decides, since the step it belongs to has
 *  run longest; with none, a running work session is choosing its next step. Null when neither is recorded. A wait for
 *  a compute slot says where the analysis stands in line. */
export function nowLine(run: RunState | undefined, active: ActiveTask[], now: number): { text: string; time: string; note?: string } | null {
  const known = active.filter((t) => taskWords[t.status]).sort((a, b) => a.submitted_at.localeCompare(b.submitted_at));
  const first = known[0];
  if (first) {
    const others = known.length - 1;
    const text = others ? `${taskWords[first.status]}, and ${others === 1 ? "1 more analysis" : `${others} more analyses`} at the same time` : taskWords[first.status];
    const time = `${duration(now - Date.parse(first.submitted_at))} on this step`;
    return first.status === "queued" ? { text, time, note: placeWords(first.queue) } : { text, time };
  }
  if (run?.status === "running") return { text: "Choosing the next step", time: `${duration(now - Date.parse(run.updated_at))} so far` };
  if (run?.status === "queued") return { text: "Waiting to start", time: `${duration(now - Date.parse(run.updated_at))} so far` };
  return null;
}

/** The steps against the plan, which can change: finished, working and planned, never a share of a whole. */
export function tally(marks: string[]): { done: number; now: number; later: number; failed: number; check: number } {
  const count = (mark: string) => marks.filter((m) => m === mark).length;
  return { done: count("done"), now: count("current"), later: count("later"), failed: count("failed"), check: count("check") };
}
/** The counts in words, in plan order of concern: "24 finished, 1 working, 1 to check, 1 did not finish, 3 planned". */
export function tallyWords(marks: string[]): string[] {
  const c = tally(marks);
  return [c.done ? `${c.done} finished` : "", c.now ? `${c.now} working` : "", c.check ? `${c.check} to check` : "",
    c.failed ? `${c.failed} did not finish` : "", c.later ? `${c.later} planned` : ""].filter(Boolean);
}

/** An ended analysis's recorded times: asked for, started on a compute worker, ended. */
export interface StepTimes { submitted_at: string; started_at: string | null; finished_at: string | null }

const median = (values: number[]) => {
  const sorted = [...values].sort((a, b) => a - b), mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
};

/** How long recent analyses waited for a compute slot and ran, at the median, from their recorded times (Glass Box
 *  long runs rule 2: slow is a state, not a failure). The newest five ended analyses are compared with up to five
 *  before them, so a queue that grew shows as a longer wait, not a guess about why or when it ends. Null with fewer
 *  than three timed analyses. */
export function paceWords(times: StepTimes[]): string | null {
  const timed = times.filter((t) => t.started_at && t.finished_at)
    .map((t) => ({ end: Date.parse(t.finished_at!), wait: Date.parse(t.started_at!) - Date.parse(t.submitted_at), run: Date.parse(t.finished_at!) - Date.parse(t.started_at!) }))
    .filter((t) => Number.isFinite(t.end) && t.wait >= 0 && t.run >= 0)
    .sort((a, b) => b.end - a.end);
  const recent = timed.slice(0, 5), before = timed.slice(5, 10);
  if (recent.length < 3) return null;
  const words = (group: typeof timed) => ({ wait: duration(median(group.map((t) => t.wait))), run: duration(median(group.map((t) => t.run))) });
  const now = words(recent);
  const head = `The last ${recent.length} analyses waited ${now.wait} for a compute slot and ran ${now.run}, at the median.`;
  if (before.length < 3) return head;
  const then = words(before);
  return `${head} The ${before.length} before them waited ${then.wait} and ran ${then.run}.`;
}
