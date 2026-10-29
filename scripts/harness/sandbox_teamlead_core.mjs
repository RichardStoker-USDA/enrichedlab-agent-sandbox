import { createHash } from "node:crypto";
import { execFile as execFileCallback } from "node:child_process";
import { lstat, readFile, realpath } from "node:fs/promises";
import { isAbsolute, resolve, sep } from "node:path";
import { promisify } from "node:util";

const execFile = promisify(execFileCallback);
const JSON_OUTPUT_LIMIT = 8 * 1024 * 1024;
const ACCEPTANCE_FILE_LIMIT = 1024 * 1024;
const ARTIFACT_RETURN_LIMIT = 32 * 1024;
const NAME_PATTERN = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
const REQUEST_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu;
const DOMAIN_PATTERN = /^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/u;
const TEAMLEAD_APPROVABLE_KINDS = new Set(["host-service", "catalog-output", "domain-read"]);
const PROFILE_DOMAIN = new Map([
	["cluster-operator", "cluster-ops"],
	["scientific-worker", "scientific"],
	["research-lead", "scientific"],
	["software-engineer", "software-engineering"],
]);

function boundedString(value, label, maximum) {
	if (typeof value !== "string" || value.length === 0 || value.length > maximum) {
		throw new Error(`${label} must be a non-empty string no longer than ${maximum} characters`);
	}
	if (/[\0-\x1f\x7f]/u.test(value)) {
		throw new Error(`${label} contains a control character`);
	}
	return value;
}

function boundedText(value, label, maximum) {
	if (typeof value !== "string" || value.length === 0 || value.length > maximum) {
		throw new Error(`${label} must be a non-empty string no longer than ${maximum} characters`);
	}
	if (/[\0-\x08\x0b\x0c\x0e-\x1f\x7f]/u.test(value)) {
		throw new Error(`${label} contains a non-text control character`);
	}
	return value;
}

export function validateAgentName(value, label = "agent name") {
	boundedString(value, label, 63);
	if (!NAME_PATTERN.test(value)) {
		throw new Error(`${label} must be a lowercase DNS label`);
	}
	return value;
}

export function validateRelativeOutputPath(value) {
	boundedString(value, "output path", 1024);
	if (isAbsolute(value) || value.includes("\\")) {
		throw new Error("output path must be a relative POSIX path");
	}
	const components = value.split("/");
	if (
		components.some(
			(component) =>
				component.length === 0 ||
				component === "." ||
				component === ".." ||
				component.startsWith("."),
		)
	) {
		throw new Error("output path contains an empty, hidden, or traversal component");
	}
	return value;
}

export function parseControllerJson(value) {
	const text = String(value ?? "").trim();
	if (text.length === 0 || text.length > JSON_OUTPUT_LIMIT) {
		throw new Error("controller returned an empty or oversized JSON response");
	}
	try {
		return JSON.parse(text);
	} catch {
		for (let index = 0; index < text.length; index += 1) {
			if (text[index] !== "{" && text[index] !== "[") continue;
			try {
				return JSON.parse(text.slice(index));
			} catch {
				// Some controller commands prefix one bounded status line.
			}
		}
	}
	throw new Error("controller did not return one complete JSON document");
}

function controllerBinary(value) {
	const configured = value ?? process.env.ENRICHEDLAB_AGENT_SANDBOX_BIN ?? "agent-sandbox";
	boundedString(configured, "controller binary", 4096);
	if (configured.includes("/") && !isAbsolute(configured)) {
		throw new Error("controller binary with a path must be absolute");
	}
	return configured;
}

async function runProgram(binary, args, { signal, timeoutMs }) {
	try {
		const result = await execFile(binary, args, {
			signal,
			timeout: timeoutMs,
			maxBuffer: JSON_OUTPUT_LIMIT,
			encoding: "utf8",
			shell: false,
		});
		return { code: 0, stdout: result.stdout, stderr: result.stderr };
	} catch (error) {
		const code = typeof error?.code === "number" ? error.code : "unavailable";
		throw new Error(`controller command failed with exit code ${code}; inspect host evidence`);
	}
}

function agentSummary(agent) {
	if (!agent || typeof agent !== "object") throw new Error("controller response has no agent record");
	return {
		name: agent.name,
		lifecycle: agent.lifecycle,
		active_sandbox: agent.active_sandbox,
		sessions_started: agent.sessions_started,
		sessions_completed: agent.sessions_completed,
		failed_sessions: agent.failed_sessions,
		last_result_class: agent.last_result_class,
		state_profile: agent.state_profile,
		harness: agent.harness,
		thinking: agent.thinking,
		route: agent.runtime?.route,
	};
}

function validateCreateInput(input) {
	const name = validateAgentName(input.name);
	const domain = boundedString(input.domain, "domain", 64);
	const stateProfile = boundedString(input.state_profile, "state profile", 128);
	const expectedDomain = PROFILE_DOMAIN.get(stateProfile);
	if (!expectedDomain || expectedDomain !== domain) {
		throw new Error(`state profile ${stateProfile} does not match domain ${domain}`);
	}
	const route = validateAgentName(input.route, "route");
	const harness = input.harness ?? "open-code";
	if (!new Set(["open-code", "pi"]).has(harness)) throw new Error("unsupported worker harness");
	const thinking = input.thinking;
	if (thinking && !new Set(["off", "low", "medium", "on", "xhigh"]).has(thinking)) {
		throw new Error("unsupported thinking mode");
	}
	return {
		name,
		domain,
		stateProfile,
		objective: boundedText(input.objective, "objective", 8192),
		role: boundedText(input.role, "role", 1024),
		route,
		harness,
		thinking,
	};
}

function expectedObject(value) {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		throw new Error("expected JSON must be an object");
	}
	const entries = Object.entries(value);
	if (entries.length === 0 || entries.length > 64) {
		throw new Error("expected JSON must contain between 1 and 64 fields");
	}
	for (const [key, item] of entries) {
		boundedString(key, "expected JSON key", 128);
		if (item !== null && !["string", "number", "boolean"].includes(typeof item)) {
			throw new Error("expected JSON values must be scalar");
		}
		if (typeof item === "string") boundedText(item, `expected JSON field ${key}`, 4096);
		if (typeof item === "number" && !Number.isFinite(item)) {
			throw new Error("expected JSON numbers must be finite");
		}
	}
	return Object.fromEntries(entries);
}

function configuredRoutes(value, enabled) {
	if (!enabled) return null;
	const source = value ?? process.env.ENRICHEDLAB_TEAMLEAD_ROUTES;
	const routes = Array.isArray(source) ? source : String(source ?? "").split(",");
	const normalized = routes.map((route) => route.trim()).filter(Boolean).map((route) => validateAgentName(route, "route"));
	if (normalized.length === 0) throw new Error("Team Lead routes are not configured");
	return new Set(normalized);
}

function configuredApprovalKinds(value, enabled) {
	if (!enabled) return new Set();
	const source = value ?? process.env.ENRICHEDLAB_TEAMLEAD_APPROVAL_KINDS;
	const kinds = Array.isArray(source) ? source : String(source ?? "").split(",");
	const normalized = kinds.map((kind) => kind.trim()).filter(Boolean);
	for (const kind of normalized) {
		if (!TEAMLEAD_APPROVABLE_KINDS.has(kind)) {
			throw new Error(`unsupported Team Lead approval kind: ${kind}`);
		}
	}
	return new Set(normalized);
}

function configuredApprovalDomains(value, enabled) {
	if (!enabled) return new Set();
	const source = value ?? process.env.ENRICHEDLAB_TEAMLEAD_APPROVAL_DOMAINS;
	const domains = Array.isArray(source) ? source : String(source ?? "").split(",");
	const normalized = domains.map((domain) => domain.trim().toLowerCase()).filter(Boolean);
	for (const domain of normalized) {
		if (!DOMAIN_PATTERN.test(domain)) throw new Error(`invalid Team Lead approval domain: ${domain}`);
	}
	return new Set(normalized);
}

function validateRequestId(value) {
	boundedString(value, "request ID", 36);
	if (!REQUEST_ID_PATTERN.test(value)) throw new Error("request ID must be a UUID");
	return value.toLowerCase();
}

function normalizedRequest(value, expectedSandbox) {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		throw new Error("controller response has no normalized request");
	}
	if (value.sandbox !== expectedSandbox) throw new Error("request is outside the active sandbox scope");
	const request = value.request;
	if (!request || typeof request !== "object" || Array.isArray(request)) {
		throw new Error("controller response has no request body");
	}
	const requestId = validateRequestId(request.request_id);
	const kind = boundedString(request.kind, "request kind", 64);
	const reason = boundedText(request.reason, "request reason", 512);
	const parameters = request.parameters;
	if (!parameters || typeof parameters !== "object" || Array.isArray(parameters)) {
		throw new Error("controller response has no request parameters");
	}
	return { source: value, request, requestId, kind, reason, parameters };
}

function requestParameters(kind, parameters) {
	switch (kind) {
		case "host-service":
			return {
				target_port: parameters.target_port,
				local_port: parameters.local_port,
				ttl_minutes: parameters.ttl_minutes,
				label: parameters.label,
			};
		case "lan-service":
			return {
				bind: parameters.bind,
				local_port: parameters.local_port,
				sources: parameters.sources,
				clients: parameters.clients,
				ttl_minutes: parameters.ttl_minutes,
			};
		case "peer-service":
			return {
				target_sandbox: parameters.target_sandbox,
				target_port: parameters.target_port,
				source_port: parameters.source_port,
				ttl_minutes: parameters.ttl_minutes,
			};
		case "domain-read":
		case "domain-write":
			return { host: parameters.host };
		case "mount":
			return { read_only: parameters.read_only, host_path_withheld: true };
		case "catalog-output":
			return {
				name: parameters.name,
				artifact_kind: parameters.artifact_kind,
				workspace_path: parameters.workspace_path,
				entrypoint: parameters.entrypoint,
			};
		default:
			return { details_withheld: true };
	}
}

async function readBoundedRegularFile(workspacePath, relativePath, maximumSize) {
	let workspace;
	try {
		workspace = await realpath(boundedString(workspacePath, "agent workspace", 4096));
	} catch {
		throw new Error("agent workspace is unavailable");
	}
	const target = resolve(workspace, relativePath);
	if (!target.startsWith(`${workspace}${sep}`)) throw new Error("output path escapes the agent workspace");

	let metadata;
	try {
		metadata = await lstat(target);
	} catch {
		throw new Error("acceptance output is missing or unreadable");
	}
	if (
		!metadata.isFile() ||
		metadata.isSymbolicLink() ||
		metadata.nlink !== 1 ||
		metadata.size > maximumSize
	) {
		throw new Error("acceptance output must be a bounded regular file, not a link");
	}

	let canonicalTarget;
	try {
		canonicalTarget = await realpath(target);
	} catch {
		throw new Error("acceptance output is missing or unreadable");
	}
	if (!canonicalTarget.startsWith(`${workspace}${sep}`)) {
		throw new Error("acceptance output resolves outside the agent workspace");
	}

	try {
		return await readFile(canonicalTarget);
	} catch {
		throw new Error("acceptance output is missing or unreadable");
	}
}

export function createControllerClient(options = {}) {
	const enabled = options.enabled ?? process.env.ENRICHEDLAB_TEAMLEAD_ENABLED === "1";
	const binary = controllerBinary(options.binary);
	const runner = options.run ?? runProgram;
	const owner = validateAgentName(
		options.owner ?? (enabled ? process.env.ENRICHEDLAB_TEAMLEAD_OWNER : "disabled-team-lead"),
		"Team Lead owner",
	);
	const allowedRoutes = configuredRoutes(options.routes, enabled);
	const allowedApprovalKinds = configuredApprovalKinds(options.approvalKinds, enabled);
	const allowedApprovalDomains = configuredApprovalDomains(options.approvalDomains, enabled);
	const decisionActor = `team-lead-${owner}`;

	function assertEnabled() {
		if (!enabled) throw new Error("Team Lead controller tools are not enabled for this process");
	}

	function assertAllowedRoute(route) {
		if (!allowedRoutes?.has(route)) throw new Error("inference route is outside this Team Lead scope");
	}

	async function call(args, { signal, timeoutMs = 30_000 } = {}) {
		const result = await runner(binary, args, { signal, timeoutMs });
		if (result?.code !== 0) {
			throw new Error(`controller command failed with exit code ${result?.code ?? "unavailable"}; inspect host evidence`);
		}
		return parseControllerJson(result.stdout);
	}

	async function rawAgentStatus(name, signal) {
		assertEnabled();
		const record = await call(["agent", "status", validateAgentName(name), "--output", "json"], { signal });
		if (record.owner !== owner) throw new Error("logical agent is outside this Team Lead owner scope");
		assertAllowedRoute(record.runtime?.route);
		return record;
	}

	async function agentStatus(name, signal) {
		return agentSummary(await rawAgentStatus(name, signal));
	}

	async function activeSandbox(name, signal) {
		const record = await rawAgentStatus(name, signal);
		return { record, sandbox: record.active_sandbox };
	}

	function approvalEligibility(kind, parameters) {
		if (!allowedApprovalKinds.has(kind)) {
			return { can_approve: false, human_required: true, policy_reason: "request kind is not enabled for Team Lead approval" };
		}
		if (kind === "domain-read") {
			const host = typeof parameters.host === "string" ? parameters.host.toLowerCase() : "";
			if (!allowedApprovalDomains.has(host)) {
				return { can_approve: false, human_required: true, policy_reason: "domain is not in the exact Team Lead read allowlist" };
			}
		}
		return { can_approve: true, human_required: false, policy_reason: "request matches the configured Team Lead approval policy" };
	}

	function requestSummary(value, sandbox) {
		const normalized = normalizedRequest(value, sandbox);
		return {
			sandbox,
			request_id: normalized.requestId,
			observed_at: normalized.source.observed_at,
			source_sha256: normalized.source.source_sha256,
			kind: normalized.kind,
			reason: normalized.reason,
			parameters: requestParameters(normalized.kind, normalized.parameters),
			...approvalEligibility(normalized.kind, normalized.parameters),
		};
	}

	function decisionSummary(value) {
		if (!value || typeof value !== "object" || Array.isArray(value)) {
			throw new Error("controller response has no request decision");
		}
		return {
			request_id: validateRequestId(value.request_id),
			sandbox: value.sandbox,
			decision: value.decision,
			decided_at: value.decided_at,
			decided_by: value.decided_by,
			reason: value.reason,
			applied_resource: value.applied_resource,
		};
	}

	async function runAgent(input, signal) {
		const name = validateAgentName(input.name);
		const task = boundedText(input.task, "task", 65_536);
		const timeoutSeconds = input.timeout_seconds ?? 3600;
		if (!Number.isInteger(timeoutSeconds) || timeoutSeconds < 30 || timeoutSeconds > 86_400) {
			throw new Error("attempt timeout must be an integer from 30 through 86400 seconds");
		}
		await rawAgentStatus(name, signal);
		const response = await call(
			[
				"agent",
				"run",
				name,
				"--attempt-timeout-seconds",
				String(timeoutSeconds),
				"--output",
				"json",
				"--",
				task,
			],
			{ signal, timeoutMs: (timeoutSeconds + 120) * 1000 },
		);
		return {
			agent: agentSummary(response.agent),
			sandbox: response.sandbox,
			segment: response.segment,
			attempts: response.attempts,
			passed: response.passed,
			result_class: response.result_class,
			runtime_released: response.runtime_released,
			acceptance_verified: false,
		};
	}

	return {
		async agentCreate(input, signal) {
			assertEnabled();
			const value = validateCreateInput(input);
			assertAllowedRoute(value.route);
			const args = [
				"agent",
				"create",
				value.name,
				"--owner",
				owner,
				"--domain",
				value.domain,
				"--state-profile",
				value.stateProfile,
				"--objective",
				value.objective,
				"--role",
				value.role,
				"--route",
				value.route,
				"--harness",
				value.harness,
			];
			if (value.thinking) args.push("--thinking", value.thinking);
			args.push("--output", "json");
			return agentSummary(await call(args, { signal }));
		},

		agentRun: runAgent,

		async agentRunMany(input, signal) {
			if (!Array.isArray(input.runs) || input.runs.length < 1 || input.runs.length > 4) {
				throw new Error("runs must contain between one and four tasks");
			}
			if (input.runs.some((run) => !run || typeof run !== "object" || Array.isArray(run))) {
				throw new Error("each concurrent run must be an object");
			}
			const names = input.runs.map((run) => validateAgentName(run.name));
			if (new Set(names).size !== names.length) throw new Error("concurrent runs must use distinct logical agents");
			const settled = await Promise.allSettled(input.runs.map((run) => runAgent(run, signal)));
			return {
				results: settled.map((result, index) =>
					result.status === "fulfilled"
						? { name: names[index], ok: true, result: result.value }
						: { name: names[index], ok: false, error: "worker run failed; inspect host evidence" },
				),
				acceptance_verified: false,
			};
		},

		agentStatus,

		async requestList(input, signal) {
			const name = validateAgentName(input.name);
			const { sandbox } = await activeSandbox(name, signal);
			if (!sandbox) {
				return { name, active: false, pending: [] };
			}
			validateAgentName(sandbox, "active sandbox");
			const pending = await call(["request", "list", sandbox, "--output", "json"], { signal });
			if (!Array.isArray(pending)) throw new Error("controller response has no request list");
			return {
				name,
				active: true,
				pending: pending.map((request) => requestSummary(request, sandbox)),
			};
		},

		async requestApprove(input, signal) {
			const name = validateAgentName(input.name);
			const requestId = validateRequestId(input.request_id);
			const justification = boundedText(input.justification, "approval justification", 512);
			const { sandbox } = await activeSandbox(name, signal);
			if (!sandbox) throw new Error("logical agent has no active sandbox runtime");
			validateAgentName(sandbox, "active sandbox");
			const request = await call(["request", "get", sandbox, requestId, "--output", "json"], { signal });
			const summary = requestSummary(request, sandbox);
			if (!summary.can_approve) throw new Error(`request requires human approval: ${summary.policy_reason}`);
			const decision = await call(
				[
					"request",
					"approve",
					sandbox,
					requestId,
					"--reason",
					justification,
					"--actor",
					decisionActor,
					"--output",
					"json",
				],
				{ signal, timeoutMs: 120_000 },
			);
			return decisionSummary(decision);
		},

		async requestReject(input, signal) {
			const name = validateAgentName(input.name);
			const requestId = validateRequestId(input.request_id);
			const reason = boundedText(input.reason, "rejection reason", 512);
			const { sandbox } = await activeSandbox(name, signal);
			if (!sandbox) throw new Error("logical agent has no active sandbox runtime");
			validateAgentName(sandbox, "active sandbox");
			await call(["request", "get", sandbox, requestId, "--output", "json"], { signal });
			const decision = await call(
				[
					"request",
					"reject",
					sandbox,
					requestId,
					"--reason",
					reason,
					"--actor",
					decisionActor,
					"--output",
					"json",
				],
				{ signal, timeoutMs: 120_000 },
			);
			return decisionSummary(decision);
		},

		async agentRetire(input, signal) {
			const name = validateAgentName(input.name);
			const reason = boundedText(input.reason, "retirement reason", 4096);
			await rawAgentStatus(name, signal);
			const response = await call(
				["agent", "retire", name, "--reason", reason, "--output", "json"],
				{ signal, timeoutMs: 120_000 },
			);
			return agentSummary(response.agent);
		},

		async outputVerify(input, signal) {
			const name = validateAgentName(input.name);
			const relativePath = validateRelativeOutputPath(input.relative_path);
			const expected = expectedObject(input.expected);
			const allowExtra = input.allow_extra_fields === true;
			const status = await rawAgentStatus(name, signal);
			if (status.active_sandbox !== null) {
				throw new Error("release the logical agent runtime before verifying its output");
			}
			const bytes = await readBoundedRegularFile(status.workspace, relativePath, ACCEPTANCE_FILE_LIMIT);
			let observed;
			try {
				observed = JSON.parse(bytes.toString("utf8"));
			} catch {
				throw new Error("acceptance output is not one complete JSON document");
			}
			if (!observed || typeof observed !== "object" || Array.isArray(observed)) {
				throw new Error("acceptance output is not a JSON object");
			}
			for (const [key, expectedValue] of Object.entries(expected)) {
				if (!Object.hasOwn(observed, key) || !Object.is(observed[key], expectedValue)) {
					throw new Error(`acceptance output field ${key} did not match`);
				}
			}
			if (!allowExtra && Object.keys(observed).length !== Object.keys(expected).length) {
				throw new Error("acceptance output contains extra fields");
			}
			return {
				name,
				relative_path: relativePath,
				sha256: createHash("sha256").update(bytes).digest("hex"),
				size: bytes.length,
				matched_fields: Object.keys(expected).sort(),
				extra_fields_allowed: allowExtra,
				acceptance_verified: true,
			};
		},

		async artifactRead(input, signal) {
			const name = validateAgentName(input.name);
			const relativePath = validateRelativeOutputPath(input.relative_path);
			const startLine = input.start_line ?? 1;
			const maxLines = input.max_lines ?? 200;
			if (!Number.isInteger(startLine) || startLine < 1 || startLine > 10_000_000) {
				throw new Error("start line must be an integer from 1 through 10000000");
			}
			if (!Number.isInteger(maxLines) || maxLines < 1 || maxLines > 400) {
				throw new Error("max lines must be an integer from 1 through 400");
			}
			const status = await rawAgentStatus(name, signal);
			if (status.active_sandbox !== null) {
				throw new Error("release the logical agent runtime before reading its artifact");
			}
			const bytes = await readBoundedRegularFile(status.workspace, relativePath, ACCEPTANCE_FILE_LIMIT);
			let text;
			try {
				text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
			} catch {
				throw new Error("artifact is not UTF-8 text");
			}
			if (text.includes("\0")) throw new Error("artifact contains binary data");
			const lines = text.split(/\r?\n/u);
			const startIndex = Math.min(startLine - 1, lines.length);
			const endIndex = Math.min(startIndex + maxLines, lines.length);
			const selected = Buffer.from(lines.slice(startIndex, endIndex).join("\n"), "utf8");
			let returned = selected;
			let byteTruncated = false;
			if (selected.length > ARTIFACT_RETURN_LIMIT) {
				let boundary = ARTIFACT_RETURN_LIMIT;
				while (boundary > 0) {
					try {
						new TextDecoder("utf-8", { fatal: true }).decode(selected.subarray(0, boundary));
						break;
					} catch {
						boundary -= 1;
					}
				}
				returned = selected.subarray(0, boundary);
				byteTruncated = true;
			}
			return {
				name,
				relative_path: relativePath,
				sha256: createHash("sha256").update(bytes).digest("hex"),
				size: bytes.length,
				total_lines: lines.length,
				start_line: startIndex + 1,
				end_line: endIndex,
				truncated: byteTruncated || endIndex < lines.length,
				content: returned.toString("utf8"),
			};
		},

		async cleanupCheck(input = {}, signal) {
			assertEnabled();
			const list = await call(["list", "--output", "json"], { signal });
			const state = await call(["state", "verify", "--output", "json"], { signal, timeoutMs: 120_000 });
			const audit = await call(["audit", "verify", "--output", "json"], { signal, timeoutMs: 120_000 });
			const doctor = await call(["doctor", "--output", "json"], { signal, timeoutMs: 120_000 });
			const summary = {
				managed_sandboxes: Array.isArray(list.managed) ? list.managed.length : -1,
				live_workspaces: list.live && typeof list.live === "object" ? Object.keys(list.live).length : -1,
				allocation_reservations: Array.isArray(doctor.allocation_reservations)
					? doctor.allocation_reservations.length
					: -1,
				state_valid: state.event_chains_valid === true && state.schemas_valid === true && state.artifacts_valid === true,
				audit_valid: audit.valid === true,
			};
			if (
				input.require_zero !== false &&
				(summary.managed_sandboxes !== 0 ||
					summary.live_workspaces !== 0 ||
					summary.allocation_reservations !== 0)
			) {
				throw new Error("controller still has an active sandbox or allocation reservation");
			}
			if (!summary.state_valid || !summary.audit_valid) {
				throw new Error("controller state or audit verification failed");
			}
			return summary;
		},
	};
}
