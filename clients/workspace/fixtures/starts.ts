// The labs' starting points in the preview: synthetic data and method sets, each labelled simulated. The nutrition lab
// registers three, so a question can start from one, several or every set (proposed broker shape); the
// food composition lab registers one and the field lab none. `combine` says the broker accepts several in one prepare.
export interface StartEntry {
  key: string; name: string; description: string; input_refs: string[]; tool_refs: string[]; allowed_profiles: string[];
}

const LABS: Record<string, StartEntry[]> = {
  open_methods: [
    { key: "open-methods-reference", name: "Simulated diet study with the reviewed summary method",
      description: "Simulated plasma responses and feature intensities for three diets, with one reviewed R method and the reviewed template for model-written R.",
      input_refs: ["input://synthetic/diet-study"], tool_refs: ["tool://reviewed/diet-response-summary"], allowed_profiles: ["evidence_review", "generated_r"] },
    { key: "open-methods-intake", name: "Simulated intake records with the reviewed intake summary",
      description: "Simulated three-day food records for the same participants, summarized into daily nutrient intakes by one reviewed R method.",
      input_refs: ["input://synthetic/intake-records"], tool_refs: ["tool://reviewed/intake-summary"], allowed_profiles: ["evidence_review"] },
    { key: "open-methods-body", name: "Simulated body composition scans",
      description: "Simulated lean and fat mass at the start and end of each diet, answered with checked, read-only queries.",
      input_refs: ["input://synthetic/body-composition"], tool_refs: ["tool://reviewed/composition-query"], allowed_profiles: ["evidence_review"] },
  ],
  food_composition: [
    { key: "example_food_composition", name: "Simulated food composition tables",
      description: "Simulated nutrient composition values for a handful of example foods, pinned to one snapshot. The agent answers from it with checked, read-only queries.",
      input_refs: ["input://synthetic/food-composition"], tool_refs: ["tool://reviewed/composition-query"], allowed_profiles: ["evidence_review"] },
  ],
};

export function startsFixture() {
  let combine = true;
  const of = (lab: string): StartEntry[] => LABS[lab] ?? [];
  return {
    get combine() { return combine; },
    /** Preview-only: a broker that takes one set per project, as the live broker did before this shape. */
    setCombine(on: boolean) { combine = on; },
    of,
    /** The list a lab's or a project's starting points answer with. */
    answer(lab: string) { return combine ? { workspaces: of(lab), combine: true } : { workspaces: of(lab) }; },
    /** The sets a prepare names, or the refusal code. One set as `workspace_key`; several as `workspace_keys`. */
    chosen(lab: string, input: { workspace_key?: unknown; workspace_keys?: unknown }): StartEntry[] | string {
      const offered = of(lab);
      if (typeof input.workspace_key === "string" && input.workspace_keys === undefined) {
        const one = offered.find((s) => s.key === input.workspace_key);
        return one ? [one] : "workspace_unavailable";
      }
      if (!combine || input.workspace_key !== undefined || !Array.isArray(input.workspace_keys)) return "invalid_request";
      const keys = input.workspace_keys;
      if (!keys.length || keys.length > offered.length || new Set(keys).size !== keys.length) return "invalid_request";
      const sets = keys.map((k) => offered.find((s) => s.key === k));
      return sets.every((s): s is StartEntry => !!s) ? sets : "workspace_unavailable";
    },
  };
}

/** The workspace a project records for the chosen sets: their data, methods and allowed methods together. */
export function workspaceFor(sets: StartEntry[], base: { schema_version: number; image_digest: string; source_digest: string }) {
  return {
    ...base, profile: sets[0].key, preparation_receipt: null,
    input_refs: [...new Set(sets.flatMap((s) => s.input_refs))], tool_refs: [...new Set(sets.flatMap((s) => s.tool_refs))],
    ...(sets.length > 1 ? { starting_points: sets.map((s) => ({ key: s.key, name: s.name })) } : {}),
  };
}
export const allowedFor = (sets: StartEntry[]) => [...new Set(sets.flatMap((s) => s.allowed_profiles))];
