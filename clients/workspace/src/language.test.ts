import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { refusalWords } from "./language.ts";

// Six of the coordinator's refusal codes had no plain words, so the workspace showed
// the code. Every code the coordinator records for a refused reply (its `rejection_reason`) needs words here.
test("every refusal code the coordinator records has plain words", () => {
  // The coordinator is `coordinator.rs` and its child modules in `coordinator/`.
  const broker = join(import.meta.dirname, "../../../src/broker");
  const files = readdirSync(join(broker, "coordinator")).filter((name) => name.endsWith(".rs")).sort();
  const source = [join(broker, "coordinator.rs"), ...files.map((name) => join(broker, "coordinator", name))]
    .map((path) => readFileSync(path, "utf8"))
    .join("\n");
  const start = source.indexOf("pub fn rejection_reason(error: &str) -> &'static str {");
  assert.ok(start > 0, "rejection_reason not found");
  const body = source.slice(start, source.indexOf("\n}\n", start));
  const codes = [...body.matchAll(/=> "([a-z][a-z0-9_]+)"/g)].map((m) => m[1]);
  assert.ok(codes.length > 40, `found ${codes.length} codes`);
  const missing = [...new Set(codes)].filter((c) => refusalWords(c) === null);
  assert.deepEqual(missing, []);
  // The prefixes the function maps through other modules have words too.
  assert.ok(refusalWords("code_dynamic_call") && refusalWords("sql_refused_unordered"));
});

test("a methods check finding reads as a sentence naming the part, a step by its title", async () => {
  const { methodsCheckWords, reviewFindingLine } = await import("./language.ts");
  const actions = [{ action_key: "s-2", title: "Average sodium by food group" }];
  assert.equal(reviewFindingLine({ about: "finding 2", problem: "wrong_rows", detail: "It reads the second table." }, actions),
    "Finding 2: It reads the second table.");
  assert.equal(reviewFindingLine({ about: "step s-2", problem: "rows_dropped" }, actions),
    'The step "Average sodium by food group": leaves out rows the question needs.');
  assert.equal(reviewFindingLine({ about: "step gone", problem: "other", detail: "No such step." }, actions), "A step: No such step.");
  assert.equal(methodsCheckWords("revise"), "sent the proposal back for corrections");
  assert.equal(methodsCheckWords("proceed"), "agreed");
});

// The account button took its second letter from the address's domain.
test("the account button's letters come from the person's name, never from the address's domain", async () => {
  const { initials } = await import("./language.ts");
  assert.equal(initials({ display_name: null, email: "jqsample@example.org" }), "J");
  assert.equal(initials({ display_name: "Jordan Sample", email: "jqsample@example.org" }), "JS");
  assert.equal(initials({ display_name: "Ana María de la Cruz", email: "a@example.org" }), "AC");
  assert.equal(initials({ display_name: " ", email: "jane.q.doe@example.org" }), "JD");
  assert.equal(initials({ display_name: null, email: "" }), "");
});

test("a duration reads from recorded times, and an instant one is never 0 s", async () => {
  const { duration } = await import("./language.ts");
  assert.equal(duration(null), "");
  assert.equal(duration(-5), "");
  assert.equal(duration(0), "under 1 s");
  assert.equal(duration(420), "under 1 s");
  assert.equal(duration(1600), "2 s");
  assert.equal(duration(125_000), "2 min");
  assert.equal(duration(3_900_000), "1 h 5 min");
});

// A lab's page became "Your research" after a reload, because only memory held the lab.
test("a lab's page has an address that names only the lab's key", async () => {
  const { href, parseRoute } = await import("./route.ts");
  assert.equal(href({ name: "home", lab: "open_methods" }), "#/labs/open_methods");
  assert.deepEqual(parseRoute("#/labs/open_methods"), { name: "home", lab: "open_methods" });
  assert.deepEqual(parseRoute(href({ name: "home" })), { name: "home" });
  // Waiting for you in the labs a widened index showed; a malformed key opens home.
  assert.equal(href({ name: "waiting", labs: ["open_methods", "field_trials"] }), "#/waiting/labs/open_methods,field_trials");
  assert.deepEqual(parseRoute("#/waiting/labs/open_methods,field_trials"), { name: "waiting", labs: ["open_methods", "field_trials"] });
  assert.deepEqual(parseRoute("#/waiting/labs/open_methods,open_methods"), { name: "waiting", labs: ["open_methods"] });
  assert.deepEqual(parseRoute(href({ name: "waiting" })), { name: "waiting" });
  assert.deepEqual(parseRoute("#/waiting/labs/Open Methods"), { name: "home" });
  assert.deepEqual(parseRoute("#/waiting/labs/"), { name: "home" });
  for (const bad of ["#/labs/", "#/labs/Open Methods", "#/labs/a/b", "#/labs/%2e%2e", "#/labs/-x"])
    assert.deepEqual(parseRoute(bad), { name: "home" }, bad);
});
