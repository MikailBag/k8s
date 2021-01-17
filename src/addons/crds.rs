use crate::addons::Addon;
use anyhow::Context as _;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use std::future::Future;
use std::pin::Pin;

pub struct Crds;

impl Addon for Crds {
    fn name(&self) -> &str {
        "crds"
    }

    fn pre_apply(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>>>> {
        Box::pin(async {
            println!("Building tool");
            let tool_path = crate::ROOT.join("tool");
            xshell::cmd!("docker build -t d-k8s-tool {tool_path}").run()?;
            println!("Obtaining custom resource definition");
            let crd = xshell::cmd!("docker run -i --rm --env PRINT=propagation-custom-resource-definition  d-k8s-tool").read()?;
            let crd: CustomResourceDefinition =
                serde_json::from_str(crd.trim()).context("failed to parse")?;
            println!("Pushing CRD to server");
            let k = crate::kube().await?;
            let crd_api = kube::Api::<CustomResourceDefinition>::all(k);
            if crd_api.create(&Default::default(), &crd).await.is_err() {
                println!("Replacing CRD");
                let old_crd = crd_api.get(crd.metadata.name.as_ref().unwrap()).await?;
                let mut crd = crd;
                crd.metadata.resource_version = old_crd.metadata.resource_version;
                crd_api
                    .replace(
                        crd.metadata.name.as_ref().unwrap(),
                        &Default::default(),
                        &crd,
                    )
                    .await?;
            }
            Ok(())
        })
    }

    fn fix(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>>>> {
        Box::pin(async { Ok(()) })
    }
}
