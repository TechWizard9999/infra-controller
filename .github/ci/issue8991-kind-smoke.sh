#!/usr/bin/env bash
# Scratch-only integration test for issue 8991. Requires the disposable kind
# cluster created by target/8991-ci.yml; never point it at an operator cluster.
set -euo pipefail

[[ "$(kubectl config current-context)" == kind-issue8991 ]] || {
    echo 'This smoke test requires the disposable kind-issue8991 context.' >&2
    exit 1
}

REPO_ROOT="$(git rev-parse --show-toplevel)"
SCRIPT_DIR="${REPO_ROOT}/helm-prereqs"
output="${NAMESPACE_SMOKE_OUTPUT:?NAMESPACE_SMOKE_OUTPUT must name the artifact directory}"
mkdir -p "${output}"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

helper="$(sed -n '/^_prepare_core_legacy_alias_namespaces()/,/^}/p' "${SCRIPT_DIR}/setup.sh")"
[[ -n "${helper}" ]] || { echo 'Namespace helper not found in setup.sh.' >&2; exit 1; }
eval "${helper}"

chart="${work}/alias-chart"
mkdir -p "${chart}/templates"
cat > "${chart}/Chart.yaml" <<'YAML'
apiVersion: v2
name: issue8991-alias-contract
version: 0.1.0
type: application
YAML

base_cmd=(
    helm upgrade --install nico ./helm --namespace nico-system
    --set-string global.image.repository=registry.invalid/nico
    --set-string global.image.tag=namespace-smoke
    --timeout 600s --wait
)

render_alias_chart() {
    local scenario="$1"
    # Only Namespace and legacy ExternalName Service documents are retained.
    # All full Core renders, which can contain Secrets, stay in the private temp directory.
    (cd "${REPO_ROOT}" && helm template "${NICO_CORE_CMD[@]:3}") > "${work}/full-core.yaml"
    python3 - "${work}/full-core.yaml" "${chart}/templates/aliases.yaml" "${work}/aliases.tsv" <<'PY'
import pathlib
import sys
import yaml

with open(sys.argv[1]) as stream:
    documents = [doc for doc in yaml.safe_load_all(stream) if isinstance(doc, dict)]
services = [doc for doc in documents
            if doc.get('kind') == 'Service'
            and doc.get('spec', {}).get('type') == 'ExternalName'
            and 'nico.nvidia.com/legacy-alias-of' in doc.get('metadata', {}).get('labels', {})]
if not services:
    raise SystemExit('The full Core render contained no legacy alias Services.')
namespaces = {doc['metadata']['namespace'] for doc in services}
selected = [doc for doc in documents
            if doc in services or (doc.get('kind') == 'Namespace'
                                   and doc['metadata']['name'] in namespaces)]
with open(sys.argv[2], 'w') as stream:
    yaml.safe_dump_all(selected, stream, sort_keys=False)
pathlib.Path(sys.argv[3]).write_text(''.join(
    f"{doc['metadata']['namespace']}\t{doc['metadata']['name']}\t{doc['spec']['externalName']}\n"
    for doc in services))
PY
    cp "${chart}/templates/aliases.yaml" "${output}/${scenario}-aliases.yaml"
}

verify_services() {
    local namespace name target actual
    while IFS=$'\t' read -r namespace name target; do
        actual="$(kubectl get service "${name}" -n "${namespace}" \
            -o jsonpath='{.spec.type}{"|"}{.spec.externalName}')"
        [[ "${actual}" == "ExternalName|${target}" ]] || {
            echo "Alias ${namespace}/${name} has ${actual}; expected ExternalName|${target}." >&2
            return 1
        }
    done < "${work}/aliases.tsv"
}

namespace_metadata() {
    kubectl get namespace "$1" -o json |
        jq -S '.metadata | {uid, labels, annotations}'
}

assert_unowned_namespace() {
    kubectl get namespace "$1" -o json | jq -e '
        .metadata.labels["app.kubernetes.io/managed-by"] != "Helm"
        and .metadata.annotations["meta.helm.sh/release-name"] == null
        and .metadata.annotations["meta.helm.sh/release-namespace"] == null
    ' > /dev/null
}

deploy_aliases() {
    helm upgrade --install nico "${chart}" --namespace nico-system --wait --timeout 60s
    verify_services
}

kubectl create namespace nico-system

# Defaults reproduce the original API-server rejection before namespace preparation.
NICO_CORE_CMD=("${base_cmd[@]}")
render_alias_chart defaults
if helm install nico "${chart}" --namespace nico-system --wait --timeout 30s \
        > "${output}/defaults-before-helper.log" 2>&1; then
    echo 'Default alias installation unexpectedly succeeded without forge-system.' >&2
    exit 1
fi
grep -Eq 'namespaces?.*forge-system.*not found' "${output}/defaults-before-helper.log" || {
    echo 'Default installation failed for a reason other than the missing alias namespace.' >&2
    cat "${output}/defaults-before-helper.log" >&2
    exit 1
}
helm uninstall nico --namespace nico-system
_prepare_core_legacy_alias_namespaces
assert_unowned_namespace forge-system
deploy_aliases
namespace_metadata forge-system > "${work}/default-before.json"
_prepare_core_legacy_alias_namespaces
deploy_aliases
namespace_metadata forge-system > "${work}/default-after.json"
cmp "${work}/default-before.json" "${work}/default-after.json"
cp "${work}/default-after.json" "${output}/default-namespace-metadata.json"
helm uninstall nico --namespace nico-system
kubectl get namespace forge-system > /dev/null
echo 'PASS: default aliases fail before preparation, then install and upgrade; the setup-created namespace remains unowned and survives uninstall.'

# Exercise the same defaults with the exact Vault-free Core flags from setup.sh.
kubectl delete namespace forge-system --wait=true --timeout=60s
NICO_CORE_CMD=(
    "${base_cmd[@]}"
    --set global.certificate.issuerRef.name=site-issuer
    --set global.certificate.issuerRef.kind=ClusterIssuer
    --set global.certificate.issuerRef.group=cert-manager.io
    --set global.certificate.useCertManager=true
    --set global.certificate.useCertManagerUfm=true
    --set nico-api.vault.enabled=false
)
render_alias_chart skip-vault
_prepare_core_legacy_alias_namespaces
assert_unowned_namespace forge-system
deploy_aliases
helm uninstall nico --namespace nico-system
echo 'PASS: Vault-free Core values preserve the alias namespace contract.'

# Mixed custom values: preserve another owner's API namespace; create a PXE-only
# namespace; leave an API createNamespace opt-in for Helm, shared with a PXE alias.
kubectl create namespace smoke-existing-api
kubectl label namespace smoke-existing-api \
    app.kubernetes.io/managed-by=Helm issue8991.example/marker=retain
kubectl annotate namespace smoke-existing-api \
    meta.helm.sh/release-name=external-owner \
    meta.helm.sh/release-namespace=external-system \
    issue8991.example/annotation=retain
namespace_metadata smoke-existing-api > "${work}/existing-before.json"
cat > "${work}/custom-values.yaml" <<'YAML'
nico-api:
  legacyAlias:
    aliases:
      - name: api-retained
        namespace: smoke-existing-api
        createNamespace: false
      - name: api-chart-owned
        namespace: smoke-chart-owned
        createNamespace: true
nico-pxe:
  legacyAlias:
    aliases:
      - name: pxe-fresh
        namespace: smoke-pxe-only
      - name: pxe-chart-owned
        namespace: smoke-chart-owned
YAML
NICO_CORE_CMD=("${base_cmd[@]}" -f "${work}/custom-values.yaml")
render_alias_chart custom
_prepare_core_legacy_alias_namespaces
namespace_metadata smoke-existing-api > "${work}/existing-after.json"
cmp "${work}/existing-before.json" "${work}/existing-after.json"
assert_unowned_namespace smoke-pxe-only
[[ -z "$(kubectl get namespace smoke-chart-owned --ignore-not-found -o name)" ]] || {
    echo 'The helper pre-created the namespace explicitly owned by the chart.' >&2
    exit 1
}
deploy_aliases
kubectl get namespace smoke-chart-owned -o json | jq -e '
    .metadata.labels["app.kubernetes.io/managed-by"] == "Helm"
    and .metadata.annotations["meta.helm.sh/release-name"] == "nico"
    and .metadata.annotations["meta.helm.sh/release-namespace"] == "nico-system"
    and .metadata.annotations["helm.sh/resource-policy"] == "keep"
' > /dev/null
_prepare_core_legacy_alias_namespaces
deploy_aliases
namespace_metadata smoke-existing-api > "${output}/existing-namespace-after-rerun.json"
cmp "${work}/existing-before.json" "${output}/existing-namespace-after-rerun.json"
assert_unowned_namespace smoke-pxe-only
helm uninstall nico --namespace nico-system
# The chart's explicit keep policy and the helper's unmanaged namespaces both survive.
kubectl get namespace smoke-chart-owned smoke-existing-api smoke-pxe-only > /dev/null
echo 'PASS: custom API/PXE namespaces preserve other ownership, create missing namespaces, honor createNamespace opt-in, and support reruns.'

cat > "${output}/scope.txt" <<'TEXT'
Verified against a disposable Kubernetes API server:
- Missing default alias namespaces reproduce the Helm installation failure.
- The production setup.sh helper prepares them from a real full Core render.
- Actual rendered alias Services install and upgrade through a minimal Helm chart.
- Default, Vault-free, custom API/PXE, pre-existing ownership and createNamespace opt-in cases pass.
- Reruns preserve existing labels/annotations and created namespaces survive uninstall as intended.

This is an alias namespace/Helm ownership integration test. It does not deploy
NICo workloads, private images, cert-manager, databases, Vault, DPF or REST, and
does not verify a complete fresh-cluster NICo setup or application readiness.
TEXT
cat "${output}/scope.txt"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    cat "${output}/scope.txt" >> "${GITHUB_STEP_SUMMARY}"
fi
