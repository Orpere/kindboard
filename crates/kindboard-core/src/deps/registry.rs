//! Tool registry: the dependency-install matrix as data
//! (`docs/dependency-install-matrix.md` is the source of truth; values here
//! were verified 2026-09-12 against Fedora 44, Homebrew, and the upstream
//! release pages).
//!
//! Adding a tool is one struct literal plus a test — no scattered match
//! arms.

use super::{BinaryDownload, DetectSpec, InstallRecipe, Tool, ToolId, VersionParse};

/// The full registry, one entry per tool (order = recommended install order).
pub static TOOLS: std::sync::LazyLock<Vec<Tool>> = std::sync::LazyLock::new(|| {
    vec![
        docker(),
        kind(),
        kubectl(),
        helm(),
        cilium(),
        k9s(),
        kubectx(),
        kustomize(),
    ]
});

/// The registry as a slice.
pub fn registry() -> &'static [Tool] {
    &TOOLS
}
/// Look up a tool by id.
pub fn tool(id: ToolId) -> Option<&'static Tool> {
    registry().iter().find(|tool| tool.id == id)
}

fn docker() -> Tool {
    Tool {
        id: ToolId::Docker,
        display: "docker",
        detect: DetectSpec {
            command: "docker",
            version_args: &["version", "--format", "{{.Client.Version}}"],
            parse: VersionParse::ShortLine,
        },
        install: InstallRecipe {
            brew: Some("docker"),
            brew_cask: Some("docker-desktop"),
            dnf: Some("moby-engine"),
            apt: Some("docker.io"),
            pacman: Some("docker"),
            binary: None,
            pkg_manager_only: true,
            post_install: Some(
                "enable the daemon (sudo systemctl enable --now docker) and \
                 add your user to the docker group (log out/in afterwards)",
            ),
        },
    }
}

fn kind() -> Tool {
    Tool {
        id: ToolId::Kind,
        display: "kind",
        detect: DetectSpec {
            command: "kind",
            version_args: &["version"],
            parse: VersionParse::Regex("v(\\d+\\.\\d+\\.\\d+)"),
        },
        install: InstallRecipe {
            brew: Some("kind"),
            brew_cask: None,
            dnf: Some("kind"),
            apt: None,
            pacman: None,
            binary: Some(BinaryDownload {
                url_template: "https://kind.sigs.k8s.io/dl/v0.33.0/kind-{os}-{arch}",
                members: &[],
                // Official digests: https://kind.sigs.k8s.io/dl/v0.33.0/kind-<os>-<arch>.sha256sum
                sha256: &[
                    (
                        "linux-amd64",
                        "aee6151561422756b764a4ae28e7f44cda5af5a9eead3cc9985112b1de8d8e0d",
                    ),
                    (
                        "linux-arm64",
                        "20022bee6cfcd5086cb7234d218e3454e6090022f2a8f55d1fa7fcf42c3867a2",
                    ),
                    (
                        "darwin-amd64",
                        "5a99f26f57246dc9319dd294803313197a0f34d33c525b3ea8b655db5916ece0",
                    ),
                    (
                        "darwin-arm64",
                        "0c8c7dbe5e23594a198b786c4bc13dacc101fa6196b0cb0b23a1ca44e61f4b4f",
                    ),
                ],
            }),
            pkg_manager_only: false,
            post_install: None,
        },
    }
}

fn kubectl() -> Tool {
    Tool {
        id: ToolId::Kubectl,
        display: "kubectl",
        detect: DetectSpec {
            command: "kubectl",
            version_args: &["version", "--client", "--output=json"],
            parse: VersionParse::JsonField("clientVersion.gitVersion"),
        },
        install: InstallRecipe {
            brew: Some("kubernetes-cli"),
            brew_cask: None,
            dnf: None,
            apt: Some("kubectl"),
            pacman: Some("kubectl"),
            binary: Some(BinaryDownload {
                url_template: "https://dl.k8s.io/release/v1.37.0/bin/{os}/{arch}/kubectl",
                members: &[],
                // Official digests: https://dl.k8s.io/release/v1.37.0/bin/<os>/<arch>/kubectl.sha256
                sha256: &[
                    (
                        "linux-amd64",
                        "6129359f4e1f3848a5572ccb0b26cf28b8ca08cef38c95a765b2f64a2c961a2f",
                    ),
                    (
                        "linux-arm64",
                        "922df28df248cc00a9e025f947704f1d1482de64ece54cfe57e61f19eaf1eef3",
                    ),
                    (
                        "darwin-amd64",
                        "d5276c0f4fde77fc446070290f345944a7f1fda153df6b960e5fde93b7a9bccd",
                    ),
                    (
                        "darwin-arm64",
                        "583beedaebe422e71d3f1a96acef8b1fef86ea2f09a45ad01aa6c9ce287c1380",
                    ),
                ],
            }),
            pkg_manager_only: false,
            post_install: None,
        },
    }
}

fn helm() -> Tool {
    Tool {
        id: ToolId::Helm,
        display: "helm",
        detect: DetectSpec {
            command: "helm",
            version_args: &["version", "--short"],
            parse: VersionParse::Regex("v(\\d+\\.\\d+\\.\\d+)"),
        },
        install: InstallRecipe {
            brew: Some("helm"),
            brew_cask: None,
            dnf: Some("helm"),
            apt: Some("helm"),
            pacman: Some("helm"),
            binary: Some(BinaryDownload {
                url_template: "https://get.helm.sh/helm-v4.2.2-{os}-{arch}.tar.gz",
                members: &["{os}-{arch}/helm"],
                // Official digests: https://get.helm.sh/helm-v4.2.2-<os>-<arch>.tar.gz.sha256sum
                sha256: &[
                    (
                        "linux-amd64",
                        "9adafecab4d406853bba163a70e9f104f47dbbf65ce24b7653bae7e36150bcb6",
                    ),
                    (
                        "linux-arm64",
                        "78803142087a0069fa4b50d3f32a84d3ef25c14d1ee8a40fbccf86a6216d2f36",
                    ),
                    (
                        "darwin-amd64",
                        "10c1e36ee8c5f2e2ee25a16599cb03ab74c0953cd889cacb980a49ba4b6574ba",
                    ),
                    (
                        "darwin-arm64",
                        "5410a0dae3d5d91f45653b161260d9301aabc4ae80ae50a6605d66884b6df8ea",
                    ),
                ],
            }),
            pkg_manager_only: false,
            post_install: None,
        },
    }
}

fn cilium() -> Tool {
    Tool {
        id: ToolId::Cilium,
        display: "cilium",
        detect: DetectSpec {
            command: "cilium",
            version_args: &["version", "--client"],
            parse: VersionParse::Regex("v(\\d+\\.\\d+\\.\\d+)"),
        },
        install: InstallRecipe {
            brew: Some("cilium-cli"),
            brew_cask: None,
            dnf: None,
            apt: None,
            pacman: None,
            binary: Some(BinaryDownload {
                url_template: "https://github.com/cilium/cilium-cli/releases/download/v0.20.0/cilium-{os}-{arch}.tar.gz",
                members: &["cilium"],
                // Official digests: GitHub release .sha256sum assets (cilium-cli v0.20.0).
                sha256: &[
                    (
                        "linux-amd64",
                        "94adc5c3be44d8240df91cec520536686f8a1eb29e5bc70c4c1c80137f00d764",
                    ),
                    (
                        "linux-arm64",
                        "9a9281dc79bd10a61fa35078b8ab63eaf491ab2f47edc6b9a3541b2fe4ede3ff",
                    ),
                    (
                        "darwin-amd64",
                        "cc338e326235e76e3924f032d8a6eb41bf751d94bb04fce70d5f80014fc179d9",
                    ),
                    (
                        "darwin-arm64",
                        "2850449e321c21e9556c312d0b6885838523d294b7e0b58955697507463a57cc",
                    ),
                ],
            }),
            pkg_manager_only: false,
            post_install: None,
        },
    }
}

fn k9s() -> Tool {
    Tool {
        id: ToolId::K9s,
        display: "k9s",
        detect: DetectSpec {
            command: "k9s",
            version_args: &["version", "--short"],
            parse: VersionParse::Regex("Version\\s+(\\S+)"),
        },
        install: InstallRecipe {
            brew: Some("k9s"),
            brew_cask: None,
            dnf: Some("k9s"),
            apt: None,
            pacman: Some("k9s"),
            binary: Some(BinaryDownload {
                url_template: "https://github.com/derailed/k9s/releases/download/v0.51.0/k9s_{OS}_{arch}.tar.gz",
                members: &["k9s"],
                // Official digests: GitHub release asset checksums.sha256 (k9s v0.51.0).
                sha256: &[
                    (
                        "linux-amd64",
                        "c3752ad51a5a4015a113819c4eeb6e55a4d0e4b8e652494797532f6fc8161dd7",
                    ),
                    (
                        "linux-arm64",
                        "3ee05c82e5f9198928a4e86133608ba6a2c10a2244d6a7789e820f78319d640c",
                    ),
                    (
                        "darwin-amd64",
                        "7e8802c57c45a0cd389e8fc38472243fe4cf48d8ad5957e189ce370e19e5eda0",
                    ),
                    (
                        "darwin-arm64",
                        "9b8c0e8f461e5d33aeee43a67f5ef4aff646a008a786887b8266cbb153c610cc",
                    ),
                ],
            }),
            pkg_manager_only: false,
            post_install: None,
        },
    }
}

fn kubectx() -> Tool {
    Tool {
        id: ToolId::Kubectx,
        display: "kubectx",
        detect: DetectSpec {
            command: "kubectx",
            version_args: &["--version"],
            parse: VersionParse::ShortLine,
        },
        install: InstallRecipe {
            brew: Some("kubectx"),
            brew_cask: None,
            dnf: None,
            apt: None,
            pacman: None,
            binary: Some(BinaryDownload {
                url_template: "https://github.com/ahmetb/kubectx/releases/download/v0.11.0/kubectx_v0.11.0_{os}_{x64}.tar.gz",
                // The kubectx archive contains only the kubectx binary; kubens
                // ships in a separate upstream archive and is not installed.
                members: &["kubectx"],
                // Official digests: GitHub release asset checksums.txt (kubectx v0.11.0).
                sha256: &[
                    (
                        "linux-amd64",
                        "08e031c54fbffb3f100e904e4eae94bba2730fedf4869921fda79e4d7a8f5d4c",
                    ),
                    (
                        "linux-arm64",
                        "1dac2072216689e773325cb7587b858ea7415706ebcf04e23bf80e7e70555340",
                    ),
                    (
                        "darwin-amd64",
                        "80097893b478c55be51ffe826c172f0fd5095d23cca02adbcbcabc412700a689",
                    ),
                    (
                        "darwin-arm64",
                        "b8c9b5150ca6d902474a115cf7e535831081b9ae10cbe561ea36c82bb3823d02",
                    ),
                ],
            }),
            pkg_manager_only: false,
            post_install: None,
        },
    }
}

fn kustomize() -> Tool {
    Tool {
        id: ToolId::Kustomize,
        display: "kustomize",
        detect: DetectSpec {
            command: "kustomize",
            version_args: &["version"],
            parse: VersionParse::ShortLine,
        },
        install: InstallRecipe {
            brew: Some("kustomize"),
            brew_cask: None,
            dnf: Some("kustomize"),
            apt: None,
            pacman: Some("kustomize"),
            binary: Some(BinaryDownload {
                url_template: "https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize/v5.8.1/kustomize_v5.8.1_{os}_{arch}.tar.gz",
                members: &["kustomize"],
                // Official digests: GitHub release asset checksums.txt (kustomize v5.8.1).
                sha256: &[
                    (
                        "linux-amd64",
                        "029a7f0f4e1932c52a0476cf02a0fd855c0bb85694b82c338fc648dcb53a819d",
                    ),
                    (
                        "linux-arm64",
                        "0953ea3e476f66d6ddfcd911d750f5167b9365aa9491b2326398e289fef2c142",
                    ),
                    (
                        "darwin-amd64",
                        "ee7cf0c1e3592aa7bb66ba82b359933a95e7f2e0b36e5f53ed0a4535b017f2f8",
                    ),
                    (
                        "darwin-arm64",
                        "8886f8a78474e608cc81234f729fda188a9767da23e28925802f00ece2bab288",
                    ),
                ],
            }),
            pkg_manager_only: false,
            post_install: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_all_eight_tools_in_order() {
        let ids: Vec<ToolId> = registry().iter().map(|tool| tool.id).collect();
        assert_eq!(
            ids,
            vec![
                ToolId::Docker,
                ToolId::Kind,
                ToolId::Kubectl,
                ToolId::Helm,
                ToolId::Cilium,
                ToolId::K9s,
                ToolId::Kubectx,
                ToolId::Kustomize
            ]
        );
    }

    #[test]
    fn every_tool_has_detect_command_and_install_path() {
        for tool in registry() {
            assert!(!tool.detect.command.is_empty(), "{tool:?}");
            assert!(!tool.detect.version_args.is_empty(), "{tool:?}");
            let has_any_install = tool.install.brew.is_some()
                || tool.install.dnf.is_some()
                || tool.install.apt.is_some()
                || tool.install.pacman.is_some()
                || tool.install.binary.is_some();
            assert!(has_any_install, "{tool:?} must have an install path");
        }
    }

    #[test]
    fn docker_is_pkg_manager_only() {
        let docker = tool(ToolId::Docker).expect("registry entry");
        assert!(docker.install.binary.is_none());
        assert!(docker.install.pkg_manager_only);
        assert!(docker.install.post_install.is_some());
    }

    #[test]
    fn binary_tools_have_urls() {
        for id in [
            ToolId::Kind,
            ToolId::Kubectl,
            ToolId::Helm,
            ToolId::Cilium,
            ToolId::K9s,
            ToolId::Kubectx,
            ToolId::Kustomize,
        ] {
            let binary = tool(id)
                .expect("registry entry")
                .install
                .binary
                .as_ref()
                .expect("binary fallback");
            assert!(binary.url_template.starts_with("https://"), "{id:?}");
        }
    }
}
