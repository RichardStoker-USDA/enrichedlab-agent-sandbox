import { BrokerError, request } from "./api.ts";
import type { LabTool, LabTools, ToolGroup, ToolKind, ToolParameter } from "./contracts.ts";

// A lab's tools in plain words. The broker decides what each lab may run and checks it again when a
// method is proposed and when it runs; this module only reads and words the lab's list.
export const toolsClaim = "Each one is checked again when it is proposed and when it runs.";

const labPath = (key: string) => {
  if (!/^[a-z][a-z0-9_-]{1,63}$/.test(key)) throw new BrokerError("invalid_reference");
  return `/api/v1/labs/${key}/tools`;
};

export const labTools = {
  list: (lab: string, signal?: AbortSignal) => request<LabTools>(labPath(lab), { signal }),
};

const KIND_WORDS: Record<ToolKind, { name: string; sentence: string }> = {
  query_method: { name: "Query", sentence: "Reads the lab's data through reviewed, read-only queries." },
  document_method: { name: "Document", sentence: "Writes a document from results the project already holds." },
  render_method: { name: "Figure", sentence: "Draws a figure in R from a result table." },
  server_analysis: { name: "Server analysis", sentence: "Runs a reviewed analysis on the server." },
  configured_method: { name: "Configured analysis", sentence: "Runs a reviewed R template with settings a person or the agent chooses." },
  template_analysis: { name: "Template analysis", sentence: "Runs a reviewed R template on a result table." },
  sandbox_method: { name: "Agent method", sentence: "Runs in an isolated sandbox with the site's reviewed tools." },
};
export const kindName = (kind: ToolKind) => KIND_WORDS[kind]?.name ?? "Method";
export const kindSentence = (kind: ToolKind) => KIND_WORDS[kind]?.sentence ?? "";

// How a tool reached the lab, in one sentence.
export function grantWords(tool: LabTool, groups: ToolGroup[]): string {
  const names = tool.via_groups.map((key) => {
    const group = groups.find((g) => g.key === key);
    return group ? `${group.title} (version ${group.version})` : key;
  });
  if (!names.length) return "Granted to the lab directly.";
  const list = names.length === 1 ? names[0] : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
  return `Granted through ${list}.`;
}

// A parameter's accepted values in words: its type, bounds or choices, and whether it may be left out.
export function parameterWords(p: ToolParameter): string {
  const parts: string[] = [];
  if (p.type === "enum" && p.choices?.length) parts.push(`one of ${p.choices.join(", ")}`);
  else if ((p.type === "integer" || p.type === "number") && p.minimum !== undefined && p.maximum !== undefined)
    parts.push(`${p.type === "integer" ? "a whole number" : "a number"} from ${p.minimum} to ${p.maximum}`);
  else if (p.type === "text") parts.push(p.max_length ? `text up to ${p.max_length} characters` : "text");
  else if (p.type) parts.push(p.type);
  parts.push(p.required ? "required" : "optional");
  return parts.join(", ");
}

// The first 12 hex digits of a digest, for display; the full value is in the title attribute.
export const shortDigest = (digest: string) => digest.replace(/^sha256:/, "").slice(0, 12);

// The lab's tools grouped for the page: each held tool group with its tools in the list's order, then the tools
// granted directly that no group holds. A tool in two groups is listed under both.
export function toolSections(view: LabTools): { group: ToolGroup | null; tools: LabTool[] }[] {
  const sections = view.tool_groups.map((group) => ({
    group,
    tools: view.tools.filter((t) => t.via_groups.includes(group.key)),
  }));
  const direct = view.tools.filter((t) => !t.via_groups.length);
  return direct.length ? [...sections, { group: null, tools: direct }] : sections;
}

// A section's key and element id. A tool group may be keyed "direct", so group sections carry a prefix the directly
// granted section cannot have.
export const sectionKey = (section: { group: ToolGroup | null }) =>
  section.group ? `group-${section.group.key}` : "direct";

export function toolsFailure(cause: unknown): string {
  if (cause instanceof BrokerError && cause.reason === "lab_not_found")
    return "This lab's tools are shown only to its members.";
  return "The lab's tools could not be loaded. Nothing has changed; reload to try again.";
}
