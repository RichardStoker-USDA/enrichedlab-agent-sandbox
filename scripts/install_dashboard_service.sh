#!/bin/sh
set -eu

umask 077

fail() {
    printf 'dashboard service: %s\n' "$1" >&2
    exit 1
}

validate_service_path() {
    service_value=$1
    case "$service_value" in
        /*) ;;
        *) fail 'every service path must be absolute' ;;
    esac
    case "$service_value" in
        *'<'*|*'>'*|*'&'*|*'"'*|*"'"*|*\\*|*'%'*|*'
'*) fail "service path contains an unsupported character: $service_value" ;;
    esac
    control_bytes=$(printf '%s' "$service_value" | LC_ALL=C tr -d '\040-\176')
    [ -z "$control_bytes" ] || fail "service path contains a control character: $service_value"
}

binary=
openshell_bin=
docker_bin=
root=
state_dir=
config_dir=
render_only=false
service_path_override=

while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary|--openshell-bin|--docker-bin|--root|--state-dir|--config-dir|--service-path)
            [ "$#" -ge 2 ] || fail "$1 needs a value"
            option=$1
            value=$2
            shift 2
            case "$option" in
                --binary) binary=$value ;;
                --openshell-bin) openshell_bin=$value ;;
                --docker-bin) docker_bin=$value ;;
                --root) root=$value ;;
                --state-dir) state_dir=$value ;;
                --config-dir) config_dir=$value ;;
                --service-path) service_path_override=$value ;;
            esac
            ;;
        --render-only) render_only=true; shift ;;
        -h|--help)
            printf '%s\n' 'Usage: install_dashboard_service.sh --binary PATH --openshell-bin PATH --docker-bin PATH --root PATH --state-dir PATH --config-dir PATH [--render-only --service-path PATH]'
            exit 0
            ;;
        *) fail "unknown option: $1" ;;
    esac
done

if [ -n "$service_path_override" ] && [ "$render_only" = false ]; then
    fail '--service-path is allowed only with --render-only'
fi

for path in "$binary" "$openshell_bin" "$docker_bin" "$root" "$state_dir" "$config_dir"; do
    validate_service_path "$path"
done
if [ -n "$service_path_override" ]; then
    validate_service_path "$service_path_override"
fi

[ -x "$binary" ] || fail "controller binary is not executable: $binary"
[ -x "$openshell_bin" ] || fail "OpenShell binary is not executable: $openshell_bin"
[ -x "$docker_bin" ] || fail "Docker-compatible binary is not executable: $docker_bin"
install -d -m 700 "$state_dir" "$state_dir/logs" "$state_dir/service-backups"
stamp=$(date -u +%Y%m%dT%H%M%SZ).$$
label=org.enrichedlab.agent-sandbox.dashboard
legacy_label=org.enrichedlab.openshell-agent-sandbox.dashboard

case "$(uname -s)" in
    Darwin)
        service_path=${service_path_override:-$HOME/Library/LaunchAgents/$label.plist}
        install -d -m 700 "$(dirname -- "$service_path")"
        legacy_service_path=$HOME/Library/LaunchAgents/$legacy_label.plist
        if [ "$render_only" = false ] && [ -f "$legacy_service_path" ]; then
            launchctl bootout "gui/$(id -u)" "$legacy_service_path" >/dev/null 2>&1 || true
            # Move it out of LaunchAgents: a copy left there loads again at the next login and holds the port.
            mv -f "$legacy_service_path" "$state_dir/service-backups/$stamp.legacy.plist"
            chmod 600 "$state_dir/service-backups/$stamp.legacy.plist"
        fi
        if [ -f "$service_path" ]; then
            install -m 600 "$service_path" "$state_dir/service-backups/$stamp.plist"
            if [ "$render_only" = false ]; then
                launchctl bootout "gui/$(id -u)" "$service_path" >/dev/null 2>&1 || true
            fi
        fi
        pending=$service_path.pending.$$
        trap 'case "${pending:-}" in *.pending.*) rm -f -- "$pending" ;; esac' EXIT HUP INT TERM
        {
            printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>'
            printf '%s\n' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
            printf '%s\n' '<plist version="1.0"><dict>'
            printf '  <key>Label</key><string>%s</string>\n' "$label"
            printf '%s\n' '  <key>ProgramArguments</key><array>'
            for argument in "$binary" --root "$root" --state-dir "$state_dir" --config-dir "$config_dir" --openshell-bin "$openshell_bin" --docker-bin "$docker_bin" dashboard; do
                printf '    <string>%s</string>\n' "$argument"
            done
            printf '%s\n' '  </array>'
            printf '%s\n' '  <key>RunAtLoad</key><true/>'
            printf '%s\n' '  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>'
            printf '  <key>StandardOutPath</key><string>%s/logs/dashboard.stdout.log</string>\n' "$state_dir"
            printf '  <key>StandardErrorPath</key><string>%s/logs/dashboard.stderr.log</string>\n' "$state_dir"
            printf '%s\n' '</dict></plist>'
        } > "$pending"
        chmod 600 "$pending"
        plutil -lint "$pending" >/dev/null
        mv -f "$pending" "$service_path"
        if [ "$render_only" = false ]; then
            launchctl bootstrap "gui/$(id -u)" "$service_path"
            launchctl kickstart -k "gui/$(id -u)/$label"
        fi
        ;;
    Linux)
        service_path=${service_path_override:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/enrichedlab-agent-sandbox-dashboard.service}
        service_dir=$(dirname -- "$service_path")
        install -d -m 700 "$service_dir"
        if [ -f "$service_path" ]; then
            install -m 600 "$service_path" "$state_dir/service-backups/$stamp.service"
        fi
        pending=$service_path.pending.$$
        trap 'case "${pending:-}" in *.pending.*) rm -f -- "$pending" ;; esac' EXIT HUP INT TERM
        {
            printf '%s\n' '[Unit]' 'Description=EnrichedLab Agent Sandbox operator dashboard' 'After=network-online.target' '' '[Service]'
            printf 'ExecStart="%s" --root "%s" --state-dir "%s" --config-dir "%s" --openshell-bin "%s" --docker-bin "%s" dashboard\n' "$binary" "$root" "$state_dir" "$config_dir" "$openshell_bin" "$docker_bin"
            printf '%s\n' 'Restart=on-failure' 'RestartSec=3' 'NoNewPrivileges=true' 'PrivateTmp=true' 'ProtectSystem=strict' 'ProtectHome=read-only' 'ProtectKernelTunables=true' 'ProtectKernelModules=true' 'ProtectControlGroups=true' 'RestrictSUIDSGID=true'
            printf 'ReadWritePaths="%s" "%s" "%s"\n' "$root" "$state_dir" "$config_dir"
            printf '%s\n' '' '[Install]' 'WantedBy=default.target'
        } > "$pending"
        chmod 600 "$pending"
        systemd-analyze --user verify "$pending"
        mv -f "$pending" "$service_path"
        if [ "$render_only" = false ]; then
            systemctl --user daemon-reload
            systemctl --user disable --now openshell-agent-sandbox-dashboard.service >/dev/null 2>&1 || true
            systemctl --user enable --now enrichedlab-agent-sandbox-dashboard.service
        fi
        ;;
    *) fail 'dashboard service installation supports macOS launchd and Linux systemd user services' ;;
esac

if [ "$render_only" = true ]; then
    printf 'Rendered dashboard service without loading it: %s\n' "$service_path"
else
    printf 'Installed dashboard service: %s\n' "$service_path"
fi
