use k8s_openapi::{
    api::core::v1, apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
    apimachinery::pkg::apis::meta::v1 as metav1,
};

use anyhow::Context as _;
use futures::StreamExt;
use kube::{
    api::{Meta, WatchEvent},
    Api,
};
use kube_derive::CustomResource;
use kube_runtime::watcher::Event;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SourceRef {
    /// ApiVersion of the source object
    api_version: String,
    /// Kind of the source object
    kind: String,
    /// Name of the source object
    name: String,
    /// Namespace of the source object
    namespace: String,
}

impl SourceRef {
    fn dynamic_resource(&self) -> kube::DynamicResource {
        let (group, version) = if self.api_version.contains('/') {
            let mut iter = self.api_version.split('/');
            (iter.next().unwrap(), iter.next().unwrap())
        } else {
            ("", self.api_version.as_str())
        };
        kube::DynamicResource::new(&self.kind)
            .group(group)
            .version(version)
    }
}

#[derive(
    CustomResource, Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[kube(group = "util.d-k8s.io", version = "v1", kind = "Propagation")]
#[serde(rename_all = "camelCase")]
struct PropagationSpec {
    /// Reference to object that should be copied
    source: SourceRef,
    /// Name of objects that should be created
    target_name: String,
}

fn strip_secret(s: &mut v1::Secret) {
    s.metadata.name = None;
    s.metadata.namespace = None;
    s.metadata.creation_timestamp = None;
    s.metadata.resource_version = None;
    s.metadata.managed_fields = None;
    s.metadata.uid = None;
}

fn cmp_secrets(mut a: v1::Secret, mut b: v1::Secret) -> bool {
    strip_secret(&mut a);
    strip_secret(&mut b);
    if a != b {
        let ar = serde_json::to_value(&a).unwrap();
        let br = serde_json::to_value(&b).unwrap();
        dbg!(json_patch::diff(&ar, &br));
    }
    return a == b;
}

async fn reconcile_single(
    k: &kube::Client,
    propagation: &Propagation,
    ns: &str,
) -> anyhow::Result<()> {
    tracing::info!(
        ns = ns,
        propagation_name = propagation.metadata.name.as_deref().unwrap_or_default(),
        "reconciling"
    );
    let src_api = propagation
        .spec
        .source
        .dynamic_resource()
        .within(&propagation.spec.source.namespace)
        .into_api::<v1::Secret>(k.clone());
    let target_api = propagation
        .spec
        .source
        .dynamic_resource()
        .within(ns)
        .into_api::<v1::Secret>(k.clone());
    let mut object = src_api
        .get(&propagation.spec.source.name)
        .await
        .context("failed to fetch")?;
    object.metadata.namespace = Some(ns.to_string());
    object.metadata.resource_version = None;
    object.metadata.name = Some(propagation.spec.target_name.clone());
    object.metadata.owner_references = Some(vec![metav1::OwnerReference {
        api_version: "util.d-k8s.io/v1".to_string(),
        block_owner_deletion: Some(false),
        controller: Some(true),
        kind: "propagation".to_string(),
        name: propagation
            .metadata
            .name
            .clone()
            .context("missing name on Propagation")?,
        uid: propagation
            .metadata
            .uid
            .clone()
            .context("missing uid on Propagation")?,
    }]);

    match target_api.get(&propagation.spec.target_name).await {
        Ok(existing) => {
            if cmp_secrets(existing.clone(), object.clone()) {
                return Ok(());
            }
            tracing::info!("Copy was changed, replacing");
            kube_utils::patch_with(
                k.clone(),
                |_| async { Ok(object.clone()) },
                Some(ns),
                &propagation.spec.target_name,
            )
            .await
            .context("failed to replace a secret")?;
        }
        Err(err) => {
            tracing::info!("Copy does not exist ({:#}), creating", err);

            target_api
                .create(&Default::default(), &object)
                .await
                .context("failed to create a copy")?;
        }
    }
    Ok(())
}

#[tracing::instrument(skip(k, propagation), fields(propagation = propagation.name().as_str()))]
async fn watch_for_copies(k: &kube::Client, propagation: &Propagation) -> anyhow::Result<()> {
    let api = propagation
        .spec
        .source
        .dynamic_resource()
        .into_api::<v1::Secret>(k.clone());
    let events = api
        .watch(&Default::default(), "0")
        .await
        .context("failed to start watch")?;
    tokio::pin!(events);
    while let Some(ev) = events.next().await {
        let ev = ev.context("watch error")?;
        match ev {
            WatchEvent::Added(copy) | WatchEvent::Deleted(copy) | WatchEvent::Modified(copy) => {
                let ns = copy
                    .metadata
                    .namespace
                    .as_ref()
                    .context("missing name in namespace")?;
                if let Err(e) = reconcile_single(k, propagation, ns).await {
                    tracing::warn!(
                        namespace = ns.as_str(),
                        "Failed to procecc a changed copy: {:#}",
                        e
                    );
                }
            }
            _ => (),
        }
    }

    Ok(())
}

#[tracing::instrument(skip(k, propagation), fields(propagation = propagation.name().as_str()))]
async fn watch_for_namespaces(k: &kube::Client, propagation: &Propagation) -> anyhow::Result<()> {
    let ns_api = Api::<v1::Namespace>::all(k.clone());
    let events = ns_api
        .watch(&Default::default(), "0")
        .await
        .context("failed to start watch")?;
    tokio::pin!(events);
    while let Some(ev) = events.next().await {
        let ev = ev.context("watch error")?;
        match ev {
            WatchEvent::Added(ns) => {
                let ns_name = ns
                    .metadata
                    .name
                    .as_ref()
                    .context("missing name in namespace")?;
                if let Err(e) = reconcile_single(k, propagation, ns_name).await {
                    tracing::warn!(
                        namespace = ns_name.as_str(),
                        "Failed to process a new namespace: {:#}",
                        e
                    );
                }
            }
            _ => (),
        }
    }
    Ok(())
}

/// Watches single propagation.
/// This function does not handle propagation updates.
async fn watch_propagation(
    k: &kube::Client,
    propagation: &Propagation,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let watch_copies = watch_for_copies(k, propagation);
    let watch_namespaces = watch_for_namespaces(k, propagation);
    tokio::select! {
        res = watch_copies => res,
        res = watch_namespaces => res,
        _ = cancel.cancelled() => anyhow::bail!("Cancelled")
    }
}

struct Worker {
    propagation_name: String,
    cancel: CancellationToken,
}

struct Supervisor {
    workers: Vec<Worker>,
    k: kube::Client,
}

impl Supervisor {
    fn find_worker(&self, propagation: &Propagation) -> Option<usize> {
        self.workers
            .iter()
            .position(|w| Some(&w.propagation_name) == propagation.metadata.name.as_ref())
    }

    fn untrack(&mut self, propagation: &Propagation) {
        if let Some(old) = self.find_worker(propagation) {
            self.workers[old].cancel.cancel();
            self.workers.remove(old);
        }
    }

    fn track(&mut self, propagation: &Propagation) {
        self.untrack(propagation);
        let cancel = CancellationToken::new();
        self.workers.push(Worker {
            cancel: cancel.clone(),
            propagation_name: propagation.metadata.name.clone().expect("name missing"),
        });
        let k = self.k.clone();
        let propagation = propagation.clone();
        tokio::task::spawn(async move {
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                if let Err(err) = watch_propagation(&k, &propagation, cancel.clone()).await {
                    tracing::warn!("Propagation reconciler failed: {:#}", err);
                }
            }
        });
    }
}

/// Ensures that `local-registry-credentials` secret is available in all namespaces
pub async fn copy_to_ns_controller(k: &kube::Client) {
    let propagations_api = Api::<Propagation>::all(k.clone());
    let propagations_watch = kube_runtime::watcher(propagations_api, Default::default());
    tokio::pin!(propagations_watch);
    let mut sv = Supervisor {
        workers: vec![],
        k: k.clone(),
    };

    loop {
        let item = propagations_watch.next().await;
        let item = item.expect("watch should be endless");
        match item {
            Ok(ev) => match ev {
                Event::Applied(prop) => {
                    sv.track(&prop);
                }
                Event::Deleted(prop) => {
                    sv.untrack(&prop);
                    tracing::error!("TODO: Proper deletion of Propagations is not supported, copies will be leaked")
                }
                Event::Restarted(props) => {
                    for prop in props {
                        sv.track(&prop);
                    }
                }
            },
            Err(err) => {
                tracing::warn!("propagations watch error: {:#}", err);
            }
        }
    }
}

pub fn crd() -> CustomResourceDefinition {
    Propagation::crd()
}
