//! Cluster specification: validated user input for a kind cluster, and the
//! exact kind v1alpha4 config YAML it maps to.
//!
//! Types mirror `docs/contracts.md` §1. All invariants are checked in
//! [`validate`]; nothing here ever panics on bad input (invalid values are
//! rejected up front, before any subprocess is spawned).

mod cidr;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Default pod CIDR used by kind itself (kindnet's network).
pub const DEFAULT_POD_CIDR: &str = "10.244.0.0/16";
/// Default service CIDR used by kind itself.
pub const DEFAULT_SERVICE_CIDR: &str = "10.96.0.0/16";
/// Default Kubernetes version (kindest/node tag) for kind 0.33.x.
pub const DEFAULT_K8S_VERSION: &str = "1.37.0";
/// Maximum allowed length of a cluster name (kind DNS label constraint).
pub const MAX_NAME_LEN: usize = 32;
/// Maximum sane number of workers (control-plane is always exactly 1).
pub const MAX_WORKERS: u32 = 32;
/// Cilium clustermesh cluster-id range lower bound.
pub const MIN_CLUSTER_ID: u16 = 1;
/// Cilium clustermesh cluster-id range upper bound.
pub const MAX_CLUSTER_ID: u16 = 255;

/// Full user specification for a managed kind cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterSpec {
    /// DNS-ish name `[a-z0-9-]`, ≤ [`MAX_NAME_LEN`]; also the kind cluster
    /// name and the kubeconfig context name.
    pub name: String,
    /// Kubernetes version; maps to the node image tag `kindest/node:v<ver>`.
    pub k8s_version: KubernetesVersion,
    /// Which CNI to run.
    pub cni: Cni,
    /// Pod network CIDR. Must match the CNI's network (see contracts §4).
    pub pod_cidr: String,
    /// Service network CIDR.
    pub service_cidr: String,
    /// Number of worker nodes (the control-plane is always exactly 1).
    pub worker_count: u32,
    /// Host→container port mappings for the control-plane node (80/443 for
    /// ingress, plus anything else).
    pub extra_port_mappings: Vec<PortMapping>,
    /// Kubernetes feature gates to pass through to the cluster config.
    pub feature_gates: BTreeMap<String, bool>,
    /// Which ingress controller to install, if any.
    pub ingress: Option<IngressController>,
    /// Cilium extras; must be `Some` iff `cni == Cilium`.
    pub cilium: Option<CiliumOptions>,
}

impl Default for ClusterSpec {
    fn default() -> Self {
        ClusterSpec {
            name: String::new(),
            k8s_version: KubernetesVersion::default(),
            cni: Cni::KindnetDefault,
            pod_cidr: DEFAULT_POD_CIDR.to_string(),
            service_cidr: DEFAULT_SERVICE_CIDR.to_string(),
            worker_count: 0,
            extra_port_mappings: Vec::new(),
            feature_gates: BTreeMap::new(),
            ingress: None,
            cilium: None,
        }
    }
}

/// Supported CNI choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cni {
    /// kind's built-in default CNI (kindnet; `disableDefaultCNI` stays
    /// false).
    KindnetDefault,
    /// Flannel (`kube-flannel.yml`, network must match `pod_cidr`).
    Flannel,
    /// Calico (Tigera operator + Installation CR).
    Calico,
    /// Cilium (via the cilium CLI; enables the extras).
    Cilium,
}

/// Ingress controller choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IngressController {
    /// ingress-nginx (kind deploy.yaml variant).
    Nginx,
    /// Traefik via Helm.
    Traefik,
    /// Cilium ingress controller; only valid when `cni == Cilium` and
    /// `cilium.ingress == true`.
    Cilium,
}

/// Cilium extras configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CiliumOptions {
    /// Enable the Gateway API controller (`gatewayAPI.enabled=true`).
    pub api_gateway: bool,
    /// Enable Hubble (relay + UI).
    pub hubble: bool,
    /// Enable the Cilium ingress controller
    /// (`ingressController.enabled=true`).
    pub ingress: bool,
    /// Enable clustermesh.
    pub mesh: bool,
    /// Cluster id in the mesh; required (1..=255) iff `mesh`.
    pub cluster_id: u16,
    /// Cluster name in the mesh; required (valid DNS label ≤ 32) iff `mesh`.
    pub cluster_name: String,
}

/// IP protocol for a port mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    /// TCP.
    #[serde(rename = "TCP")]
    Tcp,
    /// UDP.
    #[serde(rename = "UDP")]
    Udp,
    /// SCTP.
    #[serde(rename = "SCTP")]
    Sctp,
}

/// One host→container port mapping (maps onto kind's `extraPortMappings`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortMapping {
    /// Container (node) port.
    pub container_port: u16,
    /// Host port.
    pub host_port: u16,
    /// Address to listen on, e.g. `127.0.0.1` or `0.0.0.0`.
    pub listen_address: String,
    /// Protocol of the mapping.
    pub protocol: Protocol,
}

/// A validated Kubernetes version string like `1.37.0`.
///
/// Serializes transparently as a string. Constructing via [`FromStr`] or
/// [`KubernetesVersion::new`] validates the `X.Y.Z` shape; a value obtained
/// by deserialization is re-checked by [`validate`] before use.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KubernetesVersion(String);

impl KubernetesVersion {
    /// Create a version, validating the `X.Y.Z` shape.
    pub fn new(version: impl Into<String>) -> Result<Self> {
        let version = version.into();
        if is_semver_3(&version) {
            Ok(KubernetesVersion(version))
        } else {
            Err(CoreError::InvalidSpec(format!(
                "invalid kubernetes version {version:?}: expected X.Y.Z"
            )))
        }
    }

    /// The version string, without any leading `v`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The kind node image tag for this version, e.g.
    /// `kindest/node:v1.37.0`.
    pub fn node_image_tag(&self) -> String {
        format!("kindest/node:v{}", self.0)
    }

    /// Whether the string has a valid `X.Y.Z` shape (used by `validate` for
    /// deserialized values).
    pub fn is_well_formed(&self) -> bool {
        is_semver_3(&self.0)
    }
}

impl Default for KubernetesVersion {
    fn default() -> Self {
        KubernetesVersion(DEFAULT_K8S_VERSION.to_string())
    }
}

impl fmt::Display for KubernetesVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for KubernetesVersion {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self> {
        KubernetesVersion::new(s)
    }
}

fn is_semver_3(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Validate all invariants of a [`ClusterSpec`] (contracts §1).
///
/// Fails fast with the first violated invariant as
/// [`CoreError::InvalidSpec`]. No side effects.
pub fn validate(spec: &ClusterSpec) -> Result<()> {
    validate_name(&spec.name)?;

    if !spec.k8s_version.is_well_formed() {
        return Err(CoreError::InvalidSpec(format!(
            "invalid kubernetes version {:?}: expected X.Y.Z",
            spec.k8s_version.as_str()
        )));
    }

    match (&spec.cni, &spec.cilium) {
        (Cni::Cilium, Some(_)) => {}
        (Cni::Cilium, None) => {
            return Err(CoreError::InvalidSpec(
                "cni is Cilium but cilium options are missing".to_string(),
            ));
        }
        (_, Some(_)) => {
            return Err(CoreError::InvalidSpec(
                "cilium options are set but cni is not Cilium".to_string(),
            ));
        }
        (_, None) => {}
    }

    if let Some(cilium) = &spec.cilium {
        if cilium.mesh {
            if !(MIN_CLUSTER_ID..=MAX_CLUSTER_ID).contains(&cilium.cluster_id) {
                return Err(CoreError::InvalidSpec(format!(
                    "cilium clustermesh cluster id {} is out of range {MIN_CLUSTER_ID}..={MAX_CLUSTER_ID}",
                    cilium.cluster_id
                )));
            }
            if cilium.cluster_name.is_empty() {
                return Err(CoreError::InvalidSpec(
                    "cilium clustermesh requires a cluster name".to_string(),
                ));
            }
            validate_name(&cilium.cluster_name)?;
        }
        if !cilium.cluster_name.is_empty() && !cilium.mesh {
            validate_name(&cilium.cluster_name)?;
        }
    }

    if spec.ingress == Some(IngressController::Cilium) {
        let ok = match (spec.cni, &spec.cilium) {
            (Cni::Cilium, Some(cilium)) => cilium.ingress,
            _ => false,
        };
        if !ok {
            return Err(CoreError::InvalidSpec(
                "cilium ingress controller requires cni == Cilium and cilium.ingress == true"
                    .to_string(),
            ));
        }
    }

    if spec.worker_count > MAX_WORKERS {
        return Err(CoreError::InvalidSpec(format!(
            "worker_count {} exceeds maximum {MAX_WORKERS}",
            spec.worker_count
        )));
    }

    validate_port_mappings(spec)?;

    let pod = cidr::Ipv4Cidr::parse(&spec.pod_cidr)
        .ok_or_else(|| CoreError::InvalidSpec(format!("invalid pod_cidr {:?}", spec.pod_cidr)))?;
    let service = cidr::Ipv4Cidr::parse(&spec.service_cidr).ok_or_else(|| {
        CoreError::InvalidSpec(format!("invalid service_cidr {:?}", spec.service_cidr))
    })?;
    if pod.overlaps(&service) {
        return Err(CoreError::InvalidSpec(format!(
            "pod_cidr {} overlaps service_cidr {}",
            pod, service
        )));
    }

    Ok(())
}

/// Validate a cluster/mesh name: `^[a-z0-9][a-z0-9-]*$`, length 1..=32.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(CoreError::InvalidSpec("cluster name is empty".to_string()));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(CoreError::InvalidSpec(format!(
            "cluster name {name:?} is longer than {MAX_NAME_LEN} characters"
        )));
    }
    let mut chars = name.bytes();
    let first = chars.next().unwrap_or(0);
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(CoreError::InvalidSpec(format!(
            "cluster name {name:?} must start with a lowercase letter or digit"
        )));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(CoreError::InvalidSpec(format!(
            "cluster name {name:?} may only contain [a-z0-9-]"
        )));
    }
    if name.ends_with('-') {
        return Err(CoreError::InvalidSpec(format!(
            "cluster name {name:?} may not end with '-'"
        )));
    }
    Ok(())
}

fn validate_port_mappings(spec: &ClusterSpec) -> Result<()> {
    let mut seen_host_ports: Vec<u16> = Vec::new();
    for mapping in &spec.extra_port_mappings {
        if mapping.host_port == 0 {
            return Err(CoreError::InvalidSpec(
                "port mapping host_port must not be 0".to_string(),
            ));
        }
        if mapping.container_port == 0 {
            return Err(CoreError::InvalidSpec(
                "port mapping container_port must not be 0".to_string(),
            ));
        }
        if seen_host_ports.contains(&mapping.host_port) {
            return Err(CoreError::InvalidSpec(format!(
                "duplicate host_port {} in port mappings",
                mapping.host_port
            )));
        }
        seen_host_ports.push(mapping.host_port);
        mapping
            .listen_address
            .parse::<std::net::Ipv4Addr>()
            .map_err(|_| {
                CoreError::InvalidSpec(format!(
                    "invalid listen_address {:?}: expected an IPv4 address",
                    mapping.listen_address
                ))
            })?;
    }

    let wants_ingress_ports = matches!(
        spec.ingress,
        Some(IngressController::Nginx) | Some(IngressController::Traefik)
    );
    let has_http = seen_host_ports.contains(&80);
    let has_https = seen_host_ports.contains(&443);
    if wants_ingress_ports != (has_http && has_https) {
        return Err(CoreError::InvalidSpec(format!(
            "host ports 80/443 must be present iff ingress is Nginx or Traefik \
             (ingress: {wants_ingress_ports}, ports 80/443 mapped: {has_http}/{has_https})"
        )));
    }
    Ok(())
}

/// The rendered kind v1alpha4 cluster config for a spec.
///
/// Produced by [`to_kind_yaml`]; `yaml` is the exact text handed to
/// `kind create cluster --config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindConfig {
    /// Full YAML document.
    pub yaml: String,
}

impl KindConfig {
    /// The YAML document as a string.
    pub fn as_str(&self) -> &str {
        &self.yaml
    }
}

impl fmt::Display for KindConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.yaml)
    }
}

/// Render a [`ClusterSpec`] into its kind v1alpha4 config (contracts §1.1).
///
/// The mapping is exact and documented:
/// - `kind: Cluster`, `apiVersion: kind.x-k8s.io/v1alpha4`
/// - `featureGates` only when non-empty
/// - `networking`: `ipFamily: ipv4`, `podSubnet` (only if non-default or
///   CNI != kindnet), `serviceSubnet`, `disableDefaultCNI: cni != KindnetDefault`
/// - nodes: exactly one control-plane + `worker_count` workers, all with the
///   version node image; `extraPortMappings` on the control-plane.
pub fn to_kind_yaml(spec: &ClusterSpec) -> String {
    let kind_spec = KindConfigSerde {
        kind: "Cluster".to_string(),
        api_version: "kind.x-k8s.io/v1alpha4".to_string(),
        name: spec.name.clone(),
        feature_gates: if spec.feature_gates.is_empty() {
            None
        } else {
            Some(spec.feature_gates.clone())
        },
        networking: KindNetworking {
            ip_family: Some("ipv4".to_string()),
            pod_subnet: if spec.pod_cidr != DEFAULT_POD_CIDR || spec.cni != Cni::KindnetDefault {
                Some(spec.pod_cidr.clone())
            } else {
                None
            },
            service_subnet: spec.service_cidr.clone(),
            disable_default_cni: spec.cni != Cni::KindnetDefault,
        },
        nodes: build_nodes(spec),
    };
    serde_yaml::to_string(&kind_spec).unwrap_or_else(|_| {
        // `KindConfigSerde` only contains plain strings/ints/bools/maps, so
        // serialization cannot fail; fall back defensively to an empty doc
        // rather than panic.
        String::new()
    })
}

/// Render a [`ClusterSpec`] into a [`KindConfig`] (see [`to_kind_yaml`]).
pub fn to_kind_config(spec: &ClusterSpec) -> KindConfig {
    KindConfig {
        yaml: to_kind_yaml(spec),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KindConfigSerde {
    kind: String,
    #[serde(rename = "apiVersion")]
    api_version: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    feature_gates: Option<BTreeMap<String, bool>>,
    networking: KindNetworking,
    nodes: Vec<KindNode>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KindNetworking {
    #[serde(skip_serializing_if = "Option::is_none")]
    ip_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pod_subnet: Option<String>,
    service_subnet: String,
    #[serde(rename = "disableDefaultCNI")]
    disable_default_cni: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KindNode {
    role: String,
    image: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    extra_port_mappings: Vec<KindPortMapping>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KindPortMapping {
    container_port: u16,
    host_port: u16,
    listen_address: String,
    protocol: Protocol,
}

fn build_nodes(spec: &ClusterSpec) -> Vec<KindNode> {
    let image = spec.k8s_version.node_image_tag();
    let mappings: Vec<KindPortMapping> = spec
        .extra_port_mappings
        .iter()
        .map(|m| KindPortMapping {
            container_port: m.container_port,
            host_port: m.host_port,
            listen_address: m.listen_address.clone(),
            protocol: m.protocol,
        })
        .collect();
    let mut nodes = vec![KindNode {
        role: "control-plane".to_string(),
        image: image.clone(),
        extra_port_mappings: mappings,
    }];
    for _ in 0..spec.worker_count {
        nodes.push(KindNode {
            role: "worker".to_string(),
            image: image.clone(),
            extra_port_mappings: Vec::new(),
        });
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_spec() -> ClusterSpec {
        ClusterSpec {
            name: "demo".to_string(),
            ..ClusterSpec::default()
        }
    }

    fn port(http: u16, https: u16) -> Vec<PortMapping> {
        vec![
            PortMapping {
                container_port: http,
                host_port: 80,
                listen_address: "127.0.0.1".to_string(),
                protocol: Protocol::Tcp,
            },
            PortMapping {
                container_port: https,
                host_port: 443,
                listen_address: "127.0.0.1".to_string(),
                protocol: Protocol::Tcp,
            },
        ]
    }

    // ---- validation ----

    #[test]
    fn default_spec_is_valid() {
        let mut spec = base_spec();
        spec.name = "a".to_string();
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn rejects_bad_names() {
        for name in [
            "",
            "Uppercase",
            "-leading",
            "trailing-",
            "has space",
            "under_score",
            "a.b.c",
        ] {
            let mut spec = base_spec();
            spec.name = name.to_string();
            assert!(validate(&spec).is_err(), "name {name:?} should fail");
        }
        let mut spec = base_spec();
        spec.name = "a".repeat(MAX_NAME_LEN + 1);
        assert!(validate(&spec).is_err());
    }

    #[test]
    fn accepts_valid_names() {
        for name in ["a", "demo", "demo-1", "1-cluster", "a-2-b-3"] {
            let mut spec = base_spec();
            spec.name = name.to_string();
            assert!(validate(&spec).is_ok(), "name {name:?} should pass");
        }
    }

    #[test]
    fn rejects_bad_cidrs_and_overlap() {
        let mut spec = base_spec();
        spec.pod_cidr = "banana".to_string();
        assert!(validate(&spec).is_err());

        let mut spec = base_spec();
        spec.service_cidr = "10.244.0.0/16".to_string();
        spec.pod_cidr = "10.244.0.0/16".to_string();
        assert!(validate(&spec).is_err(), "overlapping cidrs must fail");
    }

    #[test]
    fn cilium_requires_options_and_vice_versa() {
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        assert!(validate(&spec).is_err());

        spec.cilium = Some(CiliumOptions::default());
        assert!(validate(&spec).is_ok());

        let mut spec = base_spec();
        spec.cilium = Some(CiliumOptions::default());
        assert!(validate(&spec).is_err());
    }

    #[test]
    fn mesh_requires_cluster_id_and_name() {
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            mesh: true,
            ..CiliumOptions::default()
        });
        assert!(validate(&spec).is_err(), "mesh without id/name must fail");

        spec.cilium = Some(CiliumOptions {
            mesh: true,
            cluster_id: 7,
            cluster_name: "mesh-a".to_string(),
            ..CiliumOptions::default()
        });
        assert!(validate(&spec).is_ok());

        spec.cilium = Some(CiliumOptions {
            mesh: true,
            cluster_id: 0,
            cluster_name: "mesh-a".to_string(),
            ..CiliumOptions::default()
        });
        assert!(validate(&spec).is_err(), "cluster_id 0 is out of range");
    }

    #[test]
    fn mesh_rejects_cluster_id_256() {
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            mesh: true,
            cluster_id: 256,
            cluster_name: "mesh-a".to_string(),
            ..CiliumOptions::default()
        });
        assert!(validate(&spec).is_err(), "cluster_id 256 is out of range");

        spec.cilium = Some(CiliumOptions {
            mesh: true,
            cluster_id: MAX_CLUSTER_ID,
            cluster_name: "mesh-a".to_string(),
            ..CiliumOptions::default()
        });
        assert!(validate(&spec).is_ok(), "cluster_id 255 is the max");
    }

    #[test]
    fn mesh_cluster_name_validated_as_dns_label() {
        let long = "a".repeat(MAX_NAME_LEN + 1);
        for bad in ["UPPER", &long, "-lead", "under_score", "trail-"] {
            let mut spec = base_spec();
            spec.cni = Cni::Cilium;
            spec.cilium = Some(CiliumOptions {
                mesh: true,
                cluster_id: 1,
                cluster_name: bad.to_string(),
                ..CiliumOptions::default()
            });
            assert!(validate(&spec).is_err(), "mesh name {bad:?} should fail");
        }
    }

    #[test]
    fn mesh_off_ignores_cluster_id_and_name() {
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            mesh: false,
            cluster_id: 0,
            cluster_name: String::new(),
            ..CiliumOptions::default()
        });
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn cilium_ingress_requires_cilium_cni() {
        let mut spec = base_spec();
        spec.ingress = Some(IngressController::Cilium);
        assert!(validate(&spec).is_err());

        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions::default());
        assert!(validate(&spec).is_err(), "needs cilium.ingress == true");

        spec.cilium = Some(CiliumOptions {
            ingress: true,
            ..CiliumOptions::default()
        });
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn worker_count_bounded() {
        let mut spec = base_spec();
        spec.worker_count = MAX_WORKERS + 1;
        assert!(validate(&spec).is_err());
        spec.worker_count = MAX_WORKERS;
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn duplicate_host_ports_rejected() {
        let mut spec = base_spec();
        spec.ingress = Some(IngressController::Nginx);
        spec.extra_port_mappings = vec![
            PortMapping {
                container_port: 80,
                host_port: 8080,
                listen_address: "127.0.0.1".to_string(),
                protocol: Protocol::Tcp,
            },
            PortMapping {
                container_port: 90,
                host_port: 8080,
                listen_address: "127.0.0.1".to_string(),
                protocol: Protocol::Tcp,
            },
        ];
        assert!(validate(&spec).is_err());
    }

    #[test]
    fn ingress_ports_iff_rule() {
        let mut spec = base_spec();
        spec.ingress = Some(IngressController::Nginx);
        assert!(validate(&spec).is_err(), "nginx needs 80/443 mapped");
        spec.extra_port_mappings = port(80, 443);
        assert!(validate(&spec).is_ok());

        let mut spec = base_spec();
        spec.extra_port_mappings = port(80, 443);
        assert!(validate(&spec).is_err(), "80/443 without ingress must fail");
    }

    #[test]
    fn bad_listen_address_rejected() {
        let mut spec = base_spec();
        spec.extra_port_mappings = vec![PortMapping {
            container_port: 8080,
            host_port: 8080,
            listen_address: "not-an-ip".to_string(),
            protocol: Protocol::Tcp,
        }];
        assert!(validate(&spec).is_err());
    }

    #[test]
    fn bad_k8s_version_rejected() {
        let mut spec = base_spec();
        spec.k8s_version = KubernetesVersion("v1.37".to_string());
        assert!(validate(&spec).is_err());
        spec.k8s_version = KubernetesVersion("v1.37.0".to_string());
        assert!(validate(&spec).is_err(), "leading v is not X.Y.Z");
        spec.k8s_version = KubernetesVersion("1.37.0".to_string());
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn invalid_service_cidr_rejected() {
        let mut spec = base_spec();
        spec.service_cidr = "banana".to_string();
        assert!(validate(&spec).is_err());
        spec.service_cidr = "10.96.0.0".to_string();
        assert!(validate(&spec).is_err(), "missing prefix length");
    }

    #[test]
    fn ipv6_cidr_rejected() {
        let mut spec = base_spec();
        spec.pod_cidr = "2001:db8::/32".to_string();
        assert!(validate(&spec).is_err(), "only IPv4 CIDRs are supported");
    }

    #[test]
    fn cilium_traefik_combo_is_allowed() {
        // No cross-CNI rule forbids cilium + traefik: the plan installs the
        // cilium CNI chain, then traefik via helm. Pin the current rule.
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions::default());
        spec.ingress = Some(IngressController::Traefik);
        spec.extra_port_mappings = port(80, 443);
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn cilium_ingress_controller_rejects_host_ports() {
        // cilium ingress uses its own service; mapped 80/443 host ports are
        // only valid for nginx/traefik.
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            ingress: true,
            ..CiliumOptions::default()
        });
        spec.ingress = Some(IngressController::Cilium);
        spec.extra_port_mappings = port(80, 443);
        assert!(validate(&spec).is_err());
        spec.extra_port_mappings = Vec::new();
        assert!(validate(&spec).is_ok());
    }

    // ---- kind config rendering (golden files) ----

    #[test]
    fn golden_kindnet_default() {
        let mut spec = base_spec();
        spec.name = "demo".to_string();
        let yaml = to_kind_yaml(&spec);
        let expected = "\
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
name: demo
networking:
  ipFamily: ipv4
  serviceSubnet: 10.96.0.0/16
  disableDefaultCNI: false
nodes:
- role: control-plane
  image: kindest/node:v1.37.0
";
        assert_eq!(yaml, expected);
    }

    #[test]
    fn golden_flannel_with_workers_and_ports() {
        let mut spec = base_spec();
        spec.name = "flannel-cluster".to_string();
        spec.cni = Cni::Flannel;
        spec.worker_count = 2;
        spec.ingress = Some(IngressController::Nginx);
        spec.extra_port_mappings = port(80, 443);
        spec.feature_gates.insert("SomeGate".to_string(), true);
        let yaml = to_kind_yaml(&spec);
        let expected = "\
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
name: flannel-cluster
featureGates:
  SomeGate: true
networking:
  ipFamily: ipv4
  podSubnet: 10.244.0.0/16
  serviceSubnet: 10.96.0.0/16
  disableDefaultCNI: true
nodes:
- role: control-plane
  image: kindest/node:v1.37.0
  extraPortMappings:
  - containerPort: 80
    hostPort: 80
    listenAddress: 127.0.0.1
    protocol: TCP
  - containerPort: 443
    hostPort: 443
    listenAddress: 127.0.0.1
    protocol: TCP
- role: worker
  image: kindest/node:v1.37.0
- role: worker
  image: kindest/node:v1.37.0
";
        assert_eq!(yaml, expected);
    }

    #[test]
    fn golden_calico_custom_cidr() {
        let mut spec = base_spec();
        spec.name = "calico".to_string();
        spec.cni = Cni::Calico;
        spec.pod_cidr = "192.168.0.0/16".to_string();
        let yaml = to_kind_yaml(&spec);
        assert!(yaml.contains("podSubnet: 192.168.0.0/16"), "{yaml}");
        assert!(yaml.contains("disableDefaultCNI: true"), "{yaml}");
        assert!(!yaml.contains("featureGates"), "{yaml}");
        assert!(!yaml.contains("extraPortMappings"), "{yaml}");
    }

    #[test]
    fn golden_cilium_mesh() {
        let mut spec = base_spec();
        spec.name = "cilium-a".to_string();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            api_gateway: true,
            hubble: true,
            ingress: true,
            mesh: true,
            cluster_id: 42,
            cluster_name: "mesh-a".to_string(),
        });
        let yaml = to_kind_yaml(&spec);
        let expected = "\
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
name: cilium-a
networking:
  ipFamily: ipv4
  podSubnet: 10.244.0.0/16
  serviceSubnet: 10.96.0.0/16
  disableDefaultCNI: true
nodes:
- role: control-plane
  image: kindest/node:v1.37.0
";
        assert_eq!(yaml, expected);
    }

    #[test]
    fn golden_kindnet_nginx_ingress() {
        let mut spec = base_spec();
        spec.name = "web".to_string();
        spec.ingress = Some(IngressController::Nginx);
        spec.extra_port_mappings = port(80, 443);
        let yaml = to_kind_yaml(&spec);
        let expected = "\
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
name: web
networking:
  ipFamily: ipv4
  serviceSubnet: 10.96.0.0/16
  disableDefaultCNI: false
nodes:
- role: control-plane
  image: kindest/node:v1.37.0
  extraPortMappings:
  - containerPort: 80
    hostPort: 80
    listenAddress: 127.0.0.1
    protocol: TCP
  - containerPort: 443
    hostPort: 443
    listenAddress: 127.0.0.1
    protocol: TCP
";
        assert_eq!(yaml, expected);
    }

    #[test]
    fn golden_calico_traefik_ports() {
        let mut spec = base_spec();
        spec.name = "cat".to_string();
        spec.cni = Cni::Calico;
        spec.ingress = Some(IngressController::Traefik);
        spec.extra_port_mappings = port(80, 443);
        let yaml = to_kind_yaml(&spec);
        assert!(yaml.contains("disableDefaultCNI: true"), "{yaml}");
        assert!(yaml.contains("podSubnet: 10.244.0.0/16"), "{yaml}");
        assert!(yaml.contains("extraPortMappings"), "{yaml}");
        assert_eq!(yaml.matches("role: worker").count(), 0, "{yaml}");
    }

    #[test]
    fn golden_flannel_traefik_no_port_conflict() {
        let mut spec = base_spec();
        spec.name = "flan-traf".to_string();
        spec.cni = Cni::Flannel;
        spec.pod_cidr = "10.99.0.0/16".to_string();
        spec.ingress = Some(IngressController::Traefik);
        spec.extra_port_mappings = port(80, 443);
        let yaml = to_kind_yaml(&spec);
        assert!(yaml.contains("podSubnet: 10.99.0.0/16"), "{yaml}");
        assert!(yaml.contains("disableDefaultCNI: true"), "{yaml}");
        assert!(yaml.contains("extraPortMappings"), "{yaml}");
    }

    #[test]
    fn golden_cilium_ingress_controller_no_host_ports() {
        let mut spec = base_spec();
        spec.name = "cil-ing".to_string();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            ingress: true,
            ..CiliumOptions::default()
        });
        spec.ingress = Some(IngressController::Cilium);
        let yaml = to_kind_yaml(&spec);
        let expected = "\
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
name: cil-ing
networking:
  ipFamily: ipv4
  podSubnet: 10.244.0.0/16
  serviceSubnet: 10.96.0.0/16
  disableDefaultCNI: true
nodes:
- role: control-plane
  image: kindest/node:v1.37.0
";
        assert_eq!(yaml, expected);
    }

    #[test]
    fn workers_zero_renders_single_control_plane() {
        let mut spec = base_spec();
        spec.name = "solo".to_string();
        spec.worker_count = 0;
        assert!(validate(&spec).is_ok());
        let yaml = to_kind_yaml(&spec);
        assert_eq!(yaml.matches("role: worker").count(), 0, "{yaml}");
        assert_eq!(yaml.matches("role: control-plane").count(), 1, "{yaml}");
    }

    #[test]
    fn kind_config_matrix_all_cni_ingress_combos() {
        // Every valid CNI × ingress combination renders a well-formed doc.
        let combos: Vec<(Cni, Option<IngressController>)> = vec![
            (Cni::KindnetDefault, None),
            (Cni::KindnetDefault, Some(IngressController::Nginx)),
            (Cni::KindnetDefault, Some(IngressController::Traefik)),
            (Cni::Flannel, None),
            (Cni::Flannel, Some(IngressController::Nginx)),
            (Cni::Flannel, Some(IngressController::Traefik)),
            (Cni::Calico, None),
            (Cni::Calico, Some(IngressController::Nginx)),
            (Cni::Calico, Some(IngressController::Traefik)),
            (Cni::Cilium, None),
            (Cni::Cilium, Some(IngressController::Nginx)),
            (Cni::Cilium, Some(IngressController::Traefik)),
            (Cni::Cilium, Some(IngressController::Cilium)),
        ];
        for (cni, ingress) in combos {
            let mut spec = base_spec();
            spec.name = "matrix".to_string();
            spec.cni = cni;
            if cni == Cni::Cilium {
                spec.cilium = Some(CiliumOptions {
                    ingress: ingress == Some(IngressController::Cilium),
                    ..CiliumOptions::default()
                });
            }
            spec.ingress = ingress;
            if matches!(
                ingress,
                Some(IngressController::Nginx) | Some(IngressController::Traefik)
            ) {
                spec.extra_port_mappings = port(80, 443);
            }
            validate(&spec)
                .unwrap_or_else(|err| panic!("combo {cni:?} × {ingress:?} must validate: {err}"));
            let yaml = to_kind_yaml(&spec);
            assert!(
                yaml.starts_with(
                    "kind: Cluster\napiVersion: kind.x-k8s.io/v1alpha4\nname: matrix\n"
                ),
                "combo {cni:?} × {ingress:?} header wrong:\n{yaml}"
            );
            let wants_disable = cni != Cni::KindnetDefault;
            assert_eq!(
                yaml.contains("disableDefaultCNI: true"),
                wants_disable,
                "combo {cni:?} × {ingress:?} disableDefaultCNI wrong:\n{yaml}"
            );
            let wants_ports = matches!(
                ingress,
                Some(IngressController::Nginx) | Some(IngressController::Traefik)
            );
            assert_eq!(
                yaml.contains("extraPortMappings"),
                wants_ports,
                "combo {cni:?} × {ingress:?} ports wrong:\n{yaml}"
            );
            assert_eq!(
                yaml.matches("role: worker").count(),
                0,
                "combo {cni:?} × {ingress:?} workers wrong:\n{yaml}"
            );
        }
    }
    #[test]
    fn custom_version_renders_node_image() {
        let mut spec = base_spec();
        spec.name = "old".to_string();
        spec.k8s_version = KubernetesVersion::new("1.30.4").unwrap();
        let yaml = to_kind_yaml(&spec);
        assert!(yaml.contains("image: kindest/node:v1.30.4"), "{yaml}");
    }

    #[test]
    fn spec_json_roundtrip() {
        let mut spec = base_spec();
        spec.name = "round".to_string();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            mesh: true,
            cluster_id: 3,
            cluster_name: "mesh-x".to_string(),
            ..CiliumOptions::default()
        });
        spec.extra_port_mappings = port(80, 443);
        spec.ingress = Some(IngressController::Nginx);
        let json = serde_json::to_string(&spec).unwrap();
        let back: ClusterSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn kind_config_display() {
        let spec = base_spec();
        let config = to_kind_config(&spec);
        assert_eq!(format!("{config}"), config.yaml);
        assert_eq!(config.as_str(), config.yaml.as_str());
    }
}
