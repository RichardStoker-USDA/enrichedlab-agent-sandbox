import { execFile as execFileCallback } from "node:child_process";
import { chmod, lstat, mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { promisify } from "node:util";

const execFile = promisify(execFileCallback);
const RESPONSE_LIMIT = 16 * 1024 * 1024;
const KEY_PATTERN = /^[a-z][a-z0-9_-]{1,63}$/u;
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu;
const FILE_NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/u;

function boundedText(value, label, maximum) {
	if (typeof value !== "string" || value.trim().length === 0 || Buffer.byteLength(value) > maximum) {
		throw new Error(`${label} must contain 1-${maximum} bytes`);
	}
	if (/\0/u.test(value)) throw new Error(`${label} contains a null byte`);
	return value;
}

function key(value, label) {
	if (typeof value !== "string" || !KEY_PATTERN.test(value)) throw new Error(`${label} is invalid`);
	return value;
}

function uuid(value, label) {
	if (typeof value !== "string" || !UUID_PATTERN.test(value)) throw new Error(`${label} is not a UUID`);
	return value.toLowerCase();
}

function binary(value) {
	const configured = value ?? process.env.ENRICHEDLAB_AGENT_REMOTE_BIN ?? "agent-sandbox-remote";
	if (typeof configured !== "string" || configured.length === 0 || configured.length > 4096) {
		throw new Error("remote broker binary is invalid");
	}
	if (configured.includes("/") && !isAbsolute(configured)) {
		throw new Error("remote broker binary with a path must be absolute");
	}
	return configured;
}

function outputRoot(value) {
	const configured = value ?? process.env.ENRICHEDLAB_REMOTE_OUTPUT_ROOT;
	if (typeof configured !== "string" || !isAbsolute(configured) || configured.length > 4096) {
		throw new Error("remote output root must be an absolute launcher-controlled directory");
	}
	return configured;
}

function parseJson(output) {
	const text = String(output ?? "").trim();
	if (text.length === 0 || Buffer.byteLength(text) > RESPONSE_LIMIT) {
		throw new Error("remote broker returned an empty or oversized response");
	}
	try {
		return JSON.parse(text);
	} catch {
		throw new Error("remote broker did not return JSON");
	}
}

async function defaultRun(program, args, options) {
	try {
		return await execFile(program, args, {
			signal: options.signal,
			timeout: options.timeoutMs,
			maxBuffer: RESPONSE_LIMIT,
			encoding: "utf8",
			shell: false,
		});
	} catch (error) {
		const code = typeof error?.code === "number" ? error.code : "unavailable";
		throw new Error(`remote broker command failed with exit code ${code}`);
	}
}

export function createRemoteBrokerClient(options = {}) {
	const enabled = options.enabled ?? process.env.ENRICHEDLAB_REMOTE_BROKER_ENABLED === "1";
	if (!enabled) throw new Error("remote broker tools are disabled by the host launcher");
	const program = binary(options.binary);
	const run = options.run ?? defaultRun;
	const invoke = async (args, signal, timeoutMs = 65_000) => {
		const result = await run(program, args, { signal, timeoutMs });
		return parseJson(result.stdout);
	};
	const invokeWithoutOutput = async (args, signal, timeoutMs = 300_000) => {
		await run(program, args, { signal, timeoutMs });
	};

	return {
		me(signal) {
			return invoke(["me"], signal);
		},
		list(input = {}, signal) {
			const limit = input.limit ?? 50;
			if (!Number.isInteger(limit) || limit < 1 || limit > 200) throw new Error("task limit is invalid");
			return invoke(["list", "--limit", String(limit)], signal);
		},
		get(input, signal) {
			return invoke(["get", uuid(input.task, "task ID")], signal);
		},
		cancel(input, signal) {
			return invoke(["cancel", uuid(input.task, "task ID")], signal);
		},
		events(input, signal) {
			const after = input.after ?? 0;
			const limit = input.limit ?? 200;
			if (!Number.isInteger(after) || after < 0 || !Number.isInteger(limit) || limit < 1 || limit > 1000) {
				throw new Error("event cursor or limit is invalid");
			}
			return invoke(
				["events", uuid(input.task, "task ID"), "--after", String(after), "--limit", String(limit)],
				signal,
			);
		},
		artifacts(input, signal) {
			return invoke(["artifacts", uuid(input.task, "task ID")], signal);
		},
		async download(input, signal) {
			const task = uuid(input.task, "task ID");
			const artifact = uuid(input.artifact, "artifact ID");
			if (typeof input.file_name !== "string" || !FILE_NAME_PATTERN.test(input.file_name)) {
				throw new Error("artifact file name is invalid");
			}
			const configuredRoot = outputRoot(options.outputRoot);
			const rootMetadata = await lstat(configuredRoot);
			if (!rootMetadata.isDirectory() || rootMetadata.isSymbolicLink()) {
				throw new Error("remote output root must be a non-symlink directory");
			}
			const canonicalRoot = await realpath(configuredRoot);
			const taskDirectory = join(canonicalRoot, task);
			try {
				await mkdir(taskDirectory, { mode: 0o700 });
			} catch (error) {
				if (error?.code !== "EEXIST") throw error;
			}
			const taskMetadata = await lstat(taskDirectory);
			if (!taskMetadata.isDirectory() || taskMetadata.isSymbolicLink()) {
				throw new Error("remote task output directory is unsafe");
			}
			const output = join(taskDirectory, input.file_name);
			try {
				await lstat(output);
				throw new Error("artifact output already exists");
			} catch (error) {
				if (error?.code !== "ENOENT") throw error;
			}
			await invokeWithoutOutput(
				["download", task, artifact, "--output", output],
				signal,
			);
			const resultMetadata = await lstat(output);
			if (!resultMetadata.isFile() || resultMetadata.isSymbolicLink()) {
				throw new Error("remote client did not create a regular artifact file");
			}
			return {
				downloaded: true,
				task,
				artifact,
				relative_output: `${task}/${input.file_name}`,
				size_bytes: resultMetadata.size,
			};
		},
		async submit(input, signal) {
			const group = key(input.group, "research group");
			const profile = key(input.profile, "agent profile");
			const objective = boundedText(input.objective, "objective", 8192);
			if (
				input.idempotency_key !== undefined &&
				(typeof input.idempotency_key !== "string" ||
					input.idempotency_key.length === 0 ||
					input.idempotency_key.length > 128 ||
					/[\s\x00-\x1f\x7f]/u.test(input.idempotency_key))
			) {
				throw new Error("idempotency key is invalid");
			}
			const directory = await mkdtemp(join(tmpdir(), "enrichedlab-remote-objective-"));
			const objectivePath = join(directory, "objective.txt");
			try {
				await writeFile(objectivePath, objective, { encoding: "utf8", mode: 0o600, flag: "wx" });
				await chmod(objectivePath, 0o600);
				const args = [
					"submit",
					"--group",
					group,
					"--profile",
					profile,
					"--objective-file",
					objectivePath,
				];
				if (input.idempotency_key) args.push("--idempotency-key", input.idempotency_key);
				return await invoke(args, signal);
			} finally {
				await rm(directory, { recursive: true, force: true });
			}
		},
	};
}

export function createDeferredRemoteBrokerClient(options = {}) {
	const call = (method, args) =>
		Promise.resolve().then(() => createRemoteBrokerClient(options)[method](...args));
	return {
		me: (...args) => call("me", args),
		list: (...args) => call("list", args),
		get: (...args) => call("get", args),
		cancel: (...args) => call("cancel", args),
		events: (...args) => call("events", args),
		artifacts: (...args) => call("artifacts", args),
		download: (...args) => call("download", args),
		submit: (...args) => call("submit", args),
	};
}

export { key as validateRemoteKey, parseJson as parseRemoteJson, uuid as validateRemoteUuid };
