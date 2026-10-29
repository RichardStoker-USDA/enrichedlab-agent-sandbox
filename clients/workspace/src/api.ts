import type {
  Artifact,
  DocumentKind,
  FigureCatalog,
  FigureControls,
  FigurePreview,
  FigureVersion,
  FigureVersionSummary,
  GeneratedCode,
  Identity,
  Lineage,
  ManuscriptMark,
  PackageContents,
  RunDocument,
  PackageMember,
  ReviewDecision,
  ScientificHistoryEvidencePage,
  ScientificHistoryPage,
  StepLineage,
  Submission,
  Task,
  TaskEvent,
  TaskQuery,
  TaskReview,
} from "./contracts.ts";
import type { StreamHandlers } from "./follow.ts";
import { changedBySelf, keysFor } from "./live.ts";
import { closedFrom, pieceFrom, type LogClosed, type LogPiece } from "./live_log.ts";
import type { Page, PageQuery, Summary } from "./views/types.ts";

const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i;
export const MAX_BROWSER_DOWNLOAD = 32 * 1024 * 1024;
export class BrokerError extends Error {
  readonly code: string;
  readonly status: number;
  /** The broker's own fixed refusal code, such as `note_evidence_unaccepted`, when its error body names one. Only
   *  the code is kept; the body's message text is never shown. */
  readonly reason: string;
  /** For an upload the upload check held or refused: its finding codes and intake reference, each checked against a
   *  fixed shape. Like the reason, they are codes the page words itself; no message text is kept. */
  readonly findings: string[];
  readonly intake: string;
  /** Where a refused request body went wrong, when the broker names it (a lab preparation's `path` and `reason`): a
   *  place in the request's own fields and a fixed word, each checked against a fixed shape. */
  readonly place: { path: string; word: string } | null;
  constructor(code: string, status = 0, reason = "", findings: string[] = [], intake = "", place: { path: string; word: string } | null = null) {
    super(code);
    this.name = "BrokerError";
    this.code = code;
    this.status = status;
    this.reason = reason;
    this.findings = findings;
    this.intake = intake;
    this.place = place;
  }
}
const CODE = /^[a-z][a-z0-9_]{0,63}$/;
// A refusal code as the broker writes it (short lowercase words joined by underscores), with an upload check's
// finding codes and intake reference when the body carries them.
// A place in a request body: field names with list positions (`columns[1].type`), or an output name (`name:glucose`).
const PLACE = /^(?:[a-z_]+(?:\[\d{1,4}\])?(?:\.[a-z_]+(?:\[\d{1,4}\])?)*|name:[a-z][a-z0-9_]{0,62})$/;
async function refusal(response: Response): Promise<{ code: string; findings: string[]; intake: string; place: { path: string; word: string } | null }> {
  const none = { code: "", findings: [], intake: "", place: null };
  if (!response.headers.get("content-type")?.includes("application/json")) return none;
  try {
    const error = ((await response.json()) as { error?: { code?: unknown; findings?: unknown; intake?: unknown; path?: unknown; reason?: unknown } })?.error;
    const code = typeof error?.code === "string" && CODE.test(error.code) ? error.code : "";
    const findings = Array.isArray(error?.findings)
      ? error.findings.filter((f): f is string => typeof f === "string" && CODE.test(f)).slice(0, 32)
      : [];
    const intake = typeof error?.intake === "string" && UUID.test(error.intake) ? error.intake.toLowerCase() : "";
    const place = typeof error?.path === "string" && (error.path === "" || PLACE.test(error.path)) && typeof error?.reason === "string" && CODE.test(error.reason)
      ? { path: error.path, word: error.reason } : null;
    return { code, findings, intake, place };
  } catch {
    return none;
  }
}
const id = (value: string) => {
  if (!UUID.test(value)) throw new BrokerError("invalid_reference");
  return value;
};
const taskPath = (task: string) => `/api/v1/tasks/${id(task)}`;
const positiveInteger = (value: number) => {
  if (!Number.isSafeInteger(value) || value < 1)
    throw new BrokerError("invalid_reference");
  return String(value);
};

// Conditional reads: the last answer to each address stays in this tab's memory with the broker's ETag, a read sends
// If-None-Match, and 304 reuses the kept text. Nothing goes to the browser's disk cache (`no-store`); the memory holds
// at most KEPT_BYTES, and an answer larger than KEPT_ONE is not kept. A broker that sends no ETag keeps nothing here.
export const KEPT_BYTES = 8 * 1024 * 1024;
const KEPT_ONE = 1024 * 1024;
const kept = new Map<string, { etag: string; text: string }>();
let keptBytes = 0;
function drop(path: string) {
  const before = kept.get(path);
  if (before) { kept.delete(path); keptBytes -= before.text.length; }
}
function keep(path: string, etag: string, text: string) {
  drop(path);
  if (text.length > KEPT_ONE) return;
  kept.set(path, { etag, text });
  keptBytes += text.length;
  for (const [oldest, entry] of kept) {
    if (keptBytes <= KEPT_BYTES) break;
    kept.delete(oldest);
    keptBytes -= entry.text.length;
  }
}
/** Forgets every kept answer (signing out, a test). */
export function forgetKept() { kept.clear(); keptBytes = 0; }

export async function request<T>(
  path: string,
  init: RequestInit = {},
  expectEmpty = false,
): Promise<T> {
  // A fixed same-origin route keeps browser credentials away from caller-selected hosts.
  if (
    !path.startsWith("/api/v1/") ||
    path.includes("\\") ||
    path.includes("//")
  )
    throw new BrokerError("invalid_route");
  const method = (init.method ?? "GET").toUpperCase();
  const prior = method === "GET" && !expectEmpty ? kept.get(path) : undefined;
  let response: Response;
  try {
    response = await fetch(path, {
      ...init,
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal: init.signal ?? AbortSignal.timeout(15000),
      headers: { Accept: "application/json", ...(prior ? { "If-None-Match": prior.etag } : {}), ...init.headers },
    });
  } catch (error) {
    if (init.signal?.aborted) throw error;
    throw new BrokerError("connection_unavailable");
  }
  // What the reader changed reads again wherever it shows (live.ts), also after a conflict, which means the copy the
  // reader acted on was out of date.
  if (method !== "GET" && (response.ok || response.status === 409))
    changedBySelf(keysFor(method, path, init.body));
  if (response.status === 304 && prior) {
    if (kept.get(path) === prior) { kept.delete(path); kept.set(path, prior); }
    try { return JSON.parse(prior.text) as T; } catch { throw new BrokerError("invalid_response"); }
  }
  if (!response.ok) {
    const refused = await refusal(response);
    throw new BrokerError(
      response.status === 401 || response.status === 403
        ? "access_required"
        : response.status === 404
          ? "record_unavailable"
          : response.status === 409
            ? "request_conflict"
            : response.status === 429
              ? "capacity_reached"
              : "request_failed",
      response.status,
      refused.code,
      refused.findings,
      refused.intake,
      refused.place,
    );
  }
  if (expectEmpty) {
    if (response.status !== 204) throw new BrokerError("invalid_response");
    return undefined as T;
  }
  if (!response.headers.get("content-type")?.includes("application/json"))
    throw new BrokerError("invalid_response");
  try {
    const text = await response.text();
    const value = JSON.parse(text) as T;
    const etag = method === "GET" ? response.headers.get("etag") : null;
    if (etag) keep(path, etag, text); else if (method === "GET") drop(path);
    return value;
  } catch {
    throw new BrokerError("invalid_response");
  }
}

export const broker = {
  me: (signal?: AbortSignal) => request<Identity>("/api/v1/me", { signal }),
  tasks: (signal?: AbortSignal) =>
    request<{ tasks: Task[] }>("/api/v1/tasks?limit=200", { signal }),
  task: (task: string, signal?: AbortSignal) =>
    request<Task>(taskPath(task), { signal }),
  events: (task: string, after = 0, signal?: AbortSignal) =>
    request<{ events: TaskEvent[] }>(
      `${taskPath(task)}/events?after=${after}&limit=200`,
      { signal },
    ),
  // A table's server pages and column summaries. `path` names a package member; it
  // is left out for an artifact that is itself a table.
  pages: (task: string, artifact: string, path: string | null, query: PageQuery, signal?: AbortSignal) => {
    const params = new URLSearchParams({ ...(path === null ? {} : { path }), offset: String(query.offset), limit: String(query.limit),
      ...(query.sort ? { sort: query.sort, order: query.order ?? "asc" } : {}) });
    return request<Page>(`${taskPath(task)}/artifacts/${id(artifact)}/pages?${params}`, { signal });
  },
  summary: (task: string, artifact: string, path: string | null, signal?: AbortSignal) =>
    request<Summary>(`${taskPath(task)}/artifacts/${id(artifact)}/summary${path === null ? "" : `?${new URLSearchParams({ path })}`}`, { signal }),
  artifacts: (task: string, signal?: AbortSignal) =>
    request<{ artifacts: Artifact[] }>(`${taskPath(task)}/artifacts`, {
      signal,
    }),
  // Model-written code the broker admitted for this analysis; null when it ran a fixed method.
  generatedCode: async (task: string, signal?: AbortSignal) =>
    (await request<{ generated_code: GeneratedCode | null }>(`${taskPath(task)}/generated-code`, { signal })).generated_code,
  // The database query the agent wrote for this analysis, with its source and receipts; null when it ran none.
  taskQuery: async (task: string, signal?: AbortSignal) =>
    (await request<{ query: TaskQuery | null }>(`${taskPath(task)}/query`, { signal })).query,
  // The plot features the render service declares; null until the worker has read them.
  figureCatalog: async (signal?: AbortSignal) =>
    (await request<{ catalog: FigureCatalog | null }>("/api/v1/figures/catalog", { signal })).catalog,
  // A figure version's request, source and receipts; null when the task is not a figure version.
  taskFigure: async (task: string, signal?: AbortSignal) =>
    (await request<{ figure: FigureVersion | null }>(`${taskPath(task)}/figure`, { signal })).figure,
  // The figure versions made from one analysis, newest first.
  figureVersions: async (task: string, signal?: AbortSignal) =>
    (await request<{ versions: FigureVersionSummary[] }>(`${taskPath(task)}/figures`, { signal })).versions,
  // Asks for a document of a run's answer; the request id makes a repeated send create one.
  requestDocument: async (project: string, body: { run_id: string; kind: DocumentKind; request_id: string }) =>
    (await request<{ task_id: string }>(`/api/v1/projects/${id(project)}/documents`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" },
      body: JSON.stringify(body),
    })).task_id,
  // The documents made from one run, newest first, each with its newest receipt.
  runDocuments: async (run: string, signal?: AbortSignal) =>
    (await request<{ documents: RunDocument[] }>(`/api/v1/research-runs/${id(run)}/documents`, { signal })).documents,
  // Asks for a new figure version; the request id makes a repeated send create one version.
  requestFigure: async (project: string, body: { source_task: string; table_path: string; feature: string;
    controls: FigureControls; title: string; previous: string | null; request_id: string }) =>
    (await request<{ task_id: string }>(`/api/v1/projects/${id(project)}/figures`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" },
      body: JSON.stringify(body),
    })).task_id,
  // Asks for R's drawing of these settings without making a version. A compute worker draws it and the broker waits up
  // to 40 seconds; it keeps one waiting preview per person.
  previewFigure: (project: string, body: { source_task: string; table_path: string; feature: string; controls: FigureControls;
    title: string }, signal?: AbortSignal) =>
    request<FigurePreview>(`/api/v1/projects/${id(project)}/figures/previews`, {
      method: "POST", signal,
      headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" },
      body: JSON.stringify(body),
    }),
  // Marks one drawn version as the figure's version in the manuscript; a figure is named by its first version.
  markManuscript: (project: string, figure: string, version: string) =>
    request<ManuscriptMark>(`/api/v1/projects/${id(project)}/figures/${id(figure)}/manuscript`, {
      method: "PUT",
      headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" },
      body: JSON.stringify({ version }),
    }),
  clearManuscript: (project: string, figure: string) =>
    request<void>(`/api/v1/projects/${id(project)}/figures/${id(figure)}/manuscript`, {
      method: "DELETE", headers: { "X-Agent-Sandbox-Intent": "mutate" },
    }, true),
  // Who asked for this analysis and what it was made from, with every unrecorded part named.
  lineage: (task: string, signal?: AbortSignal) => request<Lineage>(`${taskPath(task)}/lineage`, { signal }),
  // One bounded read of the task and every readable recorded input back to terminal source snapshots.
  stepLineage: (task: string, signal?: AbortSignal) =>
    request<StepLineage>(`${taskPath(task)}/lineage?version=3`, { signal }),
  // Every person's review of this analysis, newest first.
  reviews: async (task: string, signal?: AbortSignal) =>
    (await request<{ reviews: TaskReview[] }>(`${taskPath(task)}/reviews`, { signal })).reviews,
  // Records the signed-in person's decision; the request id makes a repeated send save once.
  review: async (task: string, body: { decision: ReviewDecision; note: string | null; request_id: string }) =>
    (await request<{ review: TaskReview }>(`${taskPath(task)}/reviews`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" },
      body: JSON.stringify(body),
    })).review,
  scientificHistory: (task: string, cursor: string | null, signal?: AbortSignal) => {
    const query = new URLSearchParams({ limit: "100" });
    if (cursor) query.set("cursor", cursor);
    return request<ScientificHistoryPage>(
      `${taskPath(task)}/scientific-history?${query}`,
      { signal },
    );
  },
  scientificHistoryEvidence: (
    task: string,
    event: string,
    attempt: number,
    generation: number,
    after: string | null,
    signal?: AbortSignal,
  ) => {
    const query = new URLSearchParams({
      attempt: positiveInteger(attempt),
      generation: positiveInteger(generation),
      limit: "100",
    });
    if (after) query.set("after", id(after));
    return request<ScientificHistoryEvidencePage>(
      `${taskPath(task)}/scientific-history/events/${id(event)}/evidence?${query}`,
      { signal },
    );
  },
  submit: (submission: Submission, key: string) =>
    request<{ task_id: string; status: string }>("/api/v1/tasks", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Agent-Sandbox-Intent": "mutate",
        "Idempotency-Key": key,
      },
      body: JSON.stringify(submission),
    }),
  cancel: (task: string) =>
    request<{ task_id: string; status: string }>(`${taskPath(task)}/cancel`, {
      method: "POST",
      headers: { "X-Agent-Sandbox-Intent": "mutate" },
    }),
};

export function watchTask(
  task: string,
  after: number,
  callbacks: {
    event: (event: TaskEvent) => void;
    connected: () => void;
    disconnected: () => void;
    complete: () => void;
    unavailable: () => void;
  },
): () => void {
  let cursor = after,
    stopped = false,
    retries = 0,
    timer: ReturnType<typeof setTimeout> | undefined;
  let source: EventSource | undefined;
  const connect = () => {
    if (stopped) return;
    source = new EventSource(
      `${taskPath(task)}/events/stream?after=${cursor}&limit=200`,
    );
    source.onopen = () => {
      callbacks.connected();
    };
    source.addEventListener("task_event", (message) => {
      try {
        const event = JSON.parse((message as MessageEvent).data) as TaskEvent;
        if (
          !Number.isSafeInteger(event.sequence) ||
          event.sequence <= cursor ||
          typeof event.kind !== "string" ||
          typeof event.state !== "string" ||
          typeof event.code !== "string" ||
          typeof event.occurred_at !== "string"
        )
          return;
        cursor = event.sequence;
        retries = 0;
        callbacks.event(event);
      } catch {
        source?.close();
        stopped = true;
        callbacks.unavailable();
      }
    });
    source.addEventListener("stream_complete", () => {
      source?.close();
      stopped = true;
      callbacks.complete();
    });
    source.addEventListener("stream_closed", () => {
      source?.close();
      stopped = true;
      callbacks.unavailable();
    });
    source.onerror = () => {
      if (stopped) return;
      source?.close();
      callbacks.disconnected();
      if (++retries > 5) {
        stopped = true;
        callbacks.unavailable();
        return;
      }
      timer = setTimeout(connect, Math.min(1000 * 2 ** (retries - 1), 15000));
    };
  };
  connect();
  return () => {
    stopped = true;
    clearTimeout(timer);
    source?.close();
  };
}

// A stream the broker refuses (at most 64 open and 4 per person) or a moment's fault reaches the page
// only as a closed connection, so it is asked for again after a pause rather than in a tight loop.
const LOG_RETRY_PAUSES = [3000, 10000, 30000];

/** A running analysis's output as numbered pieces, from after piece `after`. The browser
 *  reconnects by itself after a dropped connection and resumes after the last piece's id, as it does when the broker
 *  ends a stream it could not read (`stream_closed`). A refused or failed connection is asked for again three times,
 *  after the last piece received. A piece or end that is not the named shape, a fourth refusal, or five reconnects in a
 *  row without a piece end the stream. */
export function watchLog(task: string, after: number, on: { open(): void; piece(piece: LogPiece): void; closed(closed: LogClosed): void; lost(): void; gone(): void }): () => void {
  let source: EventSource | null = null;
  let last = after, failures = 0, retries = 0, done = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const finish = (then?: () => void) => { if (done) return; done = true; clearTimeout(timer); source?.close(); then?.(); };
  const read = (message: Event): unknown => { try { return JSON.parse((message as MessageEvent).data); } catch { return null; } };
  const connect = () => {
    const s = new EventSource(`${taskPath(task)}/log/stream${last > 0 ? `?after=${last}` : ""}`);
    source = s;
    s.onopen = () => on.open();
    s.addEventListener("piece", (message) => {
      const piece = pieceFrom(read(message));
      if (!piece) { finish(on.gone); return; }
      failures = 0;
      retries = 0;
      last = Math.max(last, piece.seq);
      on.piece(piece);
    });
    s.addEventListener("closed", (message) => {
      const closed = closedFrom(read(message));
      finish(closed ? () => on.closed(closed) : on.gone);
    });
    s.addEventListener("stream_closed", () => { if (++failures > 5) finish(on.gone); else on.lost(); });
    s.onerror = () => {
      if (done || source !== s) return;
      if (s.readyState !== EventSource.CLOSED) {
        if (++failures > 5) finish(on.gone);
        else on.lost();
        return;
      }
      s.close();
      if (retries >= LOG_RETRY_PAUSES.length) { finish(on.gone); return; }
      on.lost();
      timer = setTimeout(() => { if (!done) connect(); }, LOG_RETRY_PAUSES[retries++]);
    };
  };
  connect();
  return () => finish();
}

const PULSE = /^[0-9a-f]{32}$/;
// A project's change stream (see follow.ts). Each event carries only a fingerprint and whether work runs; anything else
// ends the connection as an error, so the page falls back to reading on a timer rather than trusting a bad event.
export function watchProject(project: string, on: StreamHandlers): () => void {
  const source = new EventSource(`/api/v1/projects/${id(project)}/events/stream`);
  let done = false;
  const finish = (then: () => void) => {
    if (done) return;
    done = true;
    source.close();
    then();
  };
  const pulse = (message: Event, then: StreamHandlers["state"]) => {
    if (done) return;
    let value: unknown;
    try { value = JSON.parse((message as MessageEvent).data); } catch { value = null; }
    const v = value as { pulse?: unknown; busy?: unknown } | null;
    if (typeof v?.pulse !== "string" || !PULSE.test(v.pulse) || typeof v.busy !== "boolean") finish(on.error);
    else then({ pulse: v.pulse, busy: v.busy });
  };
  source.addEventListener("project_state", (m) => pulse(m, on.state));
  source.addEventListener("project_changed", (m) => pulse(m, on.changed));
  source.addEventListener("stream_complete", () => finish(on.renew));
  source.addEventListener("stream_closed", () => finish(on.closed));
  source.onerror = () => finish(on.error);
  return () => {
    done = true;
    source.close();
  };
}

export async function verifiedArtifact(
  task: string,
  artifact: Artifact,
  signal?: AbortSignal,
): Promise<Blob> {
  return fetchVerified(
    `${taskPath(task)}/artifacts/${id(artifact.id)}/download`,
    artifact.size_bytes,
    artifact.sha256,
    signal,
  );
}

// One file inside a retained package. The broker hashes the member from verified
// package bytes; the browser hashes it again before showing or saving it.
export async function verifiedMember(
  task: string,
  artifact: Artifact,
  member: PackageMember,
  signal?: AbortSignal,
): Promise<Blob> {
  const query = new URLSearchParams({ path: member.path });
  return fetchVerified(
    `${taskPath(task)}/artifacts/${id(artifact.id)}/contents/file?${query}`,
    member.size_bytes,
    member.sha256,
    signal,
  );
}

export async function packageContents(
  task: string,
  artifact: Artifact,
  signal?: AbortSignal,
): Promise<PackageContents> {
  return request<PackageContents>(
    `${taskPath(task)}/artifacts/${id(artifact.id)}/contents`,
    { signal },
  );
}

async function fetchVerified(
  path: string,
  size: number,
  sha256: string,
  signal?: AbortSignal,
): Promise<Blob> {
  if (!Number.isSafeInteger(size) || size < 0 || size > MAX_BROWSER_DOWNLOAD)
    throw new BrokerError("use_verified_cli");
  if (!/^[a-f0-9]{64}$/i.test(sha256))
    throw new BrokerError("artifact_integrity_failed");
  const response = await fetch(path, {
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(60000)]) : AbortSignal.timeout(60000),
  });
  if (!response.ok)
    throw new BrokerError(
      response.status === 403 || response.status === 404
        ? "record_unavailable"
        : "download_failed",
    );
  if (response.headers.get("x-artifact-sha256") !== sha256)
    throw new BrokerError("artifact_integrity_failed");
  const bytes = await readBody(response, size);
  await checkDigest(bytes, sha256);
  return new Blob([bytes], { type: "application/octet-stream" });
}

// Fetch exposes decoded bytes. A proxy may omit Content-Length or report the
// compressed transfer size, so the streamed count and hash always decide.
async function readBody(
  response: Response,
  size: number,
  progress?: (received: number) => void,
): Promise<Uint8Array<ArrayBuffer>> {
  const encoding = response.headers.get("content-encoding")?.trim().toLowerCase();
  const transferLength = response.headers.get("content-length");
  if ((!encoding || encoding === "identity") && transferLength !== null && transferLength !== String(size))
    throw new BrokerError("artifact_integrity_failed");
  const reader = response.body?.getReader();
  if (!reader) throw new BrokerError("download_failed");
  const bytes = new Uint8Array(new ArrayBuffer(size));
  let offset = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (offset + value.length > bytes.length)
        throw new BrokerError("artifact_integrity_failed");
      bytes.set(value, offset);
      offset += value.length;
      progress?.(offset);
    }
  } catch (error) {
    await reader.cancel();
    throw error instanceof BrokerError ? error : new BrokerError("download_failed");
  }
  if (offset !== bytes.length) throw new BrokerError("artifact_integrity_failed");
  return bytes;
}

export async function sha256Hex(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map((x) => x.toString(16).padStart(2, "0"))
    .join("");
}

async function checkDigest(bytes: Uint8Array<ArrayBuffer>, expected: string) {
  if ((await sha256Hex(bytes)) !== expected.toLowerCase())
    throw new BrokerError("artifact_integrity_failed");
}

// The browser verifies packages it can hold in memory. A larger package is refused
// here and goes through the operator's verified command-line download instead.
export const MAX_BROWSER_PACKAGE = 128 * 1024 * 1024;

export interface VerifiedPackage {
  blob: Blob;
  sha256: string;
  manifestSha256: string;
  filename: string;
  bytes: number;
}

export async function downloadProjectPackage(
  project: string,
  progress: (received: number, total: number) => void,
  signal?: AbortSignal,
): Promise<VerifiedPackage> {
  const response = await fetch(`/api/v1/projects/${id(project)}/package`, {
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(600000)]) : AbortSignal.timeout(600000),
  });
  if (!response.ok)
    throw new BrokerError(
      response.status === 403 || response.status === 404
        ? "record_unavailable"
        : response.status === 413
          ? "package_too_large"
          : response.status === 429
            ? "capacity_reached"
            : "download_failed",
      response.status,
    );
  const sha256 = response.headers.get("x-artifact-sha256") ?? "";
  const manifestSha256 = response.headers.get("x-evidence-manifest-sha256") ?? "";
  const size = Number(response.headers.get("content-length"));
  const disposition = response.headers.get("content-disposition") ?? "";
  const filename = /filename="([A-Za-z0-9._-]+)"/.exec(disposition)?.[1] ?? "research-evidence.tar.gz";
  if (!/^[a-f0-9]{64}$/.test(sha256) || !Number.isSafeInteger(size) || size <= 0) {
    await response.body?.cancel();
    throw new BrokerError("artifact_integrity_failed");
  }
  if (size > MAX_BROWSER_PACKAGE) {
    await response.body?.cancel();
    throw new BrokerError("use_direct_download");
  }
  const bytes = await readBody(response, size, (received) => progress(received, size));
  await checkDigest(bytes, sha256);
  return { blob: new Blob([bytes], { type: "application/gzip" }), sha256, manifestSha256, filename, bytes: size };
}
