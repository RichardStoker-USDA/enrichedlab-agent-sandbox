import { chooseLab } from "./context.ts";
import type { Identity } from "./contracts.ts";

// The lab a person is working in. Someone can belong to several labs; the choice only
// filters what the workspace shows and is kept in this browser. The broker still decides
// what each lab and project can open.
const KEY = "workspace_lab";

class LabChoice {
  key = $state("");

  settle(identity: Identity) {
    let stored = "";
    try { stored = localStorage.getItem(KEY) ?? ""; } catch { /* a convenience only */ }
    this.key = chooseLab(identity.groups.map((g) => g.key), this.key, stored);
  }

  set(key: string) {
    this.key = key;
    try { localStorage.setItem(KEY, key); } catch { /* a convenience only */ }
  }
}

export const lab = new LabChoice();

export function currentLab(identity: Identity | null) {
  return identity?.groups.find((g) => g.key === lab.key) ?? identity?.groups[0] ?? null;
}
