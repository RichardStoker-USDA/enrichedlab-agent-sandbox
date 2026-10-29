# Team workflow

## Ownership model

The host agent is the lead. It selects tasks, declares non-overlapping repository path prefixes, starts sandboxes, reviews outputs, collects commits, and merges. Subagents commit only their assigned branch. They do not push, merge, edit another worker's paths, or receive a shared Git remote.

```bash
agent-sandbox team create study --repo /absolute/project --shared-output
agent-sandbox team add study ingest \
  --owns src/ingest \
  --owns tests/ingest \
  --task 'Implement and test input validation.' \
  --route local-agent
```

The worker sees:

```text
/sandbox/work/repo
/sandbox/work/output
/sandbox/work/assignment.json
/sandbox/mounts/shared-output   # only when enabled
```

Each worker may use its own output directory. Shared output is useful when workers exchange generated, non-secret artifacts. It is not a coordination lock. Assign a single writer per shared file or give workers separate subdirectories.

## Collection

The worker commits its branch and leaves the tree clean. The host then runs:

```bash
agent-sandbox team collect study ingest
agent-sandbox team merge study ingest
```

Collection rejects these states:

- uncommitted worker changes;
- a commit that is not descended from the frozen team base;
- any changed or deleted path outside the worker's ownership;
- a non-UTF-8 path or path containing line controls;
- an overlap with another worker's ownership declaration.

Git is run without system or global configuration, hooks, replace objects, an external diff, text conversion, credential helpers, inherited author variables, or prompts. The controller supplies the private `controller.toml` committer identity only for its merge commit, so a clean host does not need global Git identity. Integration happens in a clone that the sandboxes do not mount. The controller never pushes. A person or host agent reviews the integration branch before publishing it.

## Long-running projects

Use `agent-sandbox team stop <team>` to stop all worker sandboxes while keeping their containers, branches, outputs, and records. Start individual workers again with `agent-sandbox start <sandbox>`.

Use `agent-sandbox team delete <team>` to remove all worker sandbox resources and controller records while keeping the project tree, integration clone, branches, recovery files, and outputs. Running workers get evidence snapshots by default. If the team was already stopped, pass `--skip-evidence` or restart a worker before capture.

Catalog a finished tool or analysis after collection. Catalog registration hashes regular files and directories, rejects symlinks and special files, and keeps source attribution. Re-run `catalog verify` before another agent uses it.
