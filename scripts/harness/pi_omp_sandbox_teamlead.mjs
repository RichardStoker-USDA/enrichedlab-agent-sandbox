import { createControllerClient } from "./sandbox_teamlead_core.mjs";

const scalarSchema = {
	oneOf: [{ type: "string" }, { type: "number" }, { type: "boolean" }, { type: "null" }],
};

const nameProperty = {
	type: "string",
	pattern: "^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$",
	description: "Stable lowercase logical-agent name",
};

const definitions = (client) => [
	{
		name: "sandbox_teamlead_agent_create",
		label: "Create sandbox agent",
		description: "Register one governed logical agent without allocating compute.",
		promptSnippet: "Register a governed logical worker with a private inference route",
		promptGuidelines: [
			"Use sandbox_teamlead_agent_create before assigning work to a new disposable worker.",
		],
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["name", "domain", "state_profile", "objective", "role", "route", "harness"],
			properties: {
				name: nameProperty,
				domain: { type: "string", enum: ["cluster-ops", "scientific", "software-engineering"] },
				state_profile: {
					type: "string",
					enum: ["cluster-operator", "scientific-worker", "research-lead", "software-engineer"],
				},
				objective: { type: "string", minLength: 1, maxLength: 8192 },
				role: { type: "string", minLength: 1, maxLength: 1024 },
				route: nameProperty,
				harness: { type: "string", enum: ["open-code", "pi"] },
				thinking: { type: "string", enum: ["off", "low", "medium", "on", "xhigh"] },
			},
		},
		execute: (_id, params, signal) => toolResult(client.agentCreate(params, signal)),
	},
	{
		name: "sandbox_teamlead_agent_run_many",
		label: "Run sandbox agents concurrently",
		description:
			"Run one to four distinct logical agents concurrently, then release their disposable runtimes. Verify every acceptance file separately.",
		promptSnippet: "Run independent governed workers concurrently",
		promptGuidelines: [
			"Use distinct agents and non-overlapping output paths, then call sandbox_teamlead_output_verify for each run.",
		],
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["runs"],
			properties: {
				runs: {
					type: "array",
					minItems: 1,
					maxItems: 4,
					items: {
						type: "object",
						additionalProperties: false,
						required: ["name", "task"],
						properties: {
							name: nameProperty,
							task: { type: "string", minLength: 1, maxLength: 65536 },
							timeout_seconds: { type: "integer", minimum: 30, maximum: 86400 },
						},
					},
				},
			},
		},
		execute: (_id, params, signal) => toolResult(client.agentRunMany(params, signal)),
	},
	{
		name: "sandbox_teamlead_agent_run",
		label: "Run sandbox agent",
		description:
			"Run one fresh provider session, checkpoint state, and release compute. A passed process is not task acceptance; call sandbox_teamlead_output_verify afterward.",
		promptSnippet: "Run a governed logical worker and release its disposable runtime",
		promptGuidelines: [
			"After sandbox_teamlead_agent_run, always call sandbox_teamlead_output_verify for the task's acceptance file.",
		],
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["name", "task"],
			properties: {
				name: nameProperty,
				task: { type: "string", minLength: 1, maxLength: 65536 },
				timeout_seconds: { type: "integer", minimum: 30, maximum: 86400 },
			},
		},
		execute: (_id, params, signal) => toolResult(client.agentRun(params, signal)),
	},
	{
		name: "sandbox_teamlead_agent_status",
		label: "Inspect sandbox agent",
		description: "Read one logical agent's bounded lifecycle and runtime summary.",
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["name"],
			properties: { name: nameProperty },
		},
		execute: (_id, params, signal) => toolResult(client.agentStatus(params.name, signal)),
	},
	{
		name: "sandbox_teamlead_output_verify",
		label: "Verify sandbox output",
		description:
			"Verify a bounded regular JSON file inside one logical agent workspace against exact scalar acceptance fields.",
		promptSnippet: "Verify a worker acceptance file without granting host filesystem access",
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["name", "relative_path", "expected"],
			properties: {
				name: nameProperty,
				relative_path: { type: "string", minLength: 1, maxLength: 1024 },
				expected: {
					type: "object",
					minProperties: 1,
					maxProperties: 64,
					additionalProperties: scalarSchema,
				},
				allow_extra_fields: { type: "boolean" },
			},
		},
		execute: (_id, params, signal) => toolResult(client.outputVerify(params, signal)),
	},
	{
		name: "sandbox_teamlead_artifact_read",
		label: "Read sandbox artifact",
		description:
			"Read a bounded UTF-8 excerpt from one named artifact after the worker runtime has been released.",
		promptSnippet: "Review a worker-authored artifact without exposing its host workspace",
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["name", "relative_path"],
			properties: {
				name: nameProperty,
				relative_path: { type: "string", minLength: 1, maxLength: 1024 },
				start_line: { type: "integer", minimum: 1, maximum: 10000000 },
				max_lines: { type: "integer", minimum: 1, maximum: 400 },
			},
		},
		execute: (_id, params, signal) => toolResult(client.artifactRead(params, signal)),
	},
	{
		name: "sandbox_teamlead_request_list",
		label: "Review sandbox requests",
		description:
			"List bounded permission-request summaries for one active worker. The result marks every request that still requires a human decision.",
		promptSnippet: "Review a worker's pending permission requests without host shell access",
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["name"],
			properties: { name: nameProperty },
		},
		execute: (_id, params, signal) => toolResult(client.requestList(params, signal)),
	},
	{
		name: "sandbox_teamlead_request_approve",
		label: "Approve bounded sandbox request",
		description:
			"Approve an owner-scoped active-worker request only when the host launcher explicitly enabled its low-risk kind and, for domain read, its exact domain.",
		promptGuidelines: [
			"Call sandbox_teamlead_request_list first. Never claim approval when human_required is true.",
		],
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["name", "request_id", "justification"],
			properties: {
				name: nameProperty,
				request_id: { type: "string", format: "uuid" },
				justification: { type: "string", minLength: 1, maxLength: 512 },
			},
		},
		execute: (_id, params, signal) => toolResult(client.requestApprove(params, signal)),
	},
	{
		name: "sandbox_teamlead_request_reject",
		label: "Reject sandbox request",
		description: "Reject one owner-scoped active-worker request with a recorded reason.",
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["name", "request_id", "reason"],
			properties: {
				name: nameProperty,
				request_id: { type: "string", format: "uuid" },
				reason: { type: "string", minLength: 1, maxLength: 512 },
			},
		},
		execute: (_id, params, signal) => toolResult(client.requestReject(params, signal)),
	},
	{
		name: "sandbox_teamlead_agent_retire",
		label: "Retire sandbox agent",
		description: "Seal one reviewed logical agent and prevent another runtime allocation.",
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["name", "reason"],
			properties: {
				name: nameProperty,
				reason: { type: "string", minLength: 1, maxLength: 4096 },
			},
		},
		execute: (_id, params, signal) => toolResult(client.agentRetire(params, signal)),
	},
	{
		name: "sandbox_teamlead_cleanup_check",
		label: "Check sandbox cleanup",
		description: "Check sandbox counts, allocation reservations, state integrity, and the host audit chain.",
		parameters: {
			type: "object",
			additionalProperties: false,
			properties: { require_zero: { type: "boolean" } },
		},
		execute: (_id, params, signal) => toolResult(client.cleanupCheck(params, signal)),
	},
];

async function toolResult(promise) {
	const details = await promise;
	return { content: [{ type: "text", text: JSON.stringify(details) }], details };
}

export function registerSandboxTeamLeadTools(pi, client = createControllerClient()) {
	for (const definition of definitions(client)) pi.registerTool(definition);
}

export default function sandboxTeamLeadExtension(pi) {
	registerSandboxTeamLeadTools(pi);
}
