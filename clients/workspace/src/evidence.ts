import { broker, BrokerError, packageContents, verifiedMember } from "./api.ts";
import { isPackage, type Artifact, type PackageContents } from "./contracts.ts";
import { organize, type PackageView } from "./package_view.ts";

export interface RetainedPackage {
  artifact: Artifact;
  contents: PackageContents;
  view: PackageView;
}

export interface TaskEvidence {
  taskId: string;
  artifacts: Artifact[];
  packages: RetainedPackage[];
  // A package that could not be read stays downloadable; its reason is shown.
  unreadable: { artifact: Artifact; code: string }[];
}

// The retained files of one analysis, with every package opened by the broker. Only
// verified listings are used; nothing is inferred from file names alone.
export async function loadTaskEvidence(taskId: string, signal?: AbortSignal): Promise<TaskEvidence> {
  const { artifacts } = await broker.artifacts(taskId, signal);
  const packages: RetainedPackage[] = [];
  const unreadable: TaskEvidence["unreadable"] = [];
  for (const artifact of artifacts.filter(isPackage)) {
    try {
      const contents = await packageContents(taskId, artifact, signal);
      if (contents.sha256 !== artifact.sha256) throw new BrokerError("artifact_integrity_failed");
      packages.push({ artifact, contents, view: organize(contents.listing) });
    } catch (cause) {
      if (signal?.aborted) throw cause;
      unreadable.push({ artifact, code: cause instanceof BrokerError ? cause.code : "request_failed" });
    }
  }
  return { taskId, artifacts, packages, unreadable };
}

/** A figure version's drawing as R made it: its package's display file, checked against the recorded checksum, or null
 *  when the version holds none. */
export async function figureDrawing(task: string, signal?: AbortSignal): Promise<Blob | null> {
  const { packages } = await loadTaskEvidence(task, signal);
  const pkg = packages[0], display = pkg?.view.figures[0]?.display;
  if (!pkg || !display) return null;
  return new Blob([await verifiedMember(task, pkg.artifact, display, signal)], { type: display.media_type });
}

// Load several analyses without flooding the broker; results keep the input order.
export async function loadMany<T, R>(items: T[], limit: number, work: (item: T) => Promise<R>): Promise<R[]> {
  const results: R[] = new Array(items.length);
  let next = 0;
  const lanes = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const index = next++;
      results[index] = await work(items[index]);
    }
  });
  await Promise.all(lanes);
  return results;
}
