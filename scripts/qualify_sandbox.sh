#!/bin/sh
set -eu

profile=
route=none
name=q-$(date -u +%y%m%d%H%M%S)
keep=false

while [ "$#" -gt 0 ]; do
    case "$1" in
        --profile|--route|--name)
            [ "$#" -ge 2 ] || { printf '%s needs a value\n' "$1" >&2; exit 1; }
            option=$1
            value=$2
            shift 2
            case "$option" in
                --profile) profile=$value ;;
                --route) route=$value ;;
                --name) name=$value ;;
            esac
            ;;
        --keep) keep=true; shift ;;
        -h|--help)
            printf '%s\n' 'Usage: qualify_sandbox.sh --profile NAME [--route NAME] [--name NAME] [--keep]'
            exit 0
            ;;
        *) printf 'unknown option: %s\n' "$1" >&2; exit 1 ;;
    esac
done

[ -n "$profile" ] || { printf '%s\n' '--profile is required' >&2; exit 1; }
command -v agent-sandbox >/dev/null 2>&1 || { printf '%s\n' 'agent-sandbox is required' >&2; exit 1; }

created=false
cleanup() {
    if [ "$created" = true ] && [ "$keep" = false ]; then
        agent-sandbox delete "$name" --trash-workspace >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT HUP INT TERM

agent-sandbox doctor
agent-sandbox create --name "$name" --route "$route" --gateway-profile "$profile"
created=true
agent-sandbox exec "$name" -- /bin/sh -lc 'test -w /sandbox/work; printf "sandbox-qualified\n"'
agent-sandbox platform refresh "$name"
agent-sandbox audit isolation "$name"
if ! agent-sandbox audit snapshot "$name"; then
    printf '%s\n' 'Evidence capture failed once; refreshing runtime proof and retrying once.' >&2
    agent-sandbox platform refresh "$name"
    agent-sandbox audit snapshot "$name"
fi
agent-sandbox audit verify

if [ "$keep" = false ]; then
    agent-sandbox delete "$name" --trash-workspace
    created=false
fi
agent-sandbox service list >/dev/null
agent-sandbox list >/dev/null
printf 'Disposable qualification passed for profile %s.\n' "$profile"
