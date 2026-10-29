#!/bin/sh
set -eu

umask 077

usage() {
    printf '%s\n' \
        'Usage: scripts/setup_local.sh [options]' \
        '' \
        "  --install-root PATH       Binary prefix (default: \$HOME/.local)" \
        '  --config-dir PATH         Controller config directory' \
        '  --state-dir PATH          Private controller state directory' \
        '  --workspace-root PATH     Default sandbox workspace root' \
        '  --profile DRIVER          docker, vm, or kubernetes' \
        '  --profile-name NAME       Controller gateway profile name' \
        '  --gateway NAME            Registered OpenShell gateway name' \
        '  --namespace NAME          Kubernetes sandbox namespace' \
        '  --runtime-class NAME      Kubernetes RuntimeClass' \
        '  --kubeconfig PATH         Kubernetes observer kubeconfig' \
        '  --context NAME            Kubernetes observer context' \
        '  --enable-peer-policies    Allow exact lease NetworkPolicies' \
        '  --peer-gateway-ip IP      Private controller IP for peer relays' \
        '  --install-openshell-cli   Install the tested OpenShell CLI binary' \
        '  --install-service         Install the user dashboard service' \
        '  --install-skills          Link the focused skills into supported harnesses' \
        '  --migrate-legacy          Copy prior product config/state and skill links' \
        '  --allow-experimental-wsl2 Continue inside WSL2 with an unqualified receipt' \
        '  --skip-doctor             Do not require the live preflight' \
        '  -h, --help                Show this help'
}

fail() {
    printf 'setup: %s\n' "$1" >&2
    exit 1
}

absolute_path() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) fail "paths must be absolute: $1" ;;
    esac
}

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)
install_root=${HOME:?HOME is not set}/.local
config_dir=${XDG_CONFIG_HOME:-$HOME/.config}/enrichedlab-agent-sandbox
state_dir=${XDG_STATE_HOME:-$HOME/.local/state}/enrichedlab-agent-sandbox
workspace_root=$HOME/agent-sandboxes
driver=docker
profile_name=
gateway=openshell
namespace=agent-sandboxes
runtime_class=
kubeconfig=
context=
enable_peer_policies=false
peer_gateway_ip=
install_openshell_cli=false
install_service=false
install_skills=false
migrate_legacy=false
skip_doctor=false
allow_experimental_wsl2=false

while [ "$#" -gt 0 ]; do
    case "$1" in
        --install-root|--config-dir|--state-dir|--workspace-root|--profile|--profile-name|--gateway|--namespace|--runtime-class|--kubeconfig|--context|--peer-gateway-ip)
            [ "$#" -ge 2 ] || fail "$1 needs a value"
            option=$1
            value=$2
            shift 2
            case "$option" in
                --install-root) install_root=$value ;;
                --config-dir) config_dir=$value ;;
                --state-dir) state_dir=$value ;;
                --workspace-root) workspace_root=$value ;;
                --profile) driver=$value ;;
                --profile-name) profile_name=$value ;;
                --gateway) gateway=$value ;;
                --namespace) namespace=$value ;;
                --runtime-class) runtime_class=$value ;;
                --kubeconfig) kubeconfig=$value ;;
                --context) context=$value ;;
                --peer-gateway-ip) peer_gateway_ip=$value ;;
            esac
            ;;
        --enable-peer-policies) enable_peer_policies=true; shift ;;
        --install-openshell-cli) install_openshell_cli=true; shift ;;
        --install-service) install_service=true; shift ;;
        --install-skills) install_skills=true; shift ;;
        --migrate-legacy) migrate_legacy=true; shift ;;
        --allow-experimental-wsl2) allow_experimental_wsl2=true; shift ;;
        --skip-doctor) skip_doctor=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown option: $1" ;;
    esac
done

install_root=$(absolute_path "$install_root")
config_dir=$(absolute_path "$config_dir")
state_dir=$(absolute_path "$state_dir")
workspace_root=$(absolute_path "$workspace_root")
if [ -n "$kubeconfig" ]; then
    kubeconfig=$(absolute_path "$kubeconfig")
fi

case "$driver" in
    docker|vm|kubernetes) ;;
    *) fail "unsupported profile driver: $driver" ;;
esac

if [ -z "$profile_name" ]; then
    case "$driver" in
        docker) profile_name=docker-local ;;
        vm) profile_name=vm-local ;;
        kubernetes) profile_name=kubernetes-kata ;;
    esac
fi

command -v cargo >/dev/null 2>&1 || fail 'cargo is required to build agent-sandbox'
command -v git >/dev/null 2>&1 || fail 'git is required'
command -v openssl >/dev/null 2>&1 || fail 'openssl is required'

if [ "$migrate_legacy" = true ]; then
    legacy_config=${XDG_CONFIG_HOME:-$HOME/.config}/openshell-agent-sandbox
    legacy_state=${XDG_STATE_HOME:-$HOME/.local/state}/openshell-agent-sandbox
    for pair in "$legacy_config|$config_dir" "$legacy_state|$state_dir"; do
        source_path=${pair%%|*}
        destination_path=${pair#*|}
        if [ -d "$source_path" ]; then
            [ ! -e "$destination_path" ] || fail "legacy migration will not merge into existing $destination_path"
            if [ "$source_path" = "$legacy_state" ] \
                && [ -e "$source_path/migration/legacy-evidence-copy" ]; then
                fail 'legacy state contains the reserved migration evidence path'
            fi
            install -d -m 700 "$(dirname -- "$destination_path")"
            cp -R -p "$source_path" "$destination_path"
            chmod -R go-rwx "$destination_path"
            if [ "$source_path" = "$legacy_state" ] && [ -d "$destination_path/evidence" ]; then
                install -d -m 700 "$destination_path/migration"
                mv "$destination_path/evidence" "$destination_path/migration/legacy-evidence-copy"
                install -d -m 700 "$destination_path/evidence"
                printf '%s\n' 'Historical evidence kept at its original path; a private migration copy was retained outside the active evidence index.'
            fi
            printf 'Copied prior private state for rollback-safe migration: %s\n' "$destination_path"
        fi
    done
fi

if [ -x /usr/bin/false ]; then
    unavailable_runtime_bin=/usr/bin/false
elif [ -x /bin/false ]; then
    unavailable_runtime_bin=/bin/false
else
    fail 'a false utility is required for profiles without a local container runtime'
fi

install -d -m 700 "$install_root" "$install_root/bin" "$config_dir" "$state_dir" "$workspace_root"

if [ "$install_openshell_cli" = true ]; then
    command -v curl >/dev/null 2>&1 || fail 'curl is required to install OpenShell'
    os=$(uname -s)
    arch=$(uname -m)
    case "$os/$arch" in
        Darwin/arm64)
            archive=openshell-aarch64-apple-darwin.tar.gz
            expected=e582f2374053bebac8e6aaeb4a369931b7d4bb97bd55055e2c02e85502627e22
            ;;
        Linux/x86_64)
            archive=openshell-x86_64-unknown-linux-musl.tar.gz
            expected=4fb4476d80a1875a0b83547ec3aba999cf0a2e2d75f95f2f709b622e2103520e
            ;;
        Linux/aarch64|Linux/arm64)
            archive=openshell-aarch64-unknown-linux-musl.tar.gz
            expected=7a949c48d1e000cd280869eea1e203e24816b9cfefc575b68a8b72b939cb3f43
            ;;
        *) fail "OpenShell 0.0.116 has no tested CLI archive for $os/$arch" ;;
    esac
    download_dir=$(mktemp -d "${TMPDIR:-/tmp}/openshell-install.XXXXXX")
    trap 'case "${download_dir:-}" in "${TMPDIR:-/tmp}"/openshell-install.*) rm -rf -- "$download_dir" ;; esac' EXIT HUP INT TERM
    curl --fail --location --silent --show-error \
        --output "$download_dir/$archive" \
        "https://github.com/NVIDIA/OpenShell/releases/download/v0.0.116/$archive"
    if command -v sha256sum >/dev/null 2>&1; then
        observed=$(sha256sum "$download_dir/$archive" | awk '{print $1}')
    else
        observed=$(shasum -a 256 "$download_dir/$archive" | awk '{print $1}')
    fi
    [ "$observed" = "$expected" ] || fail 'OpenShell archive checksum mismatch'
    tar -xzf "$download_dir/$archive" -C "$download_dir"
    [ -x "$download_dir/openshell" ] || fail 'OpenShell archive has no executable'
    install -m 755 "$download_dir/openshell" "$install_root/bin/openshell"
fi

if ! command -v openshell >/dev/null 2>&1 && [ ! -x "$install_root/bin/openshell" ]; then
    fail 'OpenShell CLI is missing; install it first or pass --install-openshell-cli'
fi

if [ "$install_openshell_cli" = true ]; then
    openshell_bin=$install_root/bin/openshell
else
    openshell_bin=$(command -v openshell)
fi
case "$openshell_bin" in
    /*) ;;
    *) fail "OpenShell did not resolve to an absolute executable: $openshell_bin" ;;
esac

if [ "$(uname -s)" = Linux ] && [ -r /proc/sys/kernel/osrelease ] \
    && grep -qi microsoft /proc/sys/kernel/osrelease \
    && [ "$allow_experimental_wsl2" = false ]; then
    fail 'WSL2 is experimental and unqualified; rerun only with --allow-experimental-wsl2 after accepting that limit'
fi
[ -x "$openshell_bin" ] || fail "OpenShell is not executable: $openshell_bin"

case "$driver" in
    docker)
        command -v docker >/dev/null 2>&1 || fail 'Docker is required for the Docker profile'
        docker_bin=$(command -v docker)
        ;;
    kubernetes)
        command -v kubectl >/dev/null 2>&1 || fail 'kubectl is required for the Kubernetes profile'
        kubectl_bin=$(command -v kubectl)
        if command -v docker >/dev/null 2>&1; then
            docker_bin=$(command -v docker)
        else
            docker_bin=$unavailable_runtime_bin
        fi
        ;;
    vm)
        if command -v docker >/dev/null 2>&1; then
            docker_bin=$(command -v docker)
        else
            docker_bin=$unavailable_runtime_bin
        fi
        ;;
esac

cargo install --path "$repo_root" --locked --root "$install_root" --force
agent_bin=$install_root/bin/agent-sandbox

if [ ! -f "$config_dir/controller.toml" ]; then
    install -m 600 "$repo_root/examples/controller.toml" "$config_dir/controller.toml"
fi
if [ ! -f "$config_dir/routes.json" ]; then
    install -m 600 "$repo_root/examples/routes.json" "$config_dir/routes.json"
fi

set -- \
    --root "$workspace_root" \
    --state-dir "$state_dir" \
    --config-dir "$config_dir" \
    --openshell-bin "$openshell_bin" \
    --docker-bin "$docker_bin" \
    platform configure \
    --name "$profile_name" \
    --gateway "$gateway" \
    --driver "$driver" \
    --required \
    --replace \
    --set-default
if [ "$driver" = kubernetes ]; then
    set -- "$@" --namespace "$namespace" --kubectl-bin "$kubectl_bin"
    if [ -n "$runtime_class" ]; then
        set -- "$@" --runtime-class "$runtime_class"
    fi
    if [ -n "$kubeconfig" ]; then
        set -- "$@" --kubeconfig "$kubeconfig"
    fi
    if [ -n "$context" ]; then
        set -- "$@" --context "$context"
    fi
    if [ "$enable_peer_policies" = true ]; then
        set -- "$@" --enable-peer-policies
    fi
    if [ -n "$peer_gateway_ip" ]; then
        set -- "$@" --peer-gateway-ip "$peer_gateway_ip"
    fi
fi
"$agent_bin" "$@"
if [ "$profile_name" != docker-local ] && grep -q '^\[gateway_profiles\.docker-local\]$' "$config_dir/controller.toml"; then
    "$agent_bin" \
        --root "$workspace_root" \
        --state-dir "$state_dir" \
        --config-dir "$config_dir" \
        --openshell-bin "$openshell_bin" \
        --docker-bin "$docker_bin" \
        platform remove docker-local
fi

if [ "$install_service" = true ]; then
    "$script_dir/install_dashboard_service.sh" \
        --binary "$agent_bin" \
        --openshell-bin "$openshell_bin" \
        --docker-bin "$docker_bin" \
        --root "$workspace_root" \
        --state-dir "$state_dir" \
        --config-dir "$config_dir"
fi

if [ "$install_skills" = true ]; then
    set -- "$script_dir/install_agent_skills.sh" --all --state-dir "$state_dir"
    if [ "$migrate_legacy" = true ]; then
        set -- "$@" --migrate-legacy
    fi
    "$@"
fi

if [ "$skip_doctor" = false ]; then
    "$agent_bin" \
        --root "$workspace_root" \
        --state-dir "$state_dir" \
        --config-dir "$config_dir" \
        --openshell-bin "$openshell_bin" \
        --docker-bin "$docker_bin" \
        doctor
fi

printf '%s\n' \
    "Installed: $agent_bin" \
    "Configuration: $config_dir" \
    "Private state: $state_dir" \
    "Workspaces: $workspace_root" \
    "Shell PATH: export PATH=$install_root/bin:\$PATH" \
    "Dashboard: $agent_bin --root $workspace_root --state-dir $state_dir --config-dir $config_dir --openshell-bin $openshell_bin --docker-bin $docker_bin dashboard"
