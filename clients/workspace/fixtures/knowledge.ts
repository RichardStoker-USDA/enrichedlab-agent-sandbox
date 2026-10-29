import type { IncomingMessage, ServerResponse } from "node:http";
import type { CommonsKind, CommonsRecord, LabLink } from "../src/contracts.ts";

// A synthetic commons for the loopback preview, shaped like the broker's knowledge routes. Titles and
// summaries are invented; nothing here names a real dataset, paper or tool. The reader leads the commons unless the
// browser suite says otherwise.

type Json = (value: unknown, status?: number) => void;
type Role = "member" | "curator" | "lead" | null;
const kinds: CommonsKind[] = ["dataset", "paper", "tool", "object_collection"];

export function knowledgeFixtures(options: { at: (seconds: number) => string }) {
  const { at } = options;
  let role: Role = "lead";
  let labRole: Role = "curator";
  // Two links the preview's library papers propose to the lab's tools; the reader curates the lab unless told otherwise.
  const links: LabLink[] = [
    { relation_id: "01920000-0000-7000-8000-00000000d101", kind: "uses_tool", created_at: at(-7200),
      source: { id: "80000000-0000-4000-8000-000000000001", kind: "paper", title: "Fibre-rich breakfasts and reported fullness in a synthetic feeding study" },
      target: { id: "01920000-0000-7000-8000-00000000d201", kind: "tool", title: "Public R mean bootstrap" }, terms: ["bootstrap"], sections: 2 },
    { relation_id: "01920000-0000-7000-8000-00000000d102", kind: "uses_tool", created_at: at(-7100),
      source: { id: "80000000-0000-4000-8000-000000000002", kind: "paper", title: "Plasma markers after three test diets: an invented methods note" },
      target: { id: "01920000-0000-7000-8000-00000000d202", kind: "tool", title: "Public R median bootstrap" }, terms: ["bootstrapped"], sections: 1 },
  ];
  const record = (n: number, kind: CommonsKind, title: string, summary: string | null, baseline: boolean, extra: Partial<CommonsRecord> = {}): CommonsRecord => ({
    resource_id: `01920000-0000-7000-8000-00000000c${String(n).padStart(3, "0")}`, kind, scope: "commons", baseline, title, summary,
    data_class: "public_reference", rights: "public", lifecycle: "active", source_uri: null, created_at: at(-86400 * n), ...extra,
  });
  const records: CommonsRecord[] = [
    record(1, "dataset", "Synthetic food composition reference, release 1", "An invented table of foods and nutrient amounts, made for this preview.", true,
      { source_uri: "https://example.invalid/synthetic/food-composition/release-1" }),
    record(2, "dataset", "Invented portion sizes for common foods", "Made-up household measures and gram weights for a few hundred invented foods.", false),
    record(3, "dataset", "Placeholder dietary survey codebook", "An invented codebook for a survey that never ran; replaced by a newer placeholder.", false, { lifecycle: "superseded" }),
    record(4, "paper", "An invented note on reporting nutrient units", null, false, { data_class: "paper", source_uri: "literature://synth/paper/4" }),
    record(5, "tool", "Synthetic nutrient summary tool", "A made-up reviewed tool that averages nutrient amounts per food group.", true, { data_class: "synthetic" }),
    record(6, "paper", "A made-up feeding trial with three test diets", "Invented methods and tables, marked before two of its tables were read.", true,
      { data_class: "paper", source_uri: "literature://synth/paper/6", parts_waiting: 2 }),
    record(7, "paper", "Placeholder review of fibre and fullness", null, false, { data_class: "paper", source_uri: "literature://synth/paper/7" }),
    record(8, "dataset", "Invented supplement table for the feeding trial", null, false, { source_uri: "literature://synth/dataset/8" }),
  ];
  // Papers come two to a page, so the preview shows "Show more".
  const pageSize = (kind: CommonsKind) => (kind === "paper" ? 2 : 50);
  // Each paper has five invented parts that a paper's or a collection's mark covers.
  const PARTS = 5;
  // Preview-only: the next change is made but its answer is lost, as when a connection drops after a change is sent.
  let dropNext = false;
  const prefixOf = (uri: string | null) => /^[a-z][a-z0-9+.-]*:\/\/[a-z0-9][a-z0-9._-]*\//.exec(uri ?? "")?.[0] ?? null;
  return {
    setRole(next: Role) { role = next; },
    dropNextAnswer() { dropNext = true; },
    // Preview-only: a marked paper of the synthetic collection gains parts after its mark.
    addParts() {
      const paper = records.find((r) => r.kind === "paper" && r.baseline && r.lifecycle === "active" && (r.source_uri ?? "").startsWith("literature://synth/"));
      if (paper) paper.parts_waiting = 2;
      return !!paper;
    },
    setLabRole(next: Role) { labRole = next; },
    async handle(req: IncomingMessage, _res: ServerResponse, url: URL, json: Json): Promise<boolean> {
      if (url.pathname === "/api/v1/knowledge/commons" && req.method === "GET") {
        const kind = (url.searchParams.get("kind") ?? "dataset") as CommonsKind;
        if (!kinds.includes(kind)) { json({ error: { code: "knowledge_kind_invalid", message: "Invalid kind" } }, 400); return true; }
        const all = records.filter((r) => r.kind === kind).sort((a, b) => b.created_at.localeCompare(a.created_at) || b.resource_id.localeCompare(a.resource_id));
        const after = url.searchParams.get("after");
        const from = after ? all.findIndex((r) => `${Date.parse(r.created_at) * 1000}.${r.resource_id}` === after) + 1 : 0;
        if (after && from === 0) { json({ error: { code: "knowledge_cursor_invalid", message: "Invalid cursor" } }, 400); return true; }
        const page = all.slice(from, from + pageSize(kind));
        const last = page.at(-1);
        const next = from + page.length < all.length && last ? `${Date.parse(last.created_at) * 1000}.${last.resource_id}` : null;
        const shown = page.map((r) => (r.kind === "paper" ? { ...r, parts_waiting: r.baseline ? (r.parts_waiting ?? 0) : 0 } : r));
        json({ scope: "commons", kind, records: shown, complete: next === null, next, role, can_mark: role === "lead" });
        return true;
      }
      if (url.pathname === "/api/v1/knowledge/commons/collections" && req.method === "GET") {
        const held = new Map<string, { prefix: string; kinds: Record<string, { active: number; marked: number }>; parts_waiting: number }>();
        for (const r of records) {
          const prefix = prefixOf(r.source_uri);
          if (!prefix) continue;
          const c = held.get(prefix) ?? { prefix, kinds: {}, parts_waiting: 0 };
          const k = (c.kinds[r.kind] ??= { active: 0, marked: 0 });
          if (r.lifecycle === "active") { k.active += 1; if (r.baseline) k.marked += 1; }
          if (r.kind === "paper" && r.baseline && r.lifecycle === "active") c.parts_waiting += r.parts_waiting ?? 0;
          held.set(prefix, c);
        }
        json({ scope: "commons", collections: [...held.values()], role, can_mark: role === "lead" });
        return true;
      }
      if (url.pathname === "/api/v1/knowledge/commons/collections/baseline" && req.method === "POST") {
        let body = "";
        for await (const chunk of req) body += chunk;
        const { prefix, baseline } = JSON.parse(body || "{}") as { prefix?: unknown; baseline?: unknown };
        if (typeof prefix !== "string" || typeof baseline !== "boolean" || prefixOf(prefix) === null) {
          json({ error: { code: "knowledge_collection_invalid", message: "Invalid" } }, 400); return true;
        }
        if (role !== "lead") { json({ error: { code: "baseline_refused", message: "Refused" } }, 403); return true; }
        let changed = 0; let parts = 0;
        for (const r of records.filter((x) => x.source_uri?.startsWith(prefix) && (!baseline || x.lifecycle === "active"))) {
          if (r.kind === "paper") parts += r.baseline === baseline ? (baseline ? r.parts_waiting ?? 0 : 0) : PARTS + (r.parts_waiting ?? 0);
          if (r.baseline !== baseline) { r.baseline = baseline; changed += 1; }
          if (r.kind === "paper") r.parts_waiting = 0;
        }
        if (dropNext) { dropNext = false; json({ error: { code: "knowledge_outcome_unknown", message: "Unconfirmed" } }, 504); return true; }
        json({ prefix, baseline, records_changed: changed, parts_changed: parts });
        return true;
      }
      const queue = /^\/api\/v1\/knowledge\/labs\/([a-z0-9][a-z0-9._-]{0,62})\/proposals$/.exec(url.pathname);
      if (queue && req.method === "GET") {
        json({ lab: queue[1], proposals: queue[1] === "open_methods" ? links : [], complete: true, role: labRole,
          can_review: labRole === "curator" || labRole === "lead" });
        return true;
      }
      const reviewed = /^\/api\/v1\/knowledge\/relations\/([0-9a-f-]{36})\/review$/.exec(url.pathname);
      if (reviewed && req.method === "POST") {
        let body = "";
        for await (const chunk of req) body += chunk;
        const { accept, note } = JSON.parse(body || "{}") as { accept?: unknown; note?: unknown };
        if (typeof accept !== "boolean" || (note !== null && note !== undefined && (typeof note !== "string" || note.length > 500))) {
          json({ error: { code: "knowledge_note_invalid", message: "Invalid" } }, 400); return true;
        }
        const at = links.findIndex((l) => l.relation_id === reviewed[1]);
        if (at < 0 || !(labRole === "curator" || labRole === "lead")) { json({ error: { code: "relation_not_found", message: "Not found" } }, 404); return true; }
        links.splice(at, 1);
        if (dropNext) { dropNext = false; json({ error: { code: "knowledge_outcome_unknown", message: "Unconfirmed" } }, 504); return true; }
        json({ relation_id: reviewed[1], outcome: accept ? "accepted" : "rejected" });
        return true;
      }
      const match = /^\/api\/v1\/knowledge\/resources\/([0-9a-f-]{36})\/baseline$/.exec(url.pathname);
      if (!match || req.method !== "POST") return false;
      let body = "";
      for await (const chunk of req) body += chunk;
      const baseline = (JSON.parse(body || "{}") as { baseline?: unknown }).baseline;
      if (typeof baseline !== "boolean") { json({ error: { code: "invalid_request", message: "Invalid body" } }, 400); return true; }
      if (role !== "lead") { json({ error: { code: "baseline_refused", message: "Refused" } }, 403); return true; }
      const found = records.find((r) => r.resource_id === match[1]);
      if (!found) { json({ error: { code: "resource_not_found", message: "Not found" } }, 404); return true; }
      if (baseline && found.lifecycle !== "active") { json({ error: { code: "baseline_not_active", message: "Not active" } }, 409); return true; }
      const parts = found.kind !== "paper" ? 0 : found.baseline === baseline ? (baseline ? found.parts_waiting ?? 0 : 0) : PARTS + (found.parts_waiting ?? 0);
      found.baseline = baseline;
      if (found.kind === "paper") found.parts_waiting = 0;
      if (dropNext) { dropNext = false; json({ error: { code: "knowledge_outcome_unknown", message: "Unconfirmed" } }, 504); return true; }
      json({ ...found, parts_changed: parts });
      return true;
    },
  };
}
