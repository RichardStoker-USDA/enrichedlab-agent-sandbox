import type { Submission } from "./contracts.ts";

export type DraftScope = {
  draft: Submission;
  requestKey: string | null;
  requestInput: string;
};

export const emptyDraftScope = (): DraftScope => ({
  draft: { group: "", profile: "", objective: "" },
  requestKey: null,
  requestInput: "",
});

export function draftForPrincipal(previousPrincipal: string | null, nextPrincipal: string, current: DraftScope): DraftScope {
  return previousPrincipal === nextPrincipal ? current : emptyDraftScope();
}
