use anyhow::Context as _;
use futures::StreamExt;
use k8s_openapi::api::core::v1;
use kube_utils::webhook::Review;

pub struct PodReviewer {
    resolver: ImageRegistryResolver,
}

impl PodReviewer {
    pub fn new(resolver: ImageRegistryResolver) -> Self {
        PodReviewer { resolver }
    }
}

enum Phase<'a> {
    Check,
    Patch { repo_addr: &'a str },
}

/// # Return value
/// Phase::Patch: unspecified
/// Phase::Check: whether pod should be patched
fn patch_pod(pod: &mut v1::Pod, phase: &Phase<'_>) -> Option<bool> {
    let containers = &mut pod.spec.as_mut()?.containers;
    for container in containers {
        let cur_image = container.image.as_ref()?;
        if let Some(suf) = cur_image.strip_prefix("cr.local/") {
            match phase {
                Phase::Check => return Some(true),
                Phase::Patch { repo_addr } => {
                    container.image = Some(format!("{}/{}", repo_addr, suf));
                }
            }
        }
    }
    Some(false)
}

impl Review for PodReviewer {
    type Resource = v1::Pod;

    fn review(&self, mut pod: Self::Resource) -> anyhow::Result<Self::Resource> {
        let should_patch = patch_pod(&mut pod, &Phase::Check) == Some(true);
        if should_patch {
            let repo_addr = self
                .resolver
                .resolve_svc("registry", "registry")
                .context("failed to resolve registry service")?;
            patch_pod(
                &mut pod,
                &Phase::Patch {
                    repo_addr: &repo_addr,
                },
            );
        }
        Ok(pod)
    }
}

pub struct ImageRegistryResolver {
    services: kube_runtime::reflector::Store<v1::Service>,
    nodes: kube_runtime::reflector::Store<v1::Node>,
}

fn make_store<K: kube::api::Meta + Clone + Send + Sync + serde::de::DeserializeOwned>(
    api: kube::Api<K>,
) -> kube_runtime::reflector::Store<K> {
    let watcher = kube_runtime::watcher(api, Default::default());
    let writer = kube_runtime::reflector::store::Writer::default();
    let store = writer.as_reader();
    let reflector = kube_runtime::reflector::reflector(writer, watcher);
    tokio::task::spawn(async move {
        tokio::pin!(reflector);
        while let Some(item) = reflector.next().await {
            if let Err(e) = item {
                log::warn!("watcher: error: {}", e);
            }
        }
    });
    store
}

impl ImageRegistryResolver {
    pub async fn new() -> anyhow::Result<ImageRegistryResolver> {
        let k = kube::Client::try_default().await?;
        Ok(ImageRegistryResolver {
            services: make_store(kube::Api::namespaced(k.clone(), "registry")),
            nodes: make_store(kube::Api::all(k)),
        })
    }

    fn resolve_svc(&self, ns: &str, name: &str) -> anyhow::Result<String> {
        let nodes = self.nodes.state();
        anyhow::ensure!(nodes.len() == 1);
        let ip = {
            let n = &nodes[0];
            let meta = &n.metadata;
            let anns = meta
                .annotations
                .as_ref()
                .context("no annotations on Node object")?;
            anns.get("d-k8s.io/public-ip")
                .with_context(|| {
                    format!(
                        "annotation 'd-k8s.io/public-ip' missing on node {}",
                        meta.name.as_deref().unwrap_or_default()
                    )
                })?
                .clone()
        };
        let obj_ref = kube_runtime::reflector::ObjectRef::new(name).within(ns);
        let svc = self.services.get(&obj_ref).context("unknown service")?;
        let ports = svc
            .spec
            .context("service spec missing")?
            .ports
            .context("service ports missing")?;
        anyhow::ensure!(ports.len() == 1);
        let port = &ports[0];
        let port = port.node_port.context("NodePort missing")?;
        Ok(format!("{}:{}", ip, port))
    }
}
