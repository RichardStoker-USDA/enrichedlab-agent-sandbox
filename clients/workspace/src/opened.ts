// When this browser last opened each project. The landing rule reads it where the broker keeps no last look; it is a
// convenience only, never a record anyone else reads.
const KEY = "workspace_opened";
const KEEP = 200;

export function openedRecord(): Record<string, string> {
  try {
    const value = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    return Object.fromEntries(Object.entries(value).filter(([k, v]) => typeof k === "string" && typeof v === "string")) as Record<string, string>;
  } catch { return {}; }
}

export function markOpened(project: string, at = new Date().toISOString()) {
  try {
    const next = { ...openedRecord(), [project]: at };
    const kept = Object.entries(next).sort((a, b) => b[1].localeCompare(a[1])).slice(0, KEEP);
    localStorage.setItem(KEY, JSON.stringify(Object.fromEntries(kept)));
  } catch { /* a convenience only */ }
}
