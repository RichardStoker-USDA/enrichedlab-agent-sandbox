import { test } from "node:test";
import assert from "node:assert/strict";
import { matchesActionScope, performAction, type ActionScope } from "./actions.ts";

test("a delayed cancellation cannot overwrite a newly selected run", async () => {
  let scope: ActionScope = {
    principal: "researcher",
    revision: 0,
    taskId: "A",
  };
  let finish!: (value: string) => void;
  const committed: string[] = [];
  const action = performAction(
    () => scope,
    () =>
      new Promise<string>((resolve) => {
        finish = resolve;
      }),
    (value) => committed.push(value),
    true,
  );
  scope = { ...scope, taskId: "B" };
  finish("A cancelled");
  assert.equal(await action, false);
  assert.deepEqual(committed, []);
});

test("authentication refresh discards stale success and stale errors", async () => {
  let scope: ActionScope = {
    principal: "researcher",
    revision: 0,
    taskId: "A",
  };
  let finish!: (value: string) => void;
  let fail!: (error: Error) => void;
  const committed: string[] = [];
  const success = performAction(
    () => scope,
    () =>
      new Promise<string>((resolve) => {
        finish = resolve;
      }),
    (value) => committed.push(value),
  );
  const failure = performAction(
    () => scope,
    () =>
      new Promise<string>((_resolve, reject) => {
        fail = reject;
      }),
    (value) => committed.push(value),
  );
  scope = { principal: "another researcher", revision: 1, taskId: "B" };
  finish("prior account result");
  fail(new Error("prior account error"));
  assert.deepEqual(await Promise.all([success, failure]), [false, false]);
  assert.deepEqual(committed, []);
});

test("current actions commit once and expose current failures", async () => {
  const scope: ActionScope = {
    principal: "researcher",
    revision: 0,
    taskId: "A",
  };
  const committed: string[] = [];
  assert.equal(
    await performAction(
      () => scope,
      async () => "accepted",
      (value) => committed.push(value),
    ),
    true,
  );
  assert.deepEqual(committed, ["accepted"]);
  await assert.rejects(
    performAction(
      () => scope,
      async () => {
        throw new Error("current failure");
      },
      () => {},
    ),
    /current failure/,
  );
});

test("a revisioned task scope rejects a stale retrieval after reauthentication", () => {
  const prior: ActionScope = { principal: "researcher", revision: 4, taskId: "A" };
  assert.equal(matchesActionScope({ ...prior }, prior, true), true);
  assert.equal(matchesActionScope({ principal: "another researcher", revision: 5, taskId: "A" }, prior, true), false);
});
