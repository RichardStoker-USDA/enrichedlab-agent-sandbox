#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)

command -v cargo >/dev/null 2>&1 || {
    printf '%s\n' 'skill validation needs cargo on PATH' >&2
    exit 1
}

cd "$repo_root"
cargo test --locked --test repository_contracts agent_skills_are_well_formed_and_focused -- --exact
