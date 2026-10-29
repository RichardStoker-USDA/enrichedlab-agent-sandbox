import { createDeferredRemoteBrokerClient } from "./remote_broker_core.mjs";

export function createOpenCodeRemoteBrokerTools(tool, client = createDeferredRemoteBrokerClient()) {
	const task = tool.schema.string().uuid();
	const key = tool.schema.string().regex(/^[a-z][a-z0-9_-]{1,63}$/);
	return {
		remote_agent_me: tool({
			description: "Read the verified remote identity, research groups, quotas, and profiles.",
			args: {},
			async execute(_args, context) {
				return JSON.stringify(await client.me(context?.abort));
			},
		}),
		remote_agent_submit: tool({
			description: "Submit one durable cluster task that continues after this local session closes.",
			args: {
				group: key,
				profile: key,
				objective: tool.schema.string().min(1).max(8192),
				idempotency_key: tool.schema.string().min(1).max(128).optional(),
			},
			async execute(args, context) {
				return JSON.stringify(await client.submit(args, context?.abort));
			},
		}),
		remote_agent_list: tool({
			description: "List durable remote tasks owned by the verified identity.",
			args: { limit: tool.schema.number().int().min(1).max(200).optional() },
			async execute(args, context) {
				return JSON.stringify(await client.list(args, context?.abort));
			},
		}),
		remote_agent_get: tool({
			description: "Read one remote task's status and result summary.",
			args: { task },
			async execute(args, context) {
				return JSON.stringify(await client.get(args, context?.abort));
			},
		}),
		remote_agent_cancel: tool({
			description: "Request bounded cancellation of one owned remote task.",
			args: { task },
			async execute(args, context) {
				return JSON.stringify(await client.cancel(args, context?.abort));
			},
		}),
		remote_agent_events: tool({
			description: "Read append-only task events after a known sequence number.",
			args: {
				task,
				after: tool.schema.number().int().min(0).optional(),
				limit: tool.schema.number().int().min(1).max(1000).optional(),
			},
			async execute(args, context) {
				return JSON.stringify(await client.events(args, context?.abort));
			},
		}),
		remote_agent_artifacts: tool({
			description: "List integrity-labeled artifacts available to the verified task owner.",
			args: { task },
			async execute(args, context) {
				return JSON.stringify(await client.artifacts(args, context?.abort));
			},
		}),
		remote_agent_download: tool({
			description: "Download one artifact into the launcher-controlled output root without overwriting files.",
			args: {
				task,
				artifact: task,
				file_name: tool.schema.string().regex(/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/),
			},
			async execute(args, context) {
				return JSON.stringify(await client.download(args, context?.abort));
			},
		}),
	};
}
