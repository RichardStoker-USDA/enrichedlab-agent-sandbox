import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
	createDeferredRemoteBrokerClient,
	createRemoteBrokerClient,
	validateRemoteKey,
	validateRemoteUuid,
} from "../scripts/harness/remote_broker_core.mjs";
import { registerRemoteBrokerTools } from "../scripts/harness/pi_omp_remote_broker.mjs";

test("remote identifiers reject shell and path syntax", () => {
	assert.equal(validateRemoteKey("pilot-lab", "group"), "pilot-lab");
	assert.throws(() => validateRemoteKey("../pilot", "group"), /invalid/);
	assert.equal(
		validateRemoteUuid("00000000-0000-4000-8000-000000000000", "task"),
		"00000000-0000-4000-8000-000000000000",
	);
	assert.throws(() => validateRemoteUuid("task;rm", "task"), /UUID/);
});

test("remote submit uses a private objective file and exact argv", async () => {
	const calls = [];
	let observedObjective = "";
	const run = async (binary, args) => {
		calls.push({ binary, args: [...args] });
		const path = args[args.indexOf("--objective-file") + 1];
		observedObjective = await readFile(path, "utf8");
		return {
			stdout: JSON.stringify({ task_id: "00000000-0000-4000-8000-000000000000", status: "queued" }),
			stderr: "",
		};
	};
	const client = createRemoteBrokerClient({ enabled: true, binary: "/opt/bin/agent-remote", run });
	const result = await client.submit({
		group: "pilot-lab",
		profile: "general-research",
		objective: "Use a deterministic tool and retain the intermediate output.",
		idempotency_key: "qualification-1",
	});
	assert.equal(result.status, "queued");
	assert.equal(observedObjective, "Use a deterministic tool and retain the intermediate output.");
	assert.equal(calls[0].binary, "/opt/bin/agent-remote");
	assert.deepEqual(calls[0].args.slice(0, 5), [
		"submit",
		"--group",
		"pilot-lab",
		"--profile",
		"general-research",
	]);
	assert.equal(calls[0].args.includes(observedObjective), false);
});

test("remote tools stay disabled unless the launcher opts in", () => {
	assert.throws(() => createRemoteBrokerClient({ enabled: false }), /disabled/);
});

test("Pi can load remote tools before a launcher enables them", async () => {
	const tools = [];
	registerRemoteBrokerTools({ registerTool: (definition) => tools.push(definition) });
	assert.equal(tools.some((definition) => definition.name === "remote_agent_submit"), true);
	await assert.rejects(() => tools[0].execute("call", {}, undefined), /disabled/);
});

test("artifact downloads stay under the configured output root", async () => {
	const root = await mkdtemp(join(tmpdir(), "enrichedlab-remote-output-"));
	const task = "00000000-0000-4000-8000-000000000000";
	const artifact = "00000000-0000-4000-8000-000000000001";
	try {
		const client = createDeferredRemoteBrokerClient({
			enabled: true,
			binary: "/opt/bin/agent-remote",
			outputRoot: root,
			run: async (_binary, args) => {
				const output = args[args.indexOf("--output") + 1];
				await writeFile(output, "qualified\n", { mode: 0o600, flag: "wx" });
				return { stdout: "downloaded", stderr: "" };
			},
		});
		const result = await client.download({ task, artifact, file_name: "result.csv" });
		assert.equal(result.relative_output, `${task}/result.csv`);
		assert.equal(await readFile(join(root, result.relative_output), "utf8"), "qualified\n");
		await assert.rejects(
			() => client.download({ task, artifact, file_name: "../outside" }),
			/invalid/,
		);
	} finally {
		await rm(root, { recursive: true, force: true });
	}
});
