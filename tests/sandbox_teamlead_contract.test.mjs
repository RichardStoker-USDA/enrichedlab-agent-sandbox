import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
	createControllerClient,
	parseControllerJson,
	validateAgentName,
	validateRelativeOutputPath,
} from "../scripts/harness/sandbox_teamlead_core.mjs";
import { registerSandboxTeamLeadTools } from "../scripts/harness/pi_omp_sandbox_teamlead.mjs";

function fixtureAgent(workspace) {
	return {
		name: "worker-one",
		lifecycle: "idle",
		active_sandbox: null,
		sessions_started: 1,
		sessions_completed: 1,
		failed_sessions: 0,
		last_result_class: "passed",
		state_profile: "software-engineer",
		harness: "pi",
		thinking: "on",
		workspace,
		owner: "qualification-lead",
		runtime: { route: "local-fast" },
	};
}

test("controller JSON parsing accepts one status prefix", () => {
	assert.deepEqual(parseControllerJson('Controller checks passed\n{"valid":true}'), { valid: true });
	assert.throws(() => parseControllerJson("not-json"), /complete JSON/);
});

test("names and output paths reject shell and traversal input", () => {
	assert.equal(validateAgentName("worker-one"), "worker-one");
	assert.throws(() => validateAgentName("worker;rm"), /DNS label/);
	assert.equal(validateRelativeOutputPath("qualification/result.json"), "qualification/result.json");
	assert.throws(() => validateRelativeOutputPath("../state.json"), /traversal/);
	assert.throws(() => validateRelativeOutputPath(".agent-state/view.json"), /hidden/);
});

test("typed client uses argv, omits assistant output, and verifies a regular JSON file", async (context) => {
	const root = await mkdtemp(join(tmpdir(), "enrichedlab-teamlead-"));
	context.after(() => rm(root, { recursive: true, force: true }));
	const workspace = join(root, "worker");
	await mkdir(join(workspace, "qualification"), { recursive: true });
	await writeFile(
		join(workspace, "qualification/result.json"),
		JSON.stringify({ harness: "pi", status: "passed" }),
	);
	const calls = [];
	const run = async (binary, args) => {
		calls.push({ binary, args });
		if (args[0] === "agent" && args[1] === "create") {
			return { code: 0, stdout: JSON.stringify(fixtureAgent(workspace)), stderr: "" };
		}
		if (args[0] === "agent" && args[1] === "status") {
			return { code: 0, stdout: JSON.stringify(fixtureAgent(workspace)), stderr: "" };
		}
		if (args[0] === "agent" && args[1] === "run") {
			return {
				code: 0,
				stdout: JSON.stringify({
					agent: fixtureAgent(workspace),
					sandbox: "runtime-one",
					segment: 1,
					attempts: 1,
					passed: true,
					result_class: "passed",
					runtime_released: true,
					assistant_output: "restricted model output",
				}),
				stderr: "",
			};
		}
		throw new Error(`unexpected test command: ${args.join(" ")}`);
	};
	const client = createControllerClient({
		binary: "/opt/test/asbx",
		enabled: true,
		owner: "qualification-lead",
		routes: ["local-fast"],
		run,
	});
	await client.agentCreate({
		name: "worker-one",
		domain: "software-engineering",
		state_profile: "software-engineer",
		objective: "Complete one bounded qualification.",
		role: "qualification worker",
		route: "local-fast",
		harness: "pi",
		thinking: "on",
	});
	assert.deepEqual(calls[0].args.slice(0, 3), ["agent", "create", "worker-one"]);
	assert.ok(!calls[0].args.some((argument) => argument.includes(";")));

	const runResult = await client.agentRun({
		name: "worker-one",
		task: "Write the acceptance file.\nThen verify it.",
	});
	assert.equal(runResult.passed, true);
	assert.equal(runResult.acceptance_verified, false);
	assert.equal(JSON.stringify(runResult).includes("restricted model output"), false);
	assert.equal(calls.at(-1).args.at(-1), "Write the acceptance file.\nThen verify it.");
	await assert.rejects(
		client.agentRun({ name: "worker-one", task: "invalid\0task" }),
		/non-text control/,
	);
	assert.equal(JSON.stringify(await client.agentStatus("worker-one")).includes(workspace), false);

	const verified = await client.outputVerify({
		name: "worker-one",
		relative_path: "qualification/result.json",
		expected: { harness: "pi", status: "passed" },
	});
	assert.equal(verified.acceptance_verified, true);
	assert.match(verified.sha256, /^[0-9a-f]{64}$/);
	const artifact = await client.artifactRead({
		name: "worker-one",
		relative_path: "qualification/result.json",
		max_lines: 10,
	});
	assert.equal(artifact.content, JSON.stringify({ harness: "pi", status: "passed" }));
	assert.equal(JSON.stringify(artifact).includes(workspace), false);

	await symlink(join(workspace, "qualification/result.json"), join(workspace, "qualification/link.json"));
	await assert.rejects(
		client.outputVerify({
			name: "worker-one",
			relative_path: "qualification/link.json",
			expected: { harness: "pi" },
			allow_extra_fields: true,
		}),
		/link/,
	);
});

test("typed client enforces owner scope and runs distinct agents concurrently", async () => {
	let active = 0;
	let maximum = 0;
	const run = async (_binary, args) => {
		const name = args[2];
		if (args[0] === "agent" && args[1] === "status") {
			return {
				code: 0,
				stdout: JSON.stringify({ ...fixtureAgent("/not-returned"), name, owner: name === "foreign" ? "other" : "lead" }),
				stderr: "",
			};
		}
		if (args[0] === "agent" && args[1] === "run") {
			active += 1;
			maximum = Math.max(maximum, active);
			await new Promise((resolve) => setTimeout(resolve, 10));
			active -= 1;
			return {
				code: 0,
				stdout: JSON.stringify({
					agent: { ...fixtureAgent("/not-returned"), name, owner: "lead" },
					sandbox: `${name}-runtime`,
					segment: 1,
					attempts: 1,
					passed: true,
					result_class: "passed",
					runtime_released: true,
				}),
				stderr: "",
			};
		}
		throw new Error("unexpected command");
	};
	const client = createControllerClient({
		binary: "/opt/test/asbx",
		enabled: true,
		owner: "lead",
		routes: ["local-fast"],
		run,
	});
	await assert.rejects(client.agentStatus("foreign"), /owner scope/);
	await assert.rejects(
		client.agentRunMany({ runs: [{ name: "first", task: "one" }, { name: "first", task: "two" }] }),
		/distinct/,
	);
	const result = await client.agentRunMany({
		runs: [{ name: "first", task: "one" }, { name: "second", task: "two" }],
	});
	assert.equal(maximum, 2);
	assert.equal(result.results.length, 2);
	assert.equal(result.results.every((item) => item.ok), true);
	assert.equal(result.acceptance_verified, false);
});

test("Team Lead request tools stay owner scoped and fail closed by request kind", async () => {
	const hostRequestId = "a1b2c3d4-1234-4abc-8def-0123456789ab";
	const domainRequestId = "b1b2c3d4-1234-4abc-8def-0123456789ab";
	const request = (requestId, kind, parameters) => ({
		schema_version: 1,
		sandbox: "sandbox-one",
		observed_at: "2026-09-01T12:00:00Z",
		source_sha256: "a".repeat(64),
		request: {
			schema_version: 1,
			request_id: requestId,
			reason: `Worker requests ${kind}`,
			created_at: "2026-09-01T12:00:00Z",
			kind,
			parameters,
		},
	});
	const hostRequest = request(hostRequestId, "host-service", {
		target_port: 8080,
		local_port: 0,
		ttl_minutes: 30,
		label: "review-ui",
	});
	const domainRequest = request(domainRequestId, "domain-read", { host: "unlisted.example.org" });
	const calls = [];
	const run = async (_binary, args) => {
		calls.push(args);
		if (args[0] === "agent" && args[1] === "status") {
			return {
				code: 0,
				stdout: JSON.stringify({
					...fixtureAgent("/not-returned"),
					owner: "lead",
					active_sandbox: "sandbox-one",
				}),
				stderr: "",
			};
		}
		if (args[0] === "request" && args[1] === "list") {
			return { code: 0, stdout: JSON.stringify([hostRequest, domainRequest]), stderr: "" };
		}
		if (args[0] === "request" && args[1] === "get") {
			return {
				code: 0,
				stdout: JSON.stringify(args[3] === hostRequestId ? hostRequest : domainRequest),
				stderr: "",
			};
		}
		if (args[0] === "request" && args[1] === "approve") {
			return {
				code: 0,
				stdout: JSON.stringify({
					request_id: hostRequestId,
					sandbox: "sandbox-one",
					decision: "approved",
					decided_at: "2026-09-01T12:01:00Z",
					decided_by: "team-lead-lead",
					reason: "Bounded loopback preview requested by the operator",
					applied_resource: "lease-one",
				}),
				stderr: "",
			};
		}
		if (args[0] === "request" && args[1] === "reject") {
			return {
				code: 0,
				stdout: JSON.stringify({
					request_id: domainRequestId,
					sandbox: "sandbox-one",
					decision: "rejected",
					decided_at: "2026-09-01T12:01:00Z",
					decided_by: "team-lead-lead",
					reason: "Domain is outside the assigned task",
					applied_resource: null,
				}),
				stderr: "",
			};
		}
		throw new Error(`unexpected command: ${args.join(" ")}`);
	};
	const client = createControllerClient({
		binary: "/opt/test/asbx",
		enabled: true,
		owner: "lead",
		routes: ["local-fast"],
		approvalKinds: ["host-service", "domain-read"],
		approvalDomains: ["packages.example.org"],
		run,
	});

	const listed = await client.requestList({ name: "worker-one" });
	assert.equal(listed.pending.length, 2);
	assert.equal(listed.pending[0].can_approve, true);
	assert.equal(listed.pending[1].human_required, true);
	assert.equal(JSON.stringify(listed).includes("host_lease_id"), false);
	await assert.rejects(
		client.requestApprove({
			name: "worker-one",
			request_id: domainRequestId,
			justification: "Allow package download",
		}),
		/requires human approval/,
	);
	const approved = await client.requestApprove({
		name: "worker-one",
		request_id: hostRequestId,
		justification: "Bounded loopback preview requested by the operator",
	});
	assert.equal(approved.decision, "approved");
	const approveCall = calls.find((args) => args[0] === "request" && args[1] === "approve");
	assert.deepEqual(approveCall.slice(4, 8), [
		"--reason",
		"Bounded loopback preview requested by the operator",
		"--actor",
		"team-lead-lead",
	]);
	const rejected = await client.requestReject({
		name: "worker-one",
		request_id: domainRequestId,
		reason: "Domain is outside the assigned task",
	});
	assert.equal(rejected.decision, "rejected");
});

test("typed client fails closed when the host launcher has not enabled it", async () => {
	const client = createControllerClient({ binary: "/opt/test/asbx" });
	await assert.rejects(client.cleanupCheck(), /not enabled/);
});

test("Pi-compatible extension registers only typed sandbox tools", () => {
	const names = [];
	registerSandboxTeamLeadTools(
		{ registerTool(definition) { names.push(definition.name); } },
		{},
	);
	assert.deepEqual(names.sort(), [
		"sandbox_teamlead_agent_create",
		"sandbox_teamlead_agent_retire",
		"sandbox_teamlead_agent_run",
		"sandbox_teamlead_agent_run_many",
		"sandbox_teamlead_agent_status",
		"sandbox_teamlead_artifact_read",
		"sandbox_teamlead_cleanup_check",
		"sandbox_teamlead_output_verify",
		"sandbox_teamlead_request_approve",
		"sandbox_teamlead_request_list",
		"sandbox_teamlead_request_reject",
	]);
});

test("OpenCode adapter receives its package API from the local loader", async () => {
	const source = await readFile(
		new URL("../scripts/harness/opencode_sandbox_teamlead.ts", import.meta.url),
		"utf8",
	);
	assert.match(source, /export function createOpenCodeSandboxTeamLeadTools\(tool\)/);
	assert.equal(source.includes("@opencode-ai/plugin"), false);
});
