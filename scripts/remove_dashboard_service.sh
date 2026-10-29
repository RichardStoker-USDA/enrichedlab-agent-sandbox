#!/bin/sh
set -eu

umask 077
state_dir=${XDG_STATE_HOME:-${HOME:?HOME is not set}/.local/state}/enrichedlab-agent-sandbox

if [ "${1:-}" = --state-dir ]; then
    [ "$#" -eq 2 ] || { printf '%s\n' 'Usage: remove_dashboard_service.sh [--state-dir PATH]' >&2; exit 1; }
    state_dir=$2
elif [ "$#" -ne 0 ]; then
    printf '%s\n' 'Usage: remove_dashboard_service.sh [--state-dir PATH]' >&2
    exit 1
fi

case "$state_dir" in /*) ;; *) printf '%s\n' 'state directory must be absolute' >&2; exit 1 ;; esac
install -d -m 700 "$state_dir/service-backups"
stamp=$(date -u +%Y%m%dT%H%M%SZ).$$
label=org.enrichedlab.agent-sandbox.dashboard
legacy_label=org.enrichedlab.openshell-agent-sandbox.dashboard

case "$(uname -s)" in
    Darwin)
        for service_label in "$label" "$legacy_label"; do
            service_path=$HOME/Library/LaunchAgents/$service_label.plist
            if [ -f "$service_path" ]; then
                launchctl bootout "gui/$(id -u)" "$service_path" >/dev/null 2>&1 || true
                mv "$service_path" "$state_dir/service-backups/$stamp.$service_label.plist"
            fi
        done
        ;;
    Linux)
        for service_name in enrichedlab-agent-sandbox-dashboard.service openshell-agent-sandbox-dashboard.service; do
            service_path=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$service_name
            systemctl --user disable --now "$service_name" >/dev/null 2>&1 || true
            if [ -f "$service_path" ]; then
                mv "$service_path" "$state_dir/service-backups/$stamp.$service_name"
            fi
        done
        systemctl --user daemon-reload
        ;;
    *) printf '%s\n' 'unsupported host service manager' >&2; exit 1 ;;
esac

printf '%s\n' 'Dashboard service removed. Controller state and the prior service file backup were kept.'
