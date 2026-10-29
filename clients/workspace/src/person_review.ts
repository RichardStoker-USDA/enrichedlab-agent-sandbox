import type { ReviewDecision, TaskReview } from "./contracts.ts";
import type { Tone } from "./language.ts";

// A person's review of an analysis in plain words. The claim stays narrow: a named person read the
// results and code and recorded a decision for the project. It is not statistical validation and changes no result.
export const MAX_REVIEW_NOTE = 2000;
export const reviewClaim =
  "A review records that a named person read the results and code and made a decision for this project. It is not statistical validation, and it never changes a result.";

export const decisionWords: Record<ReviewDecision, { label: string; tone: Tone; choice: string; hint: string }> = {
  accepted: { label: "Accepted", tone: "done", choice: "Accept", hint: "The results and code are right for this project." },
  needs_changes: { label: "Changes requested", tone: "attention", choice: "Needs changes", hint: "Say what should change before it can be accepted. This does not instruct the agent." },
  rejected: { label: "Rejected", tone: "failed", choice: "Reject", hint: "Say why these results should not be used." },
};
export const decisions: ReviewDecision[] = ["accepted", "needs_changes", "rejected"];

// Only a person's account records a review now; a row a service account recorded earlier is named as such and never
// marked like a person's decision.
export const byPerson = (review: TaskReview) => review.reviewer_kind !== "service_account";
export const notAPerson = "A service account recorded this, so it is not a person's review.";

export function reviewerName(review: TaskReview, reader: string): string {
  const name = review.reviewer_name?.trim();
  if (!byPerson(review)) return name ? `the service account ${name}` : "a service account";
  return review.reviewer_id === reader ? "you" : name || "a colleague";
}

// The mark shown beside an analysis: "Accepted by Reviewer One".
export const reviewMark = (review: TaskReview, reader: string) => ({
  tone: byPerson(review) ? decisionWords[review.decision].tone : ("quiet" as Tone),
  label: `${decisionWords[review.decision].label} by ${reviewerName(review, reader)}`,
});

// What the review covered: the analysis's result files as they are now, or an earlier set.
export function coverageSentence(review: TaskReview): string {
  const n = review.outputs.length;
  if (!n) return "The analysis had no result files when this review was recorded.";
  if (!review.covers_current_outputs) return "The result files changed after this review; it covered the earlier set.";
  return n === 1 ? "It covers the analysis's result file as it is now, by its checksum."
    : `It covers the analysis's ${n} result files as they are now, by their checksums.`;
}

// Why a review cannot be sent yet, or "" when it can. A note is required unless the analysis is accepted.
export function reviewProblem(decision: ReviewDecision | null, note: string, acceptable: boolean): string {
  if (!decision) return "Choose a decision.";
  if (decision === "accepted" && !acceptable) return "Only an analysis that finished with result files can be accepted.";
  const clean = note.trim();
  if (decision !== "accepted" && !clean)
    return decision === "needs_changes" ? "Say what should change." : "Say why these results should not be used.";
  if (clean.length > MAX_REVIEW_NOTE) return `Keep the note to ${MAX_REVIEW_NOTE.toLocaleString("en-US")} characters or fewer.`;
  return "";
}

// What people's reviews say about the analyses a finding reads: a rejection first, then a request for
// changes. A service account's row says nothing here.
export function findingFlag(tasks: string[], reviews: Record<string, TaskReview>, noun: "finding" | "table" = "finding"): { tone: Tone; label: string } | null {
  const decided = tasks.map((t) => reviews[t]).filter((r): r is TaskReview => !!r && byPerson(r));
  if (decided.some((r) => r.decision === "rejected")) return { tone: "failed", label: `A person rejected an analysis this ${noun} reads` };
  if (decided.some((r) => r.decision === "needs_changes")) return { tone: "attention", label: `A person asked for changes to an analysis this ${noun} reads` };
  return null;
}
