import { createHash, randomUUID } from "node:crypto";
import { BUILT_IN } from "../src/kinds.ts";
import { answerTable } from "./tables.ts";
import { fixtureGeneratedCode, fixtureGeneratedPackage, fixtureMaterials, fixturePackage, fixtureQuery } from "./materials.ts";
import { fixtureDigest } from "./package.ts";
import { projectFixtures, type ProjectTask } from "./projects.ts";
import { datasetFixtures } from "./datasets.ts";
import { analysisFixtures } from "./analyses.ts";
import { figureFixtures } from "./figures.ts";
import { documentFixtures } from "./documents.ts";
import { libraryFixtures } from "./library.ts";
import { knowledgeFixtures } from "./knowledge.ts";
import { toolFixtures } from "./tools.ts";
import { longRunFixtures } from "./long_run.ts";
import { GENERIC_SCRIPT, serveLog } from "./log_stream.ts";
import { changeFeed } from "./changes.ts";
import { startsFixture } from "./starts.ts";
import { stepLineageFixture } from "./step_lineage.ts";
import type { Plugin } from "vite";
import type {
  Identity,
  ScientificHistoryEvent,
  Submission,
  Task,
  TaskEvent,
} from "../src/contracts.ts";

// Explicit loopback development mode only. This module is never imported by the browser bundle.
export function fixtureApi(): Plugin {
  const first = "10000000-0000-4000-8000-000000000001";
  const ids = [
    first,
    "10000000-0000-4000-8000-000000000002",
    "10000000-0000-4000-8000-000000000003",
    "10000000-0000-4000-8000-000000000004",
  ];
  const generatedTask = ids[3];
  const base = Date.now() - 45 * 60 * 1000,
    at = (seconds: number) => new Date(base + seconds * 1000).toISOString();
  const identity: Identity & { preview: string } = {
    preview: "synthetic",
    principal: {
      id: "20000000-0000-4000-8000-000000000001",
      email: "researcher@example.invalid",
      display_name: "Example researcher",
    },
    groups: [
      {
        key: "open_methods",
        name: "Example Nutrition Lab",
        // A lab lead may share the agent's notes with the lab.
        role: "lead",
        max_active_agents: 4,
        max_active_agents_per_principal: 2,
        max_queued_tasks: 8,
        retention_days: 30,
        // The lab's own methods (broker migration 052); the workspace offers a lab only these.
        profiles: ["evidence_review", "figure_render", "generated_r", "template_analysis", "lab_dataset"],
        // The lab holds the dataset method, so its projects take lab data (the per-lab signal cluster adds to /me).
        lab_data: true,
      },
      {
        key: "field_trials",
        name: "Example Field Lab",
        role: "viewer",
        max_active_agents: 2,
        max_active_agents_per_principal: 1,
        max_queued_tasks: 4,
        retention_days: 30,
        profiles: ["evidence_review"],
        lab_data: false,
      },
      {
        // A lab created today with no project yet: its page welcomes the reader with what it can work from.
        key: "food_composition",
        name: "Example Food Composition Lab",
        role: "lead",
        max_active_agents: 2,
        max_active_agents_per_principal: 2,
        max_queued_tasks: 8,
        retention_days: 365,
        profiles: ["evidence_review"],
      },
    ],
    // A synthetic organization; a site sets its own with the broker's --site-organization.
    site: { organization: "Example Research Center" },
    profiles: [
      {
        key: "evidence_review",
        name: "Evidence review",
        description:
          "Inspect approved inputs and retain each tool's original files.",
        tool_bundle: "reference_tools",
        max_run_seconds: 3600,
      },
      {
        key: "generated_r",
        name: "Model-written R analysis",
        description:
          "Runs one block of R written by the research model inside the reviewed template. Results are unvalidated.",
        tool_bundle: "reviewed_template",
        max_run_seconds: 300,
      },
      {
        // The figure method runs only from the figure editor, so it is listed for its name only.
        key: "figure_render",
        name: "Figure from a result table",
        description: "Draws one figure from a result table with the reviewed R render service.",
        tool_bundle: "render_service",
        max_run_seconds: 120,
        offered: false,
        in_runs: false,
      },
      {
        // A reviewed R analysis: a person asks for one from a result table, and a work
        // session may let the research lead choose one, so it is listed for runs but not offered to run directly.
        key: "template_analysis",
        name: "Reviewed R analyses",
        description: "Runs one of the render service's reviewed R analysis templates on a finished analysis's result table. No code is written for it.",
        tool_bundle: "render_service",
        max_run_seconds: 300,
        offered: false,
        in_runs: true,
      },
      {
        // Lab data runs only from a person's upload and approval, never in a work session.
        key: "lab_dataset",
        name: "Lab data",
        description: "Reads a table the lab keeps and writes the prepared table a person approves. No model reads a value.",
        tool_bundle: "dataset_executor",
        max_run_seconds: 300,
        offered: false,
        in_runs: false,
      },
    ],
    administration: { project_inspection: true },
  };
  const objectives = [
    "Summarize plasma response by diet and test feature changes",
    "Check the retained inputs of an earlier analysis",
    "Review feature changes before choosing follow-up measures",
    "Compare plasma response across the three diets with a model-written R analysis",
  ];
  const states = ["completed", "failed", "review_required", "completed"];
  const tasks: Task[] = ids.map((id, index) => ({
    id,
    group: "open_methods",
    profile: id === generatedTask ? "generated_r" : "evidence_review",
    objective: objectives[index],
    status: states[index],
    attempt: 1,
    max_task_attempts: 2,
    max_provider_attempts: 2,
    logical_agent: `reference_agent_${index + 1}`,
    result_class: index === 2 ? "human_review" : "success",
    error_code: null,
    submitted_at: at(-index * 420),
    started_at: at(10 - index * 420),
    updated_at: at(180 - index * 420),
    finished_at: at(180 - index * 420),
  }));
  // This review fixture is reachable only by its task address. It must not change shared task or project counts.
  const stepLineage = stepLineageFixture({ me: identity.principal.id, at });
  const histories = new Map<string, TaskEvent[]>(
    ids.map((id) => [
      id,
      [
        {
          sequence: 1,
          kind: "task",
          state: "queued",
          code: "task_accepted",
          occurred_at: at(0),
        },
        {
          sequence: 2,
          kind: "attempt",
          state: "running",
          code: "worker_lease_acquired",
          occurred_at: at(10),
        },
        {
          sequence: 3,
          kind: "attempt",
          state: "running",
          code: "runtime_ready",
          occurred_at: at(28),
        },
        {
          sequence: 4,
          kind: "artifact",
          state: "running",
          code: "evidence_collected",
          occurred_at: at(165),
        },
        {
          sequence: 5,
          kind: "task",
          state: tasks.find((t) => t.id === id)!.status,
          code:
            tasks.find((t) => t.id === id)!.status === "review_required"
              ? "review_required"
              : "task_completed",
          occurred_at: at(180),
        },
      ],
    ]),
  );
  const files = fixtureMaterials(first, at);
  // A long run in the field lab (Glass Box, "Long runs"): listed with the other projects, served by its own module.
  const freshLongRun = () => longRunFixtures({ me: identity.principal.id });
  let longRun = freshLongRun();
  const starts = startsFixture();
  // The step-lineage demo analysis is reachable by its address only; it stays out of the project's lists so the
  // project walks keep their counts and figure numbers.
  const freshProjects = () => projectFixtures({ me: identity.principal.id, firstTask: first, secondTask: ids[1], generatedTask, at,
    pkg: fixturePackage(Math.floor(base / 1000)), generated: fixtureGeneratedPackage(Math.floor(base / 1000)), also: () => longRun.summaries(), starts });
  let retained = freshProjects();
  // A person's lab data arrives in the nutrition lab's first project.
  const freshDatasets = () => datasetFixtures({ me: identity.principal.id, profile: "lab_dataset", group: "open_methods",
    project: "50000000-0000-4000-8000-000000000001", tasks, histories, addProjectTask: (entry) => retained.addTask(entry) });
  let datasets = freshDatasets();
  // Figures and reviewed R analyses read the first analysis's tables, the long run's comparison table and prepared lab
  // data; figures also read the results of finished analyses.
  const tables = () => [
    { project: "50000000-0000-4000-8000-000000000001", group: "open_methods", sourceTask: first,
      source: () => ({ files: retained.pkg.files, artifact: retained.artifacts[0] }),
      task: () => { const t = tasks.find((x) => x.id === first); return t ? { status: t.status, objective: t.objective ?? "" } : null; },
      addProjectTask: (entry: ProjectTask) => retained.addTask(entry) },
    longRun.figureSource(),
    ...datasets.prepared(),
  ];
  // Only the nutrition lab holds the template-analysis method.
  const freshAnalyses = () => analysisFixtures({ me: identity.principal.id, profile: "template_analysis", tasks, histories, sources: tables,
    knows: (task) => longRun.owns(task) || datasets.owns(task), groups: ["open_methods"] });
  let analyses = freshAnalyses();
  const freshFigures = () => figureFixtures({ me: identity.principal.id, profile: "figure_render", tasks, histories,
    knows: (task) => longRun.owns(task) || datasets.owns(task) || analyses.owns(task),
    sources: () => [...tables(), ...analyses.results()] });
  let figures = freshFigures();
  const documents = documentFixtures({ runs: () => retained.runs(), at });
  const freshLibrary = () => libraryFixtures({ me: identity.principal.id, at });
  let library = freshLibrary();
  const freshKnowledge = () => knowledgeFixtures({ at });
  let knowledge = freshKnowledge();
  const tools = toolFixtures({ at, analysis: null });
  const scientificSteps: ScientificHistoryEvent[] = Array.from(
    { length: 240 },
    (_, offset) => {
      const sequence = offset + 1;
      return {
        sequence,
        event_id: `40000000-0000-4000-8000-${String(sequence).padStart(12, "0")}`,
        segment: Math.floor(offset / 12) + 1,
        harness: "synthetic-observer",
        provider: null,
        model: null,
        engine: "synthetic",
        parser: "fixture",
        kind: sequence === 4 ? "agent.tool.failed" : "agent.tool.finished",
        evidence_role: "tool-io",
        visibility: "researcher",
        occurred_at: at(sequence),
        hash: sequence.toString(16).padStart(64, "0"),
        evidence_count: sequence <= 3 ? 1 : 0,
      };
    },
  );
  const submissions = new Map<string, { input: string; id: string }>();
  const timers = new Set<ReturnType<typeof setTimeout>>();
  // Conditional reads (proposed broker shape): each read's answer carries a weak ETag, and a read that sends
  // it back gets 304 with no body. The counts let the browser suite see the workspace reuse what it holds.
  let etags = true;
  const reads = { full: 0, notModified: 0 };
  let feed: ReturnType<typeof changeFeed> | null = null;
  return {
    name: "synthetic-broker-preview",
    apply: "serve",
    configureServer(server) {
      server.httpServer?.on("close", () => {
        for (const timer of timers) clearTimeout(timer);
        feed?.close();
      });
      const handle = async (req: import("node:http").IncomingMessage, res: import("node:http").ServerResponse, next: () => void) => {
        const url = new URL(req.url ?? "/", "http://localhost");
        if (!url.pathname.startsWith("/api/v1/")) return next();
        res.setHeader("Cache-Control", "no-store");
        res.setHeader("X-Workspace-Preview", "synthetic");
        const internal = req.headers["x-fixture-internal"] === "1";
        // A change the reader or a preview trigger makes is looked for as soon as its answer is sent.
        if (req.method !== "GET" && !internal) res.on("finish", () => { void feed?.tick(); });
        const json = (value: unknown, status = 200) => {
          const text = JSON.stringify(value);
          if (etags && req.method === "GET" && status === 200) {
            const tag = `W/"${createHash("sha256").update(text).digest("hex").slice(0, 32)}"`;
            res.setHeader("ETag", tag);
            res.setHeader("Cache-Control", "private, no-cache");
            if (req.headers["if-none-match"] === tag) {
              if (!internal) reads.notModified++;
              res.statusCode = 304;
              res.end();
              return;
            }
          }
          if (req.method === "GET" && !internal) reads.full++;
          res.statusCode = status;
          res.setHeader("Content-Type", "application/json");
          res.end(text);
        };
        const denied = () =>
          json(
            {
              error: {
                code: "record_unavailable",
                message: "Unavailable fixture reference",
              },
            },
            404,
          );
        if (
          req.method === "POST" &&
          req.headers["x-agent-sandbox-intent"] !== "mutate"
        )
          return json({ error: { code: "mutation_intent_required" } }, 403);
        if (req.method === "DELETE" && req.headers["x-agent-sandbox-intent"] !== "mutate")
          return json({ error: { code: "mutation_intent_required" } }, 403);
        if (url.pathname === "/api/v1/me") return json(feed!.on ? { ...identity, change_stream: true } : identity);
        // The reader's change stream (fixtures/changes.ts).
        if (url.pathname === "/api/v1/changes/stream" && req.method === "GET") return feed!.serve(req, res, url, json);
        // Preview-only: a broker without the change stream, ETags or combined starting points; and the stream's ends.
        if (url.pathname === "/api/v1/fixture/changes" && req.method === "POST") { feed!.setOn(url.searchParams.get("on") !== "false"); return json({ changes: url.searchParams.get("on") !== "false" }); }
        if (url.pathname === "/api/v1/fixture/changes-renew" && req.method === "POST") { feed!.renewAll(); return json({ renewed: true }); }
        if (url.pathname === "/api/v1/fixture/changes-drop" && req.method === "POST") { feed!.dropAll(); return json({ dropped: true }); }
        if (url.pathname === "/api/v1/fixture/etags" && req.method === "POST") { etags = url.searchParams.get("on") !== "false"; return json({ etags }); }
        if (url.pathname === "/api/v1/fixture/combine" && req.method === "POST") { starts.setCombine(url.searchParams.get("on") !== "false"); return json({ combine: starts.combine }); }
        if (url.pathname === "/api/v1/fixture/reads" && req.method === "GET") { res.setHeader("Cache-Control", "no-store"); const counts = { ...reads, streams: feed!.open }; res.statusCode = 200; res.setHeader("Content-Type", "application/json"); return res.end(JSON.stringify(counts)); }
        // The result-kind registry: the preview serves the built-in entries
        // under their own digest, so the client reads a registry the way it will from a broker.
        if (url.pathname === "/api/v1/kinds" && req.method === "GET") {
          const digest = `sha256:${createHash("sha256").update(JSON.stringify(BUILT_IN.kinds)).digest("hex")}`;
          res.setHeader("ETag", `"${digest}"`);
          return json({ registry: { digest, version: 1 }, kinds: BUILT_IN.kinds });
        }
        // A lab's starting points before its first project (the lab's welcome view).
        const labStarts = url.pathname.match(/^\/api\/v1\/labs\/([^/]+)\/workspaces$/);
        if (labStarts) {
          const key = decodeURIComponent(labStarts[1]);
          if (!identity.groups.some((g) => g.key === key)) return json({ error: { code: "lab_not_found" } }, 404);
          return json({ lab: key, ...starts.answer(key) });
        }
        // Preview-only: the browser suite starts each viewport from the same project state.
        if (url.pathname === "/api/v1/fixture/reset" && req.method === "POST") { starts.setCombine(true); etags = true; feed!.setOn(true); reads.full = 0; reads.notModified = 0; figures.reset(); documents.reset(); analyses.reset(); datasets.reset(); retained = freshProjects(); longRun = freshLongRun(); library = freshLibrary(); knowledge = freshKnowledge(); datasets = freshDatasets(); analyses = freshAnalyses(); figures = freshFigures(); return json({ reset: true }); }
        // Preview-only: the browser suite reads the commons as a lead and as a member.
        if (url.pathname === "/api/v1/fixture/commons-role" && req.method === "POST") {
          const role = url.searchParams.get("role");
          knowledge.setRole(role === "lead" || role === "member" || role === "curator" ? role : null); return json({ role });
        }
        // Preview-only: the next knowledge change is made but its answer is lost.
        if (url.pathname === "/api/v1/fixture/knowledge-drop-next" && req.method === "POST") { knowledge.dropNextAnswer(); return json({ drop: true }); }
        // Preview-only: a marked paper gains parts after its mark.
        if (url.pathname === "/api/v1/fixture/knowledge-parts-added" && req.method === "POST") return json({ added: knowledge.addParts() });
        if (url.pathname === "/api/v1/fixture/lab-role" && req.method === "POST") {
          const role = url.searchParams.get("role");
          knowledge.setLabRole(role === "lead" || role === "member" || role === "curator" ? role : null); return json({ role });
        }
        if (url.pathname === "/api/v1/fixture/configured" && req.method === "POST") { retained.configure(true); return json({ configured: true }); }
        // Preview-only: the sensor project's session stops for review while a page follows it.
        if (url.pathname === "/api/v1/fixture/sensor-run-stops" && req.method === "POST") { retained.stopSensorRun(); return json({ stopped: true }); }
        // Preview-only: the long run's model finishes and its next step starts while a page follows it.
        if (url.pathname === "/api/v1/fixture/long-run-advances" && req.method === "POST") { longRun.advance(); return json({ advanced: true }); }
        // Preview-only: the long run's residual check writes its output again from the first line.
        if (url.pathname === "/api/v1/fixture/long-run-log-restarts" && req.method === "POST") return json({ restarted: longRun.restartLog() });
        if (url.pathname === "/api/v1/fixture/query" && req.method === "POST") {
          const on = url.searchParams.get("on") !== "false";
          retained.queryMode(on, Math.floor(Date.parse(at(-20)) / 1000)); return json({ query: on });
        }
        if (await stepLineage.handle(req, res, url, json, denied)) return;
        // Lab data answers every project's dataset routes, the long run's project included, before that project's own.
        if (await datasets.handle(req, res, url, json, denied)) return;
        if (await analyses.handle(req, res, url, json, denied)) return;
        if (await longRun.handle(req, res, url, json, denied)) return;
        if (await library.handle(req, res, url, json, denied)) return;
        if (await knowledge.handle(req, res, url, json)) return;
        if (await tools.handle(req, res, url, json)) return;
        if (await documents.handle(req, res, url, json, denied)) return;
        if (await figures.handle(req, res, url, json, denied)) return;
        if (await retained.handle(req, res, url, json, denied)) return;
        // Any other analysis's live output: the reviewed method's opening lines while it runs; a finished one kept none.
        const logRoute = url.pathname.match(/^\/api\/v1\/tasks\/([0-9a-f-]{36})\/log\/stream$/);
        if (logRoute && req.method === "GET") {
          const t = tasks.find((x) => x.id === logRoute[1]);
          if (!t) return denied();
          return serveLog(req, res, { attempt: t.attempt || 1, ended: !["queued", "running"].includes(t.status),
            started: t.started_at ? Date.parse(t.started_at) : null, script: GENERIC_SCRIPT });
        }
        if (url.pathname === "/api/v1/tasks" && req.method === "GET")
          return json({ tasks });
        if (url.pathname === "/api/v1/tasks" && req.method === "POST") {
          let body = "";
          for await (const chunk of req) {
            body += chunk;
            if (body.length > 20000)
              return json({ error: { code: "request_too_large" } }, 413);
          }
          let input: Submission;
          try {
            input = JSON.parse(body);
          } catch {
            return json({ error: { code: "invalid_request" } }, 400);
          }
          const key = req.headers["idempotency-key"];
          if (
            typeof key !== "string" ||
            !key ||
            input.group !== "open_methods" ||
            input.profile !== "evidence_review" ||
            !input.objective?.trim() ||
            Buffer.byteLength(input.objective) > 8192
          )
            return json({ error: { code: "invalid_request" } }, 400);
          const previous = submissions.get(key);
          if (previous)
            return previous.input === body
              ? json({ task_id: previous.id, status: "queued" }, 202)
              : json({ error: { code: "request_conflict" } }, 409);
          const id = randomUUID(),
            now = new Date().toISOString();
          const task: Task = {
            ...tasks[0],
            ...input,
            id,
            status: "queued",
            logical_agent: null,
            attempt: 0,
            result_class: null,
            submitted_at: now,
            updated_at: now,
            started_at: null,
            finished_at: null,
          };
          tasks.unshift(task);
          histories.set(id, [
            {
              sequence: 1,
              kind: "task",
              state: "queued",
              code: "task_accepted",
              occurred_at: now,
            },
          ]);
          submissions.set(key, { input: body, id });
          for (const [delay, state, code] of [
            [1500, "running", "fixture_worker_started"],
            [5500, "completed", "fixture_completed"],
          ] as const) {
            const timer = setTimeout(() => {
              timers.delete(timer);
              if (task.status === "cancelled") return;
              const timestamp = new Date().toISOString();
              task.status = state;
              task.updated_at = timestamp;
              task.attempt = 1;
              task.logical_agent = "synthetic_preview_agent";
              if (state === "running") task.started_at = timestamp;
              else {
                task.finished_at = timestamp;
                task.result_class = "synthetic";
              }
              const events = histories.get(id)!;
              events.push({
                sequence: events.length + 1,
                kind: "task",
                state,
                code,
                occurred_at: timestamp,
              });
            }, delay);
            timers.add(timer);
          }
          return json({ task_id: id, status: "queued" }, 202);
        }
        const match = url.pathname.match(/^\/api\/v1\/tasks\/([^/]+)(.*)$/);
        if (!match) return denied();
        const task = tasks.find((t) => t.id === match[1]);
        if (!task) return denied();
        const suffix = match[2];
        if (!suffix) return json(task);
        if (suffix === "/reviews" && (req.method === "GET" || req.method === "POST")) return retained.taskReviews(task, req, json);
        if (suffix === "/cancel" && req.method === "POST") {
          if (
            !["completed", "failed", "cancelled", "review_required"].includes(
              task.status,
            )
          ) {
            task.status = "cancelled";
            task.finished_at = new Date().toISOString();
            const history = histories.get(task.id)!;
            history.push({
              sequence: history.length + 1,
              kind: "task",
              state: "cancelled",
              code: "task_cancelled",
              occurred_at: task.finished_at,
            });
          }
          return json({ task_id: task.id, status: task.status }, 202);
        }
        if (suffix === "/scientific-history") {
          if (task.id !== first)
            return json({ coverage: "pending", attempt: null, lease_generation: null, events: [], next_cursor: null });
          const cursor = url.searchParams.get("cursor");
          if (cursor === "stale")
            return json({ error: { code: "scientific_history_cursor_stale" } }, 409);
          const after = cursor ? Number(cursor) : 0;
          if (!Number.isInteger(after) || after < 0 || after > scientificSteps.length)
            return json({ error: { code: "scientific_history_cursor_invalid" } }, 400);
          const entries = scientificSteps.slice(after, after + Number(url.searchParams.get("limit") ?? 100));
          const next = after + entries.length;
          return json({ coverage: "available", attempt: 1, lease_generation: 1, events: entries, next_cursor: next < scientificSteps.length ? String(next) : null });
        }
        const evidence = suffix.match(/^\/scientific-history\/events\/([^/]+)\/evidence$/);
        if (evidence) {
          if (task.id !== first || url.searchParams.get("attempt") !== "1" || url.searchParams.get("generation") !== "1")
            return json({ error: { code: "scientific_history_cursor_stale" } }, 409);
          const step = scientificSteps.find((entry) => entry.event_id === evidence[1]);
          if (!step) return denied();
          const file = step.sequence <= 3 ? files[step.sequence - 1]?.artifact : undefined;
          return json({ attempt: 1, lease_generation: 1, evidence: file ? [{ artifact_id: file.id, sha256: file.sha256, size_bytes: file.size_bytes, media_type: file.media_type, role: file.role, relation: "observed_after" }] : [], next_cursor: null });
        }
        if (suffix === "/artifacts")
          return json({ artifacts: task.id === first ? [...retained.artifacts, ...files.map((file) => file.artifact)]
            : task.id === generatedTask ? retained.generatedArtifacts : [] });
        const digestOf = suffix.match(/^\/artifacts\/([0-9a-f-]{36})\/digest$/);
        if (digestOf) {
          const artifact = [...retained.artifacts, ...retained.generatedArtifacts]
            .find((a) => a.id === digestOf[1] && a.relative_path.startsWith(`tasks/${task.id}/`));
          if (!artifact) return denied();
          return json(artifact.id === retained.generatedArtifacts[0].id
            ? fixtureDigest(retained.generated, artifact.id, artifact.sha256, at(-20))
            : { artifact_id: artifact.id, artifact_sha256: artifact.sha256, status: "not_recorded", digest_sha256: null, recorded_at: null, digest: null });
        }
        if (suffix === "/generated-code")
          return json({ generated_code: task.id === generatedTask && !retained.query ? fixtureGeneratedCode(task.id, retained.runId, at(-60), retained.configured) : null });
        if (suffix === "/query")
          return json({ query: task.id === generatedTask && retained.query ? fixtureQuery(task.id, retained.runId, at(-60), retained.generated.sha256) : null });
        if (suffix === "/lineage") {
          const lineage: Record<string, unknown> = retained.lineage(task);
          if (task.id === generatedTask && !retained.query) {
            const code = fixtureGeneratedCode(task.id, retained.runId, at(-60), retained.configured);
            lineage.code = { path: code.path, sha256: code.sha256, size_bytes: code.size_bytes, admission: code.admission,
              configuration: code.configuration ?? null, run_id: code.run_id, turn: code.turn, created_at: code.created_at };
          }
          return json(lineage);
        }
        const tableRoute = suffix.match(/^\/artifacts\/([0-9a-f-]{36})\/(pages|summary)$/);
        const tableFile = tableRoute && task.id === first ? files.find(({ artifact }) => artifact.id === tableRoute[1]) : undefined;
        if (tableRoute && tableFile) return answerTable(tableRoute[2] as "pages" | "summary", url, null, { path: tableFile.artifact.relative_path, bytes: tableFile.bytes }, json);
        const file = files.find(({ artifact }) => suffix === `/artifacts/${artifact.id}/download`);
        if (file && task.id === first) {
          res.setHeader("Content-Type", file.artifact.media_type);
          res.setHeader("Content-Length", file.bytes.length);
          res.setHeader("X-Artifact-Sha256", file.artifact.sha256);
          res.setHeader("Content-Disposition", `attachment; filename="${file.artifact.relative_path.split("/").at(-1)}"`);
          return res.end(file.bytes);
        }
        if (suffix === "/events")
          return json({
            events: (histories.get(task.id) ?? [])
              .filter(
                (e) => e.sequence > Number(url.searchParams.get("after") ?? 0),
              )
              .slice(0, 200),
          });
        if (suffix === "/events/stream") {
          res.setHeader("Content-Type", "text/event-stream");
          res.setHeader("Connection", "keep-alive");
          res.flushHeaders();
          let cursor = Number(url.searchParams.get("after") ?? 0);
          const write = () => {
            for (const event of histories.get(task.id) ?? []) {
              if (event.sequence > cursor) {
                res.write(
                  `id: ${event.sequence}\nevent: task_event\ndata: ${JSON.stringify(event)}\n\n`,
                );
                cursor = event.sequence;
              }
            }
            if (
              ["completed", "failed", "cancelled", "review_required"].includes(
                task.status,
              )
            ) {
              res.write(
                `event: stream_complete\ndata: ${JSON.stringify({ status: task.status })}\n\n`,
              );
              res.end();
            }
          };
          const interval = setInterval(write, 500);
          res.on("close", () => clearInterval(interval));
          write();
          return;
        }
        return denied();
      };
      feed = changeFeed({ handle, labs: identity.groups.map((g) => g.key),
        projects: () => [...new Set([...retained.ids(), ...longRun.summaries().map((p) => p.project_id)])],
        tasks: () => tasks.map((t) => t.id), busy: (id) => retained.busy(id) ?? longRun.busy(id) });
      server.middlewares.use(handle);
    },
  };
}
