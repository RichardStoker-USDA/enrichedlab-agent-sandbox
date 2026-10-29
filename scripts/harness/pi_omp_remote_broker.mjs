import { createDeferredRemoteBrokerClient } from "./remote_broker_core.mjs";

const definitions = (client) => [
	{
		name: "remote_agent_me",
		label: "Remote agent access",
		description: "Read the verified remote identity, research groups, quotas, and allowed profiles.",
		parameters: { type: "object", additionalProperties: false, properties: {} },
		execute: (_id, _params, signal) => result(client.me(signal)),
	},
	{
		name: "remote_agent_submit",
		label: "Submit remote agent",
		description: "Submit one durable cluster task. The remote worker continues after this local session closes.",
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["group", "profile", "objective"],
			properties: {
				group: { type: "string", pattern: "^[a-z][a-z0-9_-]{1,63}$" },
				profile: { type: "string", pattern: "^[a-z][a-z0-9_-]{1,63}$" },
				objective: { type: "string", minLength: 1, maxLength: 8192 },
				idempotency_key: { type: "string", minLength: 1, maxLength: 128 },
			},
		},
		execute: (_id, params, signal) => result(client.submit(params, signal)),
	},
	{
		name: "remote_agent_list",
		label: "List remote agents",
		description: "List durable tasks owned by the verified remote identity.",
		parameters: {
			type: "object",
			additionalProperties: false,
			properties: { limit: { type: "integer", minimum: 1, maximum: 200 } },
		},
		execute: (_id, params, signal) => result(client.list(params, signal)),
	},
	{
		name: "remote_agent_get",
		label: "Inspect remote agent",
		description: "Read one remote task's status and durable result summary.",
		parameters: taskParameters(),
		execute: (_id, params, signal) => result(client.get(params, signal)),
	},
	{
		name: "remote_agent_cancel",
		label: "Cancel remote agent",
		description: "Request bounded cancellation of one owned remote task.",
		parameters: taskParameters(),
		execute: (_id, params, signal) => result(client.cancel(params, signal)),
	},
	{
		name: "remote_agent_events",
		label: "Read remote agent events",
		description: "Read append-only task events after a known sequence number.",
		parameters: {
			...taskParameters(),
			properties: {
				...taskParameters().properties,
				after: { type: "integer", minimum: 0 },
				limit: { type: "integer", minimum: 1, maximum: 1000 },
			},
		},
		execute: (_id, params, signal) => result(client.events(params, signal)),
	},
	{
		name: "remote_agent_artifacts",
		label: "List remote artifacts",
		description: "List integrity-labeled artifacts available to the verified task owner.",
		parameters: taskParameters(),
		execute: (_id, params, signal) => result(client.artifacts(params, signal)),
	},
	{
		name: "remote_agent_download",
		label: "Download remote artifact",
		description: "Download one artifact into the launcher-controlled output root without overwriting files.",
		parameters: {
			type: "object",
			additionalProperties: false,
			required: ["task", "artifact", "file_name"],
			properties: {
				task: { type: "string", format: "uuid" },
				artifact: { type: "string", format: "uuid" },
				file_name: { type: "string", pattern: "^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$" },
			},
		},
		execute: (_id, params, signal) => result(client.download(params, signal)),
	},
];

function taskParameters() {
	return {
		type: "object",
		additionalProperties: false,
		required: ["task"],
		properties: { task: { type: "string", format: "uuid" } },
	};
}

async function result(promise) {
	const details = await promise;
	return { content: [{ type: "text", text: JSON.stringify(details) }], details };
}

export function registerRemoteBrokerTools(pi, client = createDeferredRemoteBrokerClient()) {
	for (const definition of definitions(client)) pi.registerTool(definition);
}

export default function remoteBrokerExtension(pi) {
	registerRemoteBrokerTools(pi);
}
