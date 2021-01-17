use anyhow::Context as _;
use k8s_openapi::{
    api::core::v1::{HostPathVolumeSource, PersistentVolume, PersistentVolumeSpec},
    apimachinery::pkg::apis::meta::v1::LabelSelector,
};
use kube_utils::storage::{
    AccessMode, Configuration, DenyParameters, FromLabelSelector, ProvisionedVolume, VolumeMode,
};
use rand::Rng;
use std::{collections::BTreeMap, path::Path};
use tokio_util::sync::CancellationToken;

struct Provisioner;

const VOLUME_ID_ANNOTATION_NAME: &str = "storage.d-k8s.io/local-volume-id";
const VOLUME_DIR_ON_NODE: &str = "/var/d-k8s-volumes";
const VOLUME_DIR_IN_POD: &str = "/volumes";

impl kube_utils::storage::Provision for Provisioner {
    const NAME: &'static str = "d-k8s.io/local-volume";

    const VOLUME_MODES: &'static [VolumeMode] = &[VolumeMode::Filesystem];

    const ACCESS_MODES: &'static [AccessMode] = &[
        AccessMode::ReadOnlyMany,
        AccessMode::ReadWriteOnce,
        AccessMode::ReadWriteMany,
    ];

    type Labels = Selector;

    type Parameters = DenyParameters;

    fn provision(
        &self,
        labels: Selector,
        _params: Self::Parameters,
        _volume_mode: VolumeMode,
        _access_modes: &[AccessMode],
    ) -> futures::future::BoxFuture<'static, anyhow::Result<ProvisionedVolume>> {
        Box::pin(async move {
            let volume_name = labels.volume_name.unwrap_or_else(generate_volume_name);
            let src = provision_volume(&volume_name).await?;
            let mut annotations = BTreeMap::new();
            annotations.insert(VOLUME_ID_ANNOTATION_NAME.to_string(), volume_name.clone());
            Ok(ProvisionedVolume {
                pv_spec: PersistentVolumeSpec {
                    host_path: Some(src),
                    ..Default::default()
                },
                labels: BTreeMap::new(),
                annotations,
            })
        })
    }

    fn cleanup(
        &self,
        pv: PersistentVolume,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            let volume_id = pv
                .metadata
                .annotations
                .as_ref()
                .and_then(|anns| anns.get(VOLUME_ID_ANNOTATION_NAME))
                .context("missing annotation")?;

            let volume_path = Path::new(VOLUME_DIR_IN_POD).join(volume_id);
            tracing::info!("Deleting {}", volume_path.display());
            tokio::fs::remove_dir_all(volume_path).await?;

            Ok(())
        })
    }
}

fn generate_volume_name() -> String {
    let mut name = "anonymous-".to_string();
    let mut rng = rand::thread_rng();
    for _ in 0..10 {
        name.push(rng.sample(rand::distributions::Alphanumeric) as char);
    }
    name
}

pub async fn pv_controller(k: &kube::Client) {
    let mut cfg: Configuration = Default::default();
    cfg.pv_name_prefix = "d-k8s-local-volume".to_string();
    kube_utils::storage::run(k, Provisioner, cfg, CancellationToken::new()).await;
}

async fn provision_volume(volume_name: &str) -> anyhow::Result<HostPathVolumeSource> {
    let mounted_volumes = Path::new(VOLUME_DIR_IN_POD);
    tokio::fs::create_dir_all(mounted_volumes.join(volume_name)).await?;
    Ok(HostPathVolumeSource {
        path: format!("{}/{}", VOLUME_DIR_ON_NODE, volume_name),
        type_: Some("Directory".to_string()),
    })
}

#[derive(Default, Debug)]
struct Selector {
    volume_name: Option<String>,
}

impl FromLabelSelector for Selector {
    fn from_selector(selector: LabelSelector) -> anyhow::Result<Self> {
        if selector.match_expressions.is_some() {
            anyhow::bail!("matchExpressions not supported");
        }
        let mut lbls = match selector.match_labels.as_ref() {
            Some(l) => l.clone(),
            None => return Ok(Default::default()),
        };
        let mut selector = Selector { volume_name: None };
        if let Some(vn) = lbls.remove("volume-id") {
            selector.volume_name = Some(vn);
        }
        if !lbls.is_empty() {
            anyhow::bail!("unknown labels: {:?}", lbls);
        }
        Ok(selector)
    }
}
