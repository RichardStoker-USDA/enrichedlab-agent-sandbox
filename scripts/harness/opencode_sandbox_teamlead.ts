import { createControllerClient } from "./sandbox_teamlead_core.mjs";

export function createOpenCodeSandboxTeamLeadTools(tool) {
	const client = createControllerClient();
	const name = tool.schema
		.string()
		.regex(/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/)
		.describe("Stable lowercase logical-agent name");

	return {
		agent_create: tool({
			description: "Register one governed logical agent without allocating compute.",
			args: {
				name,
				domain: tool.schema.enum(["cluster-ops", "scientific", "software-engineering"]),
				state_profile: tool.schema.enum([
					"cluster-operator",
					"scientific-worker",
					"research-lead",
					"software-engineer",
				]),
				objective: tool.schema.string().min(1).max(8192),
				role: tool.schema.string().min(1).max(1024),
				route: name,
				harness: tool.schema.enum(["open-code", "pi"]),
				thinking: tool.schema.enum(["off", "low", "medium", "on", "xhigh"]).optional(),
			},
			async execute(args, context) {
				return JSON.stringify(await client.agentCreate(args, context?.abort));
			},
		}),

		agent_run: tool({
			description:
				"Run one fresh provider session and release compute. This proves transport only; verify the acceptance file next.",
			args: {
				name,
				task: tool.schema.string().min(1).max(65536),
				timeout_seconds: tool.schema.number().int().min(30).max(86400).optional(),
			},
			async execute(args, context) {
				return JSON.stringify(await client.agentRun(args, context?.abort));
			},
		}),

		agent_run_many: tool({
			description:
				"Run one to four distinct logical agents concurrently and release them. Verify every acceptance file separately.",
			args: {
				runs: tool.schema
					.array(
						tool.schema.object({
							name,
							task: tool.schema.string().min(1).max(65536),
							timeout_seconds: tool.schema.number().int().min(30).max(86400).optional(),
						}),
					)
					.min(1)
					.max(4),
			},
			async execute(args, context) {
				return JSON.stringify(await client.agentRunMany(args, context?.abort));
			},
		}),

		agent_status: tool({
			description: "Read one logical agent's bounded lifecycle and runtime summary.",
			args: { name },
			async execute(args, context) {
				return JSON.stringify(await client.agentStatus(args.name, context?.abort));
			},
		}),

		output_verify: tool({
			description: "Verify one bounded JSON acceptance file inside a logical agent workspace.",
			args: {
				name,
				relative_path: tool.schema.string().min(1).max(1024),
				expected: tool.schema.record(
					tool.schema.string(),
					tool.schema.union([
						tool.schema.string(),
						tool.schema.number(),
						tool.schema.boolean(),
						tool.schema.null(),
					]),
				),
				allow_extra_fields: tool.schema.boolean().optional(),
			},
			async execute(args, context) {
				return JSON.stringify(await client.outputVerify(args, context?.abort));
			},
		}),

		artifact_read: tool({
			description: "Read a bounded UTF-8 excerpt from one worker artifact after its runtime has been released.",
			args: {
				name,
				relative_path: tool.schema.string().min(1).max(1024),
				start_line: tool.schema.number().int().min(1).max(10000000).optional(),
				max_lines: tool.schema.number().int().min(1).max(400).optional(),
			},
			async execute(args, context) {
				return JSON.stringify(await client.artifactRead(args, context?.abort));
			},
		}),

		request_list: tool({
			description:
				"List bounded permission-request summaries for one active worker and mark requests that still require a human decision.",
			args: { name },
			async execute(args, context) {
				return JSON.stringify(await client.requestList(args, context?.abort));
			},
		}),

		request_approve: tool({
			description:
				"Approve an active-worker request only when the host launcher enabled its low-risk kind and exact domain policy.",
			args: {
				name,
				request_id: tool.schema.string().uuid(),
				justification: tool.schema.string().min(1).max(512),
			},
			async execute(args, context) {
				return JSON.stringify(await client.requestApprove(args, context?.abort));
			},
		}),

		request_reject: tool({
			description: "Reject one owner-scoped active-worker request with a recorded reason.",
			args: {
				name,
				request_id: tool.schema.string().uuid(),
				reason: tool.schema.string().min(1).max(512),
			},
			async execute(args, context) {
				return JSON.stringify(await client.requestReject(args, context?.abort));
			},
		}),

		agent_retire: tool({
			description: "Seal one reviewed logical agent and prevent another runtime allocation.",
			args: { name, reason: tool.schema.string().min(1).max(4096) },
			async execute(args, context) {
				return JSON.stringify(await client.agentRetire(args, context?.abort));
			},
		}),

		cleanup_check: tool({
			description: "Check sandbox counts, reservations, state integrity, and the host audit chain.",
			args: { require_zero: tool.schema.boolean().optional() },
			async execute(args, context) {
				return JSON.stringify(await client.cleanupCheck(args, context?.abort));
			},
		}),
	};
}
