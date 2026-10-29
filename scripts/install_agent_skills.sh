#!/bin/sh
set -eu

umask 077

fail() {
    printf 'skill install: %s\n' "$1" >&2
    exit 1
}

validate_path() {
    value=$1
    case "$value" in
        /*) ;;
        *) fail "path must be absolute: $value" ;;
    esac
    case "$value" in
        *'|'*) fail "path contains an unsupported character: $value" ;;
    esac
    printable=$(printf '%s' "$value" | LC_ALL=C tr -d '[:cntrl:]')
    [ "$printable" = "$value" ] || fail "path contains a control character"
}

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
source_root=$(CDPATH='' cd -- "$script_dir/../.agents/skills" && pwd -P)
state_dir=${XDG_STATE_HOME:-${HOME:?HOME is not set}/.local/state}/enrichedlab-agent-sandbox
replace=false
migrate_legacy=false
selected=false
codex=false
claude=false
opencode=false
pi=false
omp=false
copilot=false
custom_dirs=
codex_dir=$HOME/.codex/skills
claude_dir=$HOME/.claude/skills
opencode_dir=${XDG_CONFIG_HOME:-$HOME/.config}/opencode/skills
pi_dir=$HOME/.pi/agent/skills
omp_dir=$HOME/.omp/agent/skills
copilot_dir=$HOME/.copilot/skills
skills='enrichedlab-agent-setup enrichedlab-agent-install enrichedlab-agent-qualify enrichedlab-agent-operations enrichedlab-agent-continuity enrichedlab-research-team enrichedlab-agent-state enrichedlab-agent-remote enrichedlab-agent-broker'

while [ "$#" -gt 0 ]; do
    case "$1" in
        --all)
            codex=true
            claude=true
            opencode=true
            pi=true
            omp=true
            copilot=true
            selected=true
            shift
            ;;
        --codex|--claude|--opencode|--pi|--omp|--copilot)
            selected=true
            case "$1" in
                --codex) codex=true ;;
                --claude) claude=true ;;
                --opencode) opencode=true ;;
                --pi) pi=true ;;
                --omp) omp=true ;;
                --copilot) copilot=true ;;
            esac
            shift
            ;;
        --target-dir)
            [ "$#" -ge 2 ] || fail '--target-dir needs a value'
            validate_path "$2"
            custom_dirs="${custom_dirs}${custom_dirs:+
}$2"
            selected=true
            shift 2
            ;;
        --replace) replace=true; shift ;;
        --migrate-legacy) migrate_legacy=true; shift ;;
        --state-dir|--codex-dir|--claude-dir|--opencode-dir|--pi-dir|--omp-dir|--copilot-dir)
            [ "$#" -ge 2 ] || fail "$1 needs a value"
            option=$1
            value=$2
            validate_path "$value"
            shift 2
            case "$option" in
                --state-dir) state_dir=$value ;;
                --codex-dir) codex_dir=$value ;;
                --claude-dir) claude_dir=$value ;;
                --opencode-dir) opencode_dir=$value ;;
                --pi-dir) pi_dir=$value ;;
                --omp-dir) omp_dir=$value ;;
                --copilot-dir) copilot_dir=$value ;;
            esac
            ;;
        -h|--help)
            printf '%s\n' \
                'Usage: install_agent_skills.sh [targets] [options]' \
                '' \
                'Targets: --all, --codex, --claude, --opencode, --pi, --omp, --copilot' \
                '         --target-dir ABSOLUTE_PATH (repeatable)' \
                'Options: --replace --migrate-legacy --state-dir PATH' \
                '         --codex-dir PATH --claude-dir PATH --opencode-dir PATH' \
                '         --pi-dir PATH --omp-dir PATH --copilot-dir PATH'
            exit 0
            ;;
        *) fail "unknown option: $1" ;;
    esac
done

[ "$selected" = true ] || fail 'select at least one harness or use --all'
validate_path "$state_dir"
for source in $skills; do
    [ -f "$source_root/$source/SKILL.md" ] || fail "missing source skill: $source"
done

backup_dir=$state_dir/skill-backups
install -d -m 700 "$backup_dir"
stamp=$(date -u +%Y%m%dT%H%M%SZ).$$
targets=

add_target() {
    label=$1
    parent=$2
    validate_path "$parent"
    targets="${targets}${targets:+
}$label|$parent"
}

if [ "$codex" = true ]; then add_target codex "$codex_dir"; fi
if [ "$claude" = true ]; then add_target claude "$claude_dir"; fi
if [ "$opencode" = true ]; then add_target opencode "$opencode_dir"; fi
if [ "$pi" = true ]; then add_target pi "$pi_dir"; fi
if [ "$omp" = true ]; then add_target omp "$omp_dir"; fi
if [ "$copilot" = true ]; then add_target copilot "$copilot_dir"; fi
custom_number=0
if [ -n "$custom_dirs" ]; then
    while IFS= read -r parent; do
        custom_number=$((custom_number + 1))
        add_target "custom-$custom_number" "$parent"
    done <<EOF
$custom_dirs
EOF
fi

preflight_parent() {
    label=$1
    parent=$2
    for skill in $skills; do
        source=$source_root/$skill
        target=$parent/$skill
        if [ -e "$target" ] || [ -L "$target" ]; then
            if [ "$target" -ef "$source" ]; then
                continue
            fi
            [ "$replace" = true ] || fail "$target exists for $label; inspect it or use --replace"
        fi
    done
    legacy=$parent/openshell-agent-sandbox
    if { [ -e "$legacy" ] || [ -L "$legacy" ]; } && [ "$migrate_legacy" = false ]; then
        fail "$legacy is the prior single skill; inspect it or use --migrate-legacy"
    fi
}

while IFS='|' read -r label parent; do
    preflight_parent "$label" "$parent"
done <<EOF
$targets
EOF

install_parent() {
    label=$1
    parent=$2
    install -d -m 700 "$parent"
    legacy=$parent/openshell-agent-sandbox
    if { [ -e "$legacy" ] || [ -L "$legacy" ]; } && [ "$migrate_legacy" = true ]; then
        mv "$legacy" "$backup_dir/$label.legacy.$stamp"
        printf 'Backed up prior skill for %s: %s\n' "$label" "$legacy"
    fi
    for skill in $skills; do
        source=$source_root/$skill
        target=$parent/$skill
        if [ -e "$target" ] || [ -L "$target" ]; then
            if [ "$target" -ef "$source" ]; then
                printf 'Skill already linked for %s: %s\n' "$label" "$target"
                continue
            fi
            mv "$target" "$backup_dir/$label.$skill.$stamp"
        fi
        ln -s "$source" "$target"
        printf 'Linked skill for %s: %s\n' "$label" "$target"
    done
}

while IFS='|' read -r label parent; do
    install_parent "$label" "$parent"
done <<EOF
$targets
EOF

printf '%s\n' 'Restart running harness sessions so they reload the skill catalog.'
