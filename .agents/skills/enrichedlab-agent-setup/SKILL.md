---
name: enrichedlab-agent-setup
description: Set up EnrichedLab Agent Sandbox from a cloned repository. Use when a user wants an agent to inspect the host, choose a supported local or Kubernetes path, install the controller and focused skills, run qualification, and return a clear setup receipt, or guide the same process when the agent has no shell access.
license: MIT
---

# EnrichedLab agent setup

This workflow needs a cloned repository, Git, and either agentic shell access or a user who can run commands. Runtime
support depends on the selected NVIDIA OpenShell platform.

This is the entrypoint. Read [references/setup_questions.md](references/setup_questions.md), then use the install and
qualification skills named below. Do not make the user learn the repository layout first.

## Decide how to help

- Autonomous mode: use when you can inspect files, run commands, and read results on the target host.
- Guided mode: use when you cannot operate the host. Give one bounded command at a time, explain the expected result,
  and wait for the user to paste non-secret output before continuing.
- Never ask a user to paste an API key, certificate key, token, kubeconfig, or credential file. Ask only for its path.

## Setup flow

1. Resolve the repository root from this `SKILL.md`; do not assume the current directory.
2. Read the root `README.md`, `SECURITY.md`, and `docs/platforms.md`.
3. Ask only unanswered questions from the setup questionnaire. Prefer existing host facts over questions.
4. State whether this is a single-operator controller, a researcher client for an existing broker, or a site-owned
   shared broker. Also state the selected platform, workspace root, network posture, dashboard choice, provenance
   backend, and qualification depth before mutation. Label unsupported or experimental paths exactly as the platform
   guide does.
5. For a researcher client, read `../enrichedlab-agent-remote/SKILL.md` and stop after its client acceptance checks.
   For a shared broker, read `../enrichedlab-agent-broker/SKILL.md` before the install and qualification skills.
6. Read `../enrichedlab-agent-install/SKILL.md` and complete its workflow for a local controller or broker host.
7. Read `../enrichedlab-agent-qualify/SKILL.md` and complete its workflow. Installation without the selected live gate
   is incomplete unless the user explicitly asks for source-only setup.
8. Install the focused skills only after their repository bytes pass validation. Do not copy site routes, paths,
   addresses, or credentials into a skill.
9. Return a short receipt with installed version, platform, controller paths, dashboard address, provenance backend and
   tenant, tested allowed and denied paths, retained rollback location, and any claim that remains unqualified.

## Rules for weaker agents

- Do one phase at a time: discover, plan, install, qualify, report.
- Stop after a failed command. Diagnose that result before offering a different command.
- Do not change runtime, firewall, Kubernetes, or service state to make a test pass without naming the change and its
  rollback first.
- Never use a broad host mount, wildcard LAN bind, public dashboard, container socket mount, or host credential mount.
- A running sandbox is not proof of isolation. A Kubernetes RuntimeClass name is not proof that a microVM handler ran.
- Agreement between several model workers is not scientific validation.
- Native Windows, Podman, Docker Desktop, and an untested Kubernetes cluster are not qualified by this repository.

## User prompt

After cloning, a user can tell a coding agent:

```text
Read .agents/skills/enrichedlab-agent-setup/SKILL.md and set this repository up for me. Ask only for choices you
cannot safely infer, keep credentials out of commands and chat, run the matching qualification, and give me the short
setup receipt.
```

If the user later asks to operate sandboxes, use `../enrichedlab-agent-operations/SKILL.md`. For concurrent scientific
work, use `../enrichedlab-research-team/SKILL.md`.
For a named agent that must survive sandbox deletion, use `../enrichedlab-agent-continuity/SKILL.md`.
For state profiles, context resets, provenance storage, or evidence export, use
`../enrichedlab-agent-state/SKILL.md`.
For an authenticated remote task, use `../enrichedlab-agent-remote/SKILL.md`. For shared-cluster deployment, use
`../enrichedlab-agent-broker/SKILL.md`.
