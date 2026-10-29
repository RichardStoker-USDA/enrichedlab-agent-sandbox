// Each lab's starting points (its registered data and method sets), read once per visit and shared by home's ask field,
// the setup sheet and a new lab's welcome. A lab the broker has none for answers not found, which reads as none; any
// other failure is not remembered, so the next read tries again. A broker that lets a project start from several sets
// says so (`combine`); an older one takes one set per project.
import { BrokerError } from "./api.ts";
import { projects, type WorkspaceChoice } from "./projects.ts";

const known = new Map<string, WorkspaceChoice[]>();
const several = new Set<string>();
const pending = new Map<string, Promise<WorkspaceChoice[]>>();

export const knownStarts = (lab: string): WorkspaceChoice[] | null => known.get(lab) ?? null;
/** Whether a project in this lab may start from several of its sets, once its starting points are read. */
export const combines = (lab: string): boolean => several.has(lab);

export function readStarts(lab: string): Promise<WorkspaceChoice[]> {
  const have = known.get(lab);
  if (have) return Promise.resolve(have);
  let reading = pending.get(lab);
  if (!reading) {
    reading = projects.labWorkspaces(lab)
      .then((r) => { known.set(lab, r.workspaces); if (r.combine === true) several.add(lab); else several.delete(lab); return r.workspaces; })
      .catch((cause) => {
        if (cause instanceof BrokerError && cause.status === 404) { known.set(lab, []); return []; }
        throw cause;
      })
      .finally(() => pending.delete(lab));
    pending.set(lab, reading);
  }
  return reading;
}

/** One sentence for the ask field: what the agent can work from in this lab, by the names the lab registered. */
export function startsSentence(starts: WorkspaceChoice[] | null, labName: string, failed = false, combine = false): string {
  if (failed) return `What ${labName} can work from could not be read. Continue to try again.`;
  if (!starts) return `Reading what ${labName} can work from…`;
  if (!starts.length) return `No data and method set is registered for ${labName} yet, so the agent can't start research here.`;
  if (starts.length === 1) return `The agent can work from ${starts[0].name}.`;
  return `The agent can work from ${starts.length} sets in ${labName}; you choose ${combine ? "one, several or all of them" : "one"} next.`;
}

/** The methods a project may use from the chosen sets: each set's allowed methods, together, in the order offered. */
export function methodsOf(chosen: WorkspaceChoice[]): string[] {
  return [...new Set(chosen.flatMap((c) => c.allowed_profiles))];
}

/** What the chosen sets come to, for the line under the list. */
export function chosenSentence(chosen: WorkspaceChoice[], total: number): string {
  if (!chosen.length) return total > 1 ? "Choose at least one set." : "";
  if (chosen.length === 1) return total > 1 ? `The agent works from ${chosen[0].name}.` : "";
  const data = new Set(chosen.flatMap((c) => c.input_refs)).size, tools = new Set(chosen.flatMap((c) => c.tool_refs)).size;
  const which = chosen.length === total ? (total === 2 ? "both sets" : `all ${total} sets`) : `${chosen.length} of the ${total} sets`;
  return `The agent works across ${which}: ${data === 1 ? "one data source" : `${data} data sources`} and ${tools === 1 ? "one method" : `${tools} methods`}.`;
}
