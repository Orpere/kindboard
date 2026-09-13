//! Provisioning URLs and manifest constants.
//!
//! `docs/dependency-install-matrix.md` and `docs/contracts.md` §4 are the
//! source of truth; values verified 2026-09-12 (see the architecture doc's
//! verification log).
//!
//! Every downloaded manifest is pinned to an immutable release tag **and**
//! carries a SHA-256 digest constant checked at install time. Manifest
//! digests are best-effort pinning: the upstream projects publish no
//! checksums for these YAML files, so the digest records the exact content
//! at the pinned tag (raw.githubusercontent.com serves tag-immutable
//! content over HTTPS).

/// Flannel CNI manifest (`kube-flannel.yml`), pinned to release v0.28.9
/// (latest release, 2026-09-12). Pod CIDR is patched into
/// `net-conf.json.Network` when it differs from flannel's default
/// `10.244.0.0/16`.
pub const FLANNEL_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/flannel-io/flannel/v0.28.9/Documentation/kube-flannel.yml";

/// SHA-256 of [`FLANNEL_MANIFEST_URL`] (best-effort pin, no upstream digest).
pub const FLANNEL_MANIFEST_SHA256: &str =
    "e875824be2f552b45711dbda91af81b17eb961d00025d914d9fef18fad8f09c0";

/// Flannel's default pod network (matches kind's default podSubnet).
pub const FLANNEL_DEFAULT_NETWORK: &str = "10.244.0.0/16";

/// Calico (Tigera) operator manifest, v3.32.0.
pub const TIGERA_OPERATOR_URL: &str =
    "https://raw.githubusercontent.com/projectcalico/calico/v3.32.0/manifests/tigera-operator.yaml";

/// SHA-256 of [`TIGERA_OPERATOR_URL`] (best-effort pin, no upstream digest).
pub const TIGERA_OPERATOR_SHA256: &str =
    "e48fe027f8be3d9136a012a32450f1eabc4c5257c0b76083cd1ab32316637d47";

/// ingress-nginx kind deployment manifest (controller-v1.12.1).
pub const INGRESS_NGINX_KIND_DEPLOY_URL: &str = "https://raw.githubusercontent.com/kubernetes/ingress-nginx/controller-v1.12.1/deploy/static/provider/kind/deploy.yaml";

/// SHA-256 of [`INGRESS_NGINX_KIND_DEPLOY_URL`] (best-effort pin, no
/// upstream digest).
pub const INGRESS_NGINX_KIND_DEPLOY_SHA256: &str =
    "cb84b0ea747c9149cce08aef4e95b1e55f183f07299f40e7009043e960a0133f";

/// Gateway API standard CRDs (server-side applied before enabling Cilium's
/// Gateway API controller).
pub const GATEWAY_API_CRDS_URL: &str =
    "https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.4.0/standard-install.yaml";

/// SHA-256 of [`GATEWAY_API_CRDS_URL`] (best-effort pin, no upstream
/// digest).
pub const GATEWAY_API_CRDS_SHA256: &str =
    "6a4029e661446d64add866a00ecdc40c14219b68777ab614c5cdaac0adb481f1";

/// Traefik Helm chart repository.
pub const TRAEFIK_HELM_REPO_URL: &str = "https://traefik.github.io/charts";

/// Traefik Helm repo name.
pub const TRAEFIK_HELM_REPO_NAME: &str = "traefik";

/// Traefik Helm release/chart reference.
pub const TRAEFIK_HELM_CHART: &str = "traefik/traefik";

/// Namespace used for the Traefik release.
pub const TRAEFIK_NAMESPACE: &str = "kube-system";

/// Namespace used by the ingress-nginx kind deploy manifest.
pub const INGRESS_NGINX_NAMESPACE: &str = "ingress-nginx";

/// Timeout used for node-ready wait verification.
pub const NODE_READY_TIMEOUT: &str = "2m";

/// Budget for polling on the tigera operator's CRDs to appear (the
/// operator pod registers them at startup; applying the Installation CR
/// before that fails with "no matches for kind Installation").
pub const CALICO_CRD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Cilium helm value forcing kube-proxy replacement off for kind (kind
/// manages its own kube-proxy). Cilium 1.20+ validates this strictly:
/// only `true`/`false` are accepted (the old `disabled` keyword errors
/// with "kubeProxyReplacement must be explicitly set to a valid value").
pub const CILIUM_SET_KUBE_PROXY_DISABLED: &str = "kubeProxyReplacement=false";

/// Cilium helm value enabling the ingress controller.
pub const CILIUM_SET_INGRESS_ENABLED: &str = "ingressController.enabled=true";

/// Cilium helm value enabling the Gateway API controller.
pub const CILIUM_SET_GATEWAY_API_ENABLED: &str = "gatewayAPI.enabled=true";

/// Cilium clustermesh apiserver service type for kind (no LB).
pub const CILIUM_SET_MESH_NODE_PORT: &str = "clustermesh.apiserver.service.type=NodePort";

/// Render the Calico `Installation` custom resource with the pod CIDR baked
/// in (contracts §4: operator path, `ipPools[0].cidr = pod_cidr`).
pub fn calico_installation_cr(pod_cidr: &str) -> String {
    format!(
        "apiVersion: operator.tigera.io/v1\n\
         kind: Installation\n\
         metadata:\n\
         \x20 name: default\n\
         spec:\n\
         \x20 calicoNetwork:\n\
         \x20   ipPools:\n\
         \x20   - blockSize: 26\n\
         \x20     cidr: {pod_cidr}\n\
         \x20     encapsulation: VXLANCrossSubnet\n\
         \x20     natOutgoing: Enabled\n\
         \x20     nodeSelector: all()\n"
    )
}

/// Patch `net-conf.json`'s `Network` value inside the kube-flannel manifest
/// when the pod CIDR differs from flannel's default.
pub fn patch_flannel_network(manifest: &str, pod_cidr: &str) -> Option<String> {
    let needle = format!("\"Network\": \"{}\"", FLANNEL_DEFAULT_NETWORK);
    let replacement = format!("\"Network\": \"{pod_cidr}\"");
    if manifest.contains(&needle) {
        Some(manifest.replace(&needle, &replacement))
    } else if manifest.contains(&replacement) {
        Some(manifest.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flannel_patch_replaces_default_network() {
        let manifest = r#"apiVersion: v1
kind: ConfigMap
data:
  net-conf.json: |
    {
      "Network": "10.244.0.0/16",
      "Backend": {"Type": "vxlan"}
    }
"#;
        let patched = patch_flannel_network(manifest, "10.99.0.0/16").unwrap();
        assert!(
            patched.contains("\"Network\": \"10.99.0.0/16\""),
            "{patched}"
        );
        assert!(
            !patched.contains("\"Network\": \"10.244.0.0/16\""),
            "{patched}"
        );
    }

    #[test]
    fn flannel_patch_noop_when_already_matching() {
        let manifest = "data: \"Network\": \"10.99.0.0/16\"";
        let patched = patch_flannel_network(manifest, "10.99.0.0/16").unwrap();
        assert_eq!(patched, manifest);
    }

    #[test]
    fn flannel_patch_missing_needle_is_none() {
        assert!(patch_flannel_network("nothing here", "10.99.0.0/16").is_none());
    }

    #[test]
    fn calico_cr_contains_cidr() {
        let cr = calico_installation_cr("192.168.0.0/16");
        assert!(cr.contains("kind: Installation"), "{cr}");
        assert!(cr.contains("cidr: 192.168.0.0/16"), "{cr}");
        assert!(cr.contains("name: default"), "{cr}");
        assert!(cr.contains("operator.tigera.io/v1"), "{cr}");
    }
}
