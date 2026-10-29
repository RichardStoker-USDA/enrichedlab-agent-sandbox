#!/bin/sh
set -eu

umask 077

profile=
route=
harness=open-code
exercise_harness=false
attempt_timeout_seconds=600
controller_root=
controller_state_dir=
controller_config_dir=
controller_openshell_bin=
controller_docker_bin=
name=cq-$(date -u +%y%m%d%H%M%S)
registered=false
retired=false
scratch_parent=${TMPDIR:-/tmp}
scratch=$(mktemp -d "$scratch_parent/enrichedlab-continuity.XXXXXX")
first=$scratch/first.json
released=$scratch/released.json
second=$scratch/second.json
final=$scratch/final.json
harness_result=$scratch/harness.json

usage() {
    printf '%s\n' \
        'Usage: qualify_continuity.sh --profile NAME --route NAME [options]' \
        '' \
        '  --name NAME            Retained logical-agent qualification name' \
        '  --harness NAME         open-code or pi (default: open-code)' \
        '  --root PATH            Match a custom controller workspace root' \
        '  --state-dir PATH       Match a custom controller state directory' \
        '  --config-dir PATH      Match a custom controller config directory' \
        '  --openshell-bin PATH   Match a custom OpenShell executable' \
        '  --docker-bin PATH      Match a custom Docker-compatible executable' \
        '  --attempt-timeout-seconds N  Provider-session deadline (default: 600)' \
        '  --exercise-harness     Run one live model session in addition to state restore' \
        '  -h, --help             Show this help'
}

controller() {
    if [ -n "$controller_docker_bin" ]; then
        set -- --docker-bin "$controller_docker_bin" "$@"
    fi
    if [ -n "$controller_openshell_bin" ]; then
        set -- --openshell-bin "$controller_openshell_bin" "$@"
    fi
    if [ -n "$controller_config_dir" ]; then
        set -- --config-dir "$controller_config_dir" "$@"
    fi
    if [ -n "$controller_state_dir" ]; then
        set -- --state-dir "$controller_state_dir" "$@"
    fi
    if [ -n "$controller_root" ]; then
        set -- --root "$controller_root" "$@"
    fi
    agent-sandbox --output json "$@"
}

cleanup() {
    if [ "$registered" = true ] && [ "$retired" = false ]; then
        controller agent release "$name" >/dev/null 2>&1 || true
        controller agent retire "$name" --reason 'qualification interrupted' >/dev/null 2>&1 || true
    fi
    rm -f -- "$first" "$released" "$second" "$final" "$harness_result"
    rmdir "$scratch" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

while [ "$#" -gt 0 ]; do
    case "$1" in
        --profile|--route|--name|--harness|--root|--state-dir|--config-dir|--openshell-bin|--docker-bin|--attempt-timeout-seconds)
            [ "$#" -ge 2 ] || { printf '%s needs a value\n' "$1" >&2; exit 1; }
            option=$1
            value=$2
            shift 2
            case "$option" in
                --profile) profile=$value ;;
                --route) route=$value ;;
                --name) name=$value ;;
                --harness) harness=$value ;;
                --root) controller_root=$value ;;
                --state-dir) controller_state_dir=$value ;;
                --config-dir) controller_config_dir=$value ;;
                --openshell-bin) controller_openshell_bin=$value ;;
                --docker-bin) controller_docker_bin=$value ;;
                --attempt-timeout-seconds) attempt_timeout_seconds=$value ;;
            esac
            ;;
        --exercise-harness) exercise_harness=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'unknown option: %s\n' "$1" >&2; exit 1 ;;
    esac
done

[ -n "$profile" ] || { printf '%s\n' '--profile is required' >&2; exit 1; }
[ -n "$route" ] || { printf '%s\n' '--route is required' >&2; exit 1; }
case "$harness" in
    open-code|pi) ;;
    *) printf '%s\n' '--harness must be open-code or pi' >&2; exit 1 ;;
esac
command -v agent-sandbox >/dev/null 2>&1 || { printf '%s\n' 'agent-sandbox is required' >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { printf '%s\n' 'python3 is required' >&2; exit 1; }

controller doctor >/dev/null
controller agent create "$name" \
    --domain cluster-ops \
    --state-profile cluster-operator \
    --objective 'Verify scoped state survives disposable runtime generations.' \
    --role 'continuity qualification worker' \
    --route "$route" \
    --gateway-profile "$profile" \
    --harness "$harness" >/dev/null
registered=true

controller agent start "$name" >"$first"
python3 - "$first" <<'PY'
import json
import os
import pathlib
import sys
import uuid

result = json.loads(pathlib.Path(sys.argv[1]).read_text())
agent = result["agent"]
view_path = pathlib.Path(agent["workspace"]) / ".agent-state" / "view.json"
view = json.loads(view_path.read_text())
knowledge = view["collections"]["knowledge"]
mutation = {
    "schema_version": 1,
    "run_id": view["run_id"],
    "subject": view["subject"],
    "collection": "knowledge",
    "base_version": knowledge["version"],
    "set": {
        "/lessons/runtime-continuity": {
            "lesson": "scoped state outlives disposable compute",
            "trigger": "qualification runtime release",
            "evidence": [f"runtime:{result['sandbox']['name']}"]
        }
    },
    "remove": [],
    "rationale": {
        "summary": "qualification state marker",
        "alternatives": [],
        "evidence": [f"runtime:{result['sandbox']['name']}"],
        "confidence": 1.0
    }
}
outbox = view_path.parent / "outbox"
pending = outbox / f".{uuid.uuid4().hex}.pending"
accepted = outbox / "continuity-qualification.json"
pending.write_text(json.dumps(mutation, indent=2) + "\n")
os.replace(pending, accepted)
PY

controller agent release "$name" >"$released"
python3 - "$first" "$released" <<'PY'
import json
import pathlib
import sys

first = json.loads(pathlib.Path(sys.argv[1]).read_text())
released = json.loads(pathlib.Path(sys.argv[2]).read_text())
assert first["agent"]["runtime_generation"] == 1
assert released["agent"]["active_sandbox"] is None
assert released["agent"]["lifecycle"] == "idle"
assert released["checkpoint"] is not None
PY

controller agent start "$name" >"$second"
python3 - "$first" "$second" <<'PY'
import json
import pathlib
import sys

first = json.loads(pathlib.Path(sys.argv[1]).read_text())
second = json.loads(pathlib.Path(sys.argv[2]).read_text())
assert second["agent"]["agent_id"] == first["agent"]["agent_id"]
assert second["agent"]["state_run_id"] == first["agent"]["state_run_id"]
assert second["agent"]["runtime_generation"] == 2
assert second["sandbox"]["name"] != first["sandbox"]["name"]
view_path = pathlib.Path(second["agent"]["workspace"]) / ".agent-state" / "view.json"
view = json.loads(view_path.read_text())
lesson = view["collections"]["knowledge"]["value"]["lessons"]["runtime-continuity"]
assert lesson["lesson"] == "scoped state outlives disposable compute"
PY

controller agent release "$name" >/dev/null
if [ "$exercise_harness" = true ]; then
    controller agent run "$name" --harness "$harness" --max-attempts 1 \
        --attempt-timeout-seconds "$attempt_timeout_seconds" -- \
        'Read the scoped state view and return the exact phrase continuity-qualified.' >"$harness_result"
    python3 - "$harness_result" <<'PY'
import json
import pathlib
import sys

result = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert result["passed"] is True
assert result["runtime_released"] is True
assert result["attempts"] == 1
PY
fi

controller agent retire "$name" --reason 'continuity qualification completed' >/dev/null
retired=true
controller agent status "$name" >"$final"
python3 - "$final" <<'PY'
import json
import pathlib
import sys

agent = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert agent["lifecycle"] == "retired"
assert agent["active_sandbox"] is None
assert agent["runtime_generation"] >= 2
PY
controller state verify >/dev/null
controller audit verify >/dev/null
printf 'Continuity qualification passed. Retained logical agent: %s\n' "$name"
