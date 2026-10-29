# Agent-led setup and harnesses

The repository uses nine focused Agent Skills:

| Skill | Use it for |
|---|---|
| `enrichedlab-agent-setup` | Entry point for a new install or guided setup |
| `enrichedlab-agent-install` | Platform-specific install and upgrade work |
| `enrichedlab-agent-qualify` | Source, runtime, dashboard, isolation, service, and cleanup gates |
| `enrichedlab-agent-operations` | Daily sandbox, approval, service, team, catalog, and evidence work |
| `enrichedlab-agent-continuity` | Named agents whose state persists across disposable runtimes |
| `enrichedlab-research-team` | Independent scientific or data-analysis workers and result custody |
| `enrichedlab-agent-state` | Scoped working state, context epochs, reasoning policy, and provenance export |
| `enrichedlab-agent-remote` | Researcher login, durable cluster tasks, reconnect, cancellation, and artifact download |
| `enrichedlab-agent-broker` | Shared Kubernetes broker deployment, Flux order, identity, quota, and recovery |

Each directory has the exact uppercase filename `SKILL.md`. Setup instructions live in the setup skill; operating rules
are not mixed into the installer.

## Install the skills

Install links for every recognized harness:

```bash
scripts/install_agent_skills.sh --all
```

Or select only installed harnesses:

```bash
scripts/install_agent_skills.sh --codex --claude
scripts/install_agent_skills.sh --opencode --pi --omp --copilot
```

Default catalogs:

| Harness | Directory |
|---|---|
| Codex | `~/.codex/skills` |
| Claude Code | `~/.claude/skills` |
| OpenCode | `$XDG_CONFIG_HOME/opencode/skills`, or `~/.config/opencode/skills` |
| Pi | `~/.pi/agent/skills` |
| OMP | `~/.omp/agent/skills` |
| GitHub Copilot | `~/.copilot/skills` |

Use `--target-dir /absolute/path` for another Agent Skills-compatible harness. Every selected target is checked before
anything changes. Existing entries stop the install unless `--replace` is given; replaced entries move into the private
controller state directory. `--migrate-legacy` handles the prior monolithic skill without deleting it.

Restart an open harness session after installing links so it reloads the skill catalog.

## Use the setup skill without installing it

An agent working in the cloned repository can read the source directly:

```text
Read .agents/skills/enrichedlab-agent-setup/SKILL.md and set up this repository for my host.
```

Harnesses differ in automatic skill discovery, so the explicit prompt remains the portable fallback.

## Authority boundary

Installing a skill does not give a model more OS authority. A host agent may call the controller within the permissions
you gave that harness. A sandboxed agent receives no privileged controller binary or host credentials. It can write a
bounded request; the host controller or operator decides it.

Pi and OpenCode have pinned automatic adapters for direct sandbox runs and research segments. Their JSONL events are
normalized into the same provenance schema and fail closed on malformed or truncated capture. Provider-returned
reasoning is restricted evidence and is never inserted into prompt-facing state.

For an unattended Pi or OMP Team Lead, load `scripts/harness/pi_omp_sandbox_teamlead.mjs` and activate only its
`sandbox_teamlead_*` tools plus any read-only tools the lead needs. Do not give that session Bash merely so it can call
the controller. The adapter uses exact argv, exposes create, single or concurrent runs, status, output verification,
retirement, bounded artifact reads, permission-request review, and cleanup checks. Raw model output, reasoning, and host paths are omitted from
the Team Lead's tool result. It binds every operation to the configured Team Lead owner and route allowlist, and it
stays disabled unless the host launcher explicitly enables it. `agent_run` proves that the harness process finished;
`output_verify` separately checks a bounded regular JSON file under the stopped worker's own workspace. Use
`artifact_read` only for an explicitly named UTF-8 result or manifest; it limits each excerpt so a worker cannot fill
the Team Lead's context with one file.

Request review is owner-scoped to the logical agent's current sandbox. Listing and rejection are available after the
host enables the adapter. Approval remains off unless `ENRICHEDLAB_TEAMLEAD_APPROVAL_KINDS` names `host-service`,
`catalog-output`, or `domain-read`. A domain read also needs an exact hostname in
`ENRICHEDLAB_TEAMLEAD_APPROVAL_DOMAINS`. LAN services, peer links, mounts, write domains, and unlisted domains are
reported as `human_required`. Keep both approval variables unset for manual-only operation.

OpenCode uses the same core through `scripts/harness/opencode_sandbox_teamlead.ts`. Its local tool entry imports
OpenCode's own `tool` API, passes it to `createOpenCodeSandboxTeamLeadTools`, and exports each returned definition. This
keeps package resolution in OpenCode's config directory while the controller code stays in the reviewed source tree.
Allow only its `sandbox_teamlead_*` tools for an unattended coordinator profile, and deny Bash and the native subagent
tool. The launcher must set `ENRICHEDLAB_TEAMLEAD_ENABLED=1`, a DNS-safe
`ENRICHEDLAB_TEAMLEAD_OWNER`, a comma-separated `ENRICHEDLAB_TEAMLEAD_ROUTES` allowlist, and
`ENRICHEDLAB_AGENT_SANDBOX_BIN` to an absolute host wrapper when the controller needs private root, state, config,
OpenShell, or runtime arguments. Never put those coordinates in this repository.

`agent_run` is synchronous. The same model turn cannot stop its own wait to answer a worker request. Use the dashboard,
a separate owner-scoped supervisor session, or a durable cluster broker while a worker is active. The current local
adapter does not make a laptop-hosted run survive laptop shutdown.

For cluster-owned work that survives a laptop shutdown, use `scripts/harness/pi_omp_remote_broker.mjs` or
`scripts/harness/opencode_remote_broker.ts`. Their `remote_agent_*` tools cover identity, submission, listing, status,
cancellation, lifecycle events, artifact listing, and verified download. Login stays outside the model session. The
launcher must set `ENRICHEDLAB_REMOTE_BROKER_ENABLED=1`, an absolute `ENRICHEDLAB_AGENT_REMOTE_BIN`, and an owner-only
`ENRICHEDLAB_REMOTE_OUTPUT_ROOT`. The adapters do not expose a broker URL override, Access token, raw OpenShell, or an
arbitrary host download path. See [remote broker](remote-broker.md).

An OpenCode profile also needs read-only `external_directory` permission for the exact installed directories of any
skill it is required to load. Skill bodies do not grant access to their referenced files. Keep the default external
directory rule denied, allow only those skill directories, and leave edits denied. Include every catalog OpenCode may
select when the same skill is installed in more than one supported location. Run `opencode debug agent <profile>` and
an explicit skill-reference read before accepting an unattended profile.

Codex, Claude Code, and OMP can act as the host controller through these skills. They create named workers, select a
private route for each worker, and review or retire the resulting evidence. They do not run inside the disposable
sandbox or receive a second privileged controller. A Team Lead can allocate one or more workers on the same route,
subject to that route's controller pool cap, or mix routes in one team. Pi and OpenCode remain the qualified runtime
adapters inside those workers.

Private host-harness transcripts are not silently treated as machine evidence. A custom scientist harness should use
the stable `provenance ingest` boundary:

- `--harness open-ai-compatible` for a reviewed inference stream;
- `--harness native` for the strict EnrichedLab event envelope;
- the state CLI for typed state mutations, checkpoints, and artifacts.

This is the recommended application boundary because it does not tie the scheduler, UI, or custody record to Pi or
OpenCode internals. Add a native Codex or Claude adapter only when its versioned event format has fixtures, truncation
tests, and a documented field mapping. See [provenance](provenance.md).

Site model routes, API credentials, network addresses, mount paths, and cluster settings remain in private local config.
Do not add them to the reusable skills.
