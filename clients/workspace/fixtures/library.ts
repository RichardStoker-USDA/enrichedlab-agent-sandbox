import { createHash, randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { LibraryLabel, LibraryLabelNamespace, LibraryPaper } from "../src/contracts.ts";

// A synthetic lab library for the loopback preview, shaped like broker migrations 028 and 034's answers.
// Titles, authors, abstracts and passages are invented; nothing here comes from a real paper.

type Json = (value: unknown, status?: number) => void;
type LabelEvent = { kind: "added" | "removed"; namespace: LibraryLabelNamespace; value: string; by_person: boolean; at: string; actor: string; request_id: string };
type PaperEvent = { kind: "removed" | "restored" | "accepted" | "acceptance_withdrawn"; reason: string | null; at: string; by: string | null; actor: string; request_id: string };
type PaperRecord = Omit<LibraryPaper, "labels" | "removed" | "acceptance" | "can_remove" | "can_accept" | "added_by"> & {
  added_by_id: string; added_by_name: string | null; added_by_kind: "person" | "service_account"; document: string;
  request_id: string; labels_log: LabelEvent[]; events: PaperEvent[];
  text: { section: string; pages: string; body: string }[]; abstract: string; figures: { label: string; caption: string; page: number }[];
  // Preview-only: the paper's writes are made but their answers are lost, as when a gateway times out after the broker
  // saved; set for a PDF whose bytes carry `% lose-answer`.
  lose_answers?: boolean;
};
// The preview's paper whose answers are lost keeps one address, so the browser check can name its refused requests.
export const LOST_ANSWER_PAPER = "80000000-0000-4000-8000-0000000000aa";
const lostAnswer = (res: ServerResponse) => { res.writeHead(504, { "content-type": "text/plain" }); res.end("Gateway timeout"); };
const namespaces = ["topic", "organism", "food", "compound", "method", "design", "population"];
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function libraryFixtures(options: { me: string; at: (seconds: number) => string }) {
  const { me, at } = options;
  const colleague = { id: "20000000-0000-4000-8000-000000000002", name: "Example colleague" };
  const papers: PaperRecord[] = [
    {
      paper_id: "80000000-0000-4000-8000-000000000001", lab: "open_methods", document: randomUUID(),
      title: "Fibre-rich breakfasts and reported fullness in a synthetic feeding study",
      authors: ["Ada Example", "Ben Sample", "Cora Placeholder", "Dev Fixture"], year: 2025, doi: "10.5555/synthetic.0001", page_count: 9,
      byte_size: 812_344, source_sha256: "1".repeat(64), parse: { state: "indexed", severity: "ok", chunks: 42, figures: 3, index: "chunks_only" },
      added_by_id: colleague.id, added_by_name: colleague.name, added_by_kind: "person", added_at: at(-3000), request_id: randomUUID(),
      labels_log: [
        { kind: "added", namespace: "topic", value: "satiety", by_person: true, at: at(-2900), actor: colleague.id, request_id: randomUUID() },
        { kind: "added", namespace: "design", value: "crossover trial", by_person: false, at: at(-2800), actor: "service", request_id: randomUUID() },
      ],
      events: [{ kind: "accepted", reason: null, at: at(-2700), by: "Example researcher", actor: me, request_id: randomUUID() }],
      abstract: "An invented crossover study in which each participant ate a fibre-rich and a standard breakfast on separate mornings and rated fullness every thirty minutes.",
      figures: [{ label: "Figure 1", caption: "Invented fullness ratings over three hours after each breakfast.", page: 4 },
        { label: "Figure 2", caption: "Invented energy intake at the following lunch.", page: 5 }],
      text: [
        { section: "Methods", pages: "page 3", body: "Participants ate each breakfast on a separate morning, in random order, and rated fullness on a visual scale every thirty minutes." },
        { section: "Results", pages: "page 4", body: "Reported fullness stayed higher after the fibre-rich breakfast for the first two hours in this synthetic study." },
      ],
    },
    {
      paper_id: "80000000-0000-4000-8000-000000000002", lab: "open_methods", document: randomUUID(),
      title: "Plasma markers after three test diets: an invented methods note",
      authors: ["Eve Illustrative"], year: 2023, doi: null, page_count: 5,
      byte_size: 301_220, source_sha256: "2".repeat(64), parse: { state: "indexed", severity: "ok", chunks: 17, figures: 1, index: "chunks_only" },
      added_by_id: me, added_by_name: "Example researcher", added_by_kind: "person", added_at: at(-600), request_id: randomUUID(),
      labels_log: [], events: [],
      abstract: "A made-up note describing how plasma samples were handled for a three-diet comparison.",
      figures: [{ label: "Figure 1", caption: "Invented sample handling timeline.", page: 2 }],
      text: [{ section: "Sample handling", pages: "page 2", body: "Plasma was separated within one hour and stored frozen until the synthetic assay." }],
    },
  ];

  function labels(r: PaperRecord): LibraryLabel[] {
    const keys = [...new Set(r.labels_log.map((e) => `${e.namespace}\u0000${e.value}`))];
    const out: LibraryLabel[] = [];
    for (const key of keys) {
      const [namespace, value] = key.split("\u0000") as [LibraryLabelNamespace, string];
      const log = r.labels_log.filter((e) => e.namespace === namespace && e.value === value);
      const cleared = log.filter((e) => e.kind === "removed").at(-1)?.at;
      const adds = log.filter((e) => e.kind === "added" && (!cleared || e.at > cleared));
      if (!adds.length) continue;
      out.push({ namespace, value, state: adds.some((e) => e.by_person) ? "set" : "proposed", since: adds[0].at });
    }
    return out.sort((a, b) => a.namespace.localeCompare(b.namespace) || a.value.localeCompare(b.value));
  }
  // Removal and acceptance both read the newest paper event, as the broker does.
  const removal = (r: PaperRecord) => {
    const last = r.events.at(-1);
    return last?.kind === "removed" ? { removed_at: last.at, reason: last.reason ?? "", by: last.by } : null;
  };
  const acceptance = (r: PaperRecord) => {
    const last = r.events.at(-1);
    return last?.kind === "accepted" ? { accepted_at: last.at, by: last.by } : null;
  };
  const view = (r: PaperRecord): LibraryPaper => ({
    paper_id: r.paper_id, lab: r.lab, title: r.title, authors: r.authors, year: r.year, doi: r.doi, page_count: r.page_count,
    byte_size: r.byte_size, source_sha256: r.source_sha256, parse: r.parse,
    added_by: { name: r.added_by_name, kind: r.added_by_kind, you: r.added_by_id === me }, added_at: r.added_at,
    labels: labels(r), removed: removal(r), acceptance: acceptance(r),
    can_remove: r.lab === "open_methods" || r.added_by_id === me, can_accept: r.lab === "open_methods",
  });
  const refused = (json: Json, code: string, status: number) => json({ error: { code, message: "Refused" } }, status);
  const readBody = async (req: IncomingMessage, limit: number) => {
    const chunks: Buffer[] = [];
    let size = 0;
    for await (const chunk of req) { size += (chunk as Buffer).length; if (size > limit) return null; chunks.push(chunk as Buffer); }
    return Buffer.concat(chunks);
  };
  const jsonBody = async (req: IncomingMessage) => { const b = await readBody(req, 20000); return b?.length ? JSON.parse(b.toString()) : {}; };

  return {
    async handle(req: IncomingMessage, _res: ServerResponse, url: URL, json: Json, denied: () => void): Promise<boolean> {
      const m = url.pathname.match(/^\/api\/v1\/labs\/([a-z][a-z0-9_-]{1,63})\/library(\/.*)?$/);
      if (!m) return false;
      const [, lab, rest = ""] = m;
      if (!["open_methods", "field_trials"].includes(lab)) { refused(json, "lab_not_found", 404); return true; }
      const configured = lab === "open_methods";
      const mine = papers.filter((p) => p.lab === lab);
      if (rest === "" && req.method === "GET") {
        json({ lab, role: lab === "open_methods" ? "lead" : "researcher", configured,
          papers: mine.filter((p) => !removal(p)).sort((a, b) => b.added_at.localeCompare(a.added_at)).map(view),
          removed: mine.filter((p) => removal(p)).length });
        return true;
      }
      if (rest === "/search" && req.method === "GET") {
        if (!configured) { refused(json, "library_not_configured", 409); return true; }
        const q = (url.searchParams.get("q") ?? "").trim();
        if (q.length < 2 || q.length > 200) { refused(json, "library_query_invalid", 400); return true; }
        const words = q.toLowerCase().split(/\W+/).filter((w) => w.length > 1);
        const results = mine.filter((p) => !removal(p)).flatMap((p) => p.text
          .filter((t) => words.some((w) => t.body.toLowerCase().includes(w)))
          .slice(0, 2)
          .map((t) => ({ paper_id: p.paper_id, title: p.title, year: p.year, authors: p.authors.slice(0, 3), section: t.section,
            heading_path: [t.section], pages: t.pages, kind: "text", text: t.body })));
        json({ query: q, ranking: "words", results, left_out: 0 });
        return true;
      }
      if (rest === "/papers" && req.method === "POST") {
        const request = url.searchParams.get("request_id") ?? "";
        if (!uuid.test(request)) { refused(json, "library_invalid", 400); return true; }
        if (req.headers["content-type"] !== "application/pdf") { refused(json, "library_not_pdf", 400); return true; }
        const bytes = await readBody(req, 100 * 1024 * 1024);
        if (!bytes) { refused(json, "library_paper_too_large", 413); return true; }
        if (bytes.subarray(0, 5).toString("latin1") !== "%PDF-") { refused(json, "library_not_pdf", 400); return true; }
        const sha = createHash("sha256").update(bytes).digest("hex");
        const same = mine.find((p) => p.source_sha256 === sha);
        if (same) {
          if (removal(same)) {
            same.events.push({ kind: "restored", reason: null, at: new Date().toISOString(), by: "Example researcher", actor: me, request_id: request });
            json({ paper: view(same), created: false, restored: true });
          } else json({ paper: view(same), created: false, restored: false });
          return true;
        }
        if (!configured) { refused(json, "library_not_configured", 409); return true; }
        const losing = bytes.includes(Buffer.from("% lose-answer"));
        const record: PaperRecord = {
          paper_id: losing ? LOST_ANSWER_PAPER : randomUUID(), lab, lose_answers: losing, document: randomUUID(), title: "A synthetic paper added in the preview", authors: ["Preview Author"], year: 2026,
          doi: null, page_count: 2, byte_size: bytes.length, source_sha256: sha,
          parse: { state: "indexed", severity: "ok", chunks: 3, figures: 1, index: "chunks_only" },
          added_by_id: me, added_by_name: "Example researcher", added_by_kind: "person", added_at: new Date().toISOString(), request_id: request,
          labels_log: [], events: [], abstract: "An invented abstract for a paper added in the preview.",
          figures: [{ label: "Figure 1", caption: "An invented figure caption.", page: 2 }],
          text: [{ section: "Results", pages: "page 2", body: "The preview paper reports an invented result about breakfast timing." }],
        };
        papers.push(record);
        if (losing) { lostAnswer(_res); return true; }
        json({ paper: view(record), created: true, restored: false }, 201);
        return true;
      }
      const one = rest.match(/^\/papers\/([0-9a-f-]{36})(\/remove|\/labels|\/acceptance)?$/);
      if (!one) { denied(); return true; }
      const r = mine.find((p) => p.paper_id === one[1]);
      if (!r) { refused(json, "paper_not_found", 404); return true; }
      if (!one[2] && req.method === "GET") {
        json({ paper: view(r), outline: removal(r) ? null : { abstract: r.abstract, journal: "Journal of Invented Examples", figures: r.figures, figures_total: r.figures.length },
          outline_error: null });
        return true;
      }
      if (req.method !== "POST") { denied(); return true; }
      const input = await jsonBody(req);
      if (!uuid.test(input.request_id ?? "")) { refused(json, "library_invalid", 400); return true; }
      if (one[2] === "/remove") {
        const reason = typeof input.reason === "string" ? input.reason.trim() : "";
        if (!reason || reason.length > 600) { refused(json, "library_invalid", 400); return true; }
        if (removal(r)) { refused(json, "paper_removed", 409); return true; }
        r.events.push({ kind: "removed", reason, at: new Date().toISOString(), by: "Example researcher", actor: me, request_id: input.request_id });
        json({ paper: view(r) });
        return true;
      }
      if (one[2] === "/acceptance") {
        if (lab !== "open_methods") { refused(json, "library_forbidden", 403); return true; }
        const reason = typeof input.reason === "string" ? input.reason.trim() : "";
        if (typeof input.accept !== "boolean" || (input.accept && reason) || (!input.accept && (!reason || reason.length > 600))) {
          refused(json, "library_invalid", 400); return true;
        }
        if (removal(r)) { refused(json, "paper_removed", 409); return true; }
        if (Boolean(acceptance(r)) !== input.accept) {
          r.events.push({ kind: input.accept ? "accepted" : "acceptance_withdrawn", reason: input.accept ? null : reason,
            at: new Date().toISOString(), by: "Example researcher", actor: me, request_id: input.request_id });
        }
        if (r.lose_answers) { lostAnswer(_res); return true; }
        json({ paper: view(r) });
        return true;
      }
      const value = typeof input.value === "string" ? input.value.trim() : "";
      if (!["added", "removed"].includes(input.kind) || !namespaces.includes(input.namespace) || !value || value.length > 80) {
        refused(json, "label_invalid", 400); return true;
      }
      if (removal(r)) { refused(json, "paper_removed", 409); return true; }
      const current = labels(r).find((l) => l.namespace === input.namespace && l.value === value);
      if ((input.kind === "added" && current?.state === "set") || (input.kind === "removed" && !current)) { json({ paper: view(r) }); return true; }
      r.labels_log.push({ kind: input.kind, namespace: input.namespace, value, by_person: true, at: new Date().toISOString(), actor: me, request_id: input.request_id });
      json({ paper: view(r) });
      return true;
    },
  };
}
