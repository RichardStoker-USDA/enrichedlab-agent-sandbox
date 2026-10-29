#!/bin/sh
set -eu

namespace=agent-sandboxes
runtime_class=
context=
kubeconfig=
kubectl_bin=$(command -v kubectl 2>/dev/null || true)
peer=false

while [ "$#" -gt 0 ]; do
    case "$1" in
        --namespace|--runtime-class|--context|--kubeconfig|--kubectl-bin)
            [ "$#" -ge 2 ] || { printf '%s needs a value\n' "$1" >&2; exit 1; }
            option=$1
            value=$2
            shift 2
            case "$option" in
                --namespace) namespace=$value ;;
                --runtime-class) runtime_class=$value ;;
                --context) context=$value ;;
                --kubeconfig) kubeconfig=$value ;;
                --kubectl-bin) kubectl_bin=$value ;;
            esac
            ;;
        --peer) peer=true; shift ;;
        -h|--help)
            printf '%s\n' 'Usage: preflight_kubernetes.sh [--kubectl-bin PATH] [--kubeconfig PATH] [--context NAME] [--namespace NAME] --runtime-class NAME [--peer]'
            exit 0
            ;;
        *) printf 'unknown option: %s\n' "$1" >&2; exit 1 ;;
    esac
done

[ -n "$runtime_class" ] || { printf '%s\n' '--runtime-class is required' >&2; exit 1; }
[ -n "$kubectl_bin" ] && [ -x "$kubectl_bin" ] || { printf '%s\n' 'an executable kubectl-compatible binary is required' >&2; exit 1; }
case "$kubectl_bin" in
    /*) ;;
    *) printf '%s\n' '--kubectl-bin must resolve to an absolute path' >&2; exit 1 ;;
esac
if [ -n "$kubeconfig" ]; then
    case "$kubeconfig" in
        /*) ;;
        *) printf '%s\n' '--kubeconfig must be an absolute path' >&2; exit 1 ;;
    esac
    [ -f "$kubeconfig" ] || { printf '%s\n' 'kubeconfig is not a regular file' >&2; exit 1; }
fi

set -- "$kubectl_bin"
if [ -n "$kubeconfig" ]; then
    set -- "$@" --kubeconfig "$kubeconfig"
fi
if [ -n "$context" ]; then
    set -- "$@" --context "$context"
fi

"$@" version --output=json >/dev/null
"$@" get namespace "$namespace" >/dev/null
"$@" get crd sandboxes.agents.x-k8s.io >/dev/null
"$@" get runtimeclass.node.k8s.io "$runtime_class" -o jsonpath='{.handler}' | awk 'NF { found=1 } END { exit found ? 0 : 1 }'
"$@" auth can-i get pods --namespace "$namespace" | grep -qx yes
"$@" auth can-i list sandboxes.agents.x-k8s.io --namespace "$namespace" | grep -qx yes
"$@" auth can-i get runtimeclasses.node.k8s.io | grep -qx yes
"$@" auth can-i list networkpolicies.networking.k8s.io --namespace "$namespace" | grep -qx yes

if [ "$peer" = true ]; then
    "$@" auth can-i create networkpolicies.networking.k8s.io --namespace "$namespace" | grep -qx yes
    "$@" auth can-i delete networkpolicies.networking.k8s.io --namespace "$namespace" | grep -qx yes
fi

printf '%s\n' \
    "Kubernetes API preflight passed for namespace $namespace and RuntimeClass $runtime_class." \
    'This proves API presence and caller permission. Run the disposable qualification to prove Pod, CNI, OpenShell, and microVM behavior.'
