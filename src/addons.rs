use anyhow::Context as _;
use k8s_openapi::{
    api::{apps::v1 as appsv1, core::v1},
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{api::PatchParams, Api};
use rand::Rng;
use std::{collections::BTreeMap, future::Future, pin::Pin};

trait Addon {
    fn name(&self) -> &str;

    fn fix(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>>>>;
}

struct Dashboard;
impl Addon for Dashboard {
    fn name(&self) -> &str {
        "dashboard"
    }
    fn fix(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>>>> {
        Box::pin(async move { Ok(()) })
    }
}

struct Registry;
impl Addon for Registry {
    fn name(&self) -> &str {
        "registry"
    }
    fn fix(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>>>> {
        Box::pin(async move { install_docker_registry().await })
    }
}

struct Admission;
impl Addon for Admission {
    fn name(&self) -> &str {
        "admission"
    }
    fn fix(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>>>> {
        Box::pin(async move {
            println!("Building tool");
            let tool_path = crate::ROOT.join("tool");
            xshell::cmd!("docker build -t d-k8s-tool {tool_path}").run()?;
            println!("Pushing tool");

            crate::push_img::push("d-k8s-tool", "tool").await?;
            println!("Issuing certificates");
            issue_certs(
                &["admission-controller-svc.admission.svc"],
                "admission-webhook",
                ("admission", "admission-controller-pki"),
            )
            .await?;
            println!("Patching deployment");
            let k = crate::kube().await?;
            let image_registry =
                crate::service_util::resolve_service("registry", "registry").await?;
            let image = format!("{}/tool", image_registry);
            let deployments_api = kube::Api::<appsv1::Deployment>::namespaced(k, "admission");
            let mut deployment = deployments_api.get("admission-controller").await?;
            {
                let spec = deployment.spec.as_mut().context("no .spec")?;
                let pod_spec = spec
                    .template
                    .spec
                    .as_mut()
                    .context("no .spec.template.spec")?;
                anyhow::ensure!(pod_spec.containers.len() == 1);
                let container = &mut pod_spec.containers[0];
                container.image = Some(image);
            }
            deployments_api
                .replace("admission-controller", &Default::default(), &deployment)
                .await?;
            Ok(())
        })
    }
}

pub async fn install(only_apply: bool, filter: Option<&[String]>) -> anyhow::Result<()> {
    crate::configure_kubectl();
    let mut all_addons: Vec<Box<dyn Addon>> =
        vec![Box::new(Dashboard), Box::new(Registry), Box::new(Admission)];

    println!("Applying addons");
    if let Some(filter) = filter {
        all_addons = std::mem::take(&mut all_addons)
            .into_iter()
            .filter(|addon| filter.contains(&addon.name().to_string()))
            .collect();
    }
    if all_addons.is_empty() {
        anyhow::bail!("No addons matched the filter");
    }

    let addons_base_path = crate::ROOT.join("addons");
    let mut cmd = xshell::cmd!("kubectl apply --recursive");
    for addon in &all_addons {
        cmd = cmd.arg("-f").arg(addons_base_path.join(addon.name()));
    }

    cmd.run()?;
    if only_apply {
        return Ok(());
    }
    println!("Setting up addons");
    for addon in &all_addons {
        println!("------ Setting up addon {} ------", addon.name());
        addon.fix().await?;
    }
    Ok(())
}

/// Gets or creates secret with credentials
async fn get_registry_credentials(k: &kube::Client) -> anyhow::Result<BTreeMap<String, String>> {
    const SECRET_NAME: &str = "registry-credentials";
    let registry_url = crate::service_util::resolve_service("registry", "registry").await?;
    let username = "admin";
    let mut rng = rand::thread_rng();
    let password = std::iter::repeat(())
        .map(|_| rng.sample(rand::distributions::Alphanumeric))
        .map(char::from)
        .take(30)
        .collect::<String>();
    let credentials = xshell::cmd!("htpasswd -Bbn {username} {password}").read()?;
    let mut new_creds = BTreeMap::new();
    new_creds.insert("credentials".to_string(), credentials.to_string());
    new_creds.insert("USERNAME".to_string(), username.to_string());
    new_creds.insert("PASSWORD".to_string(), password.clone());
    new_creds.insert("ADDRESS".to_string(), registry_url.clone());

    let new_secret = v1::Secret {
        string_data: Some(new_creds),
        metadata: ObjectMeta {
            name: Some(SECRET_NAME.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let secrets_api = Api::<v1::Secret>::namespaced(k.clone(), "registry");
    let create_secret_res = secrets_api.create(&Default::default(), &new_secret).await;

    let effective_secret = match create_secret_res {
        Ok(created) => created,
        Err(err) => {
            let is_caused_by_conflict = match &err {
                kube::Error::Api(resp) => resp.code == 409,
                _ => false,
            };
            if is_caused_by_conflict {
                // secret already exists, lets just use it
                let old_secret = secrets_api.get(SECRET_NAME).await?;
                old_secret
            } else {
                return Err(err.into());
            }
        }
    };

    let mut res = BTreeMap::new();
    for (k, v) in effective_secret.data.expect("no data on returned secret") {
        let v = String::from_utf8(v.0)?;
        res.insert(k, v);
    }
    Ok(res)
}

async fn install_docker_registry() -> anyhow::Result<()> {
    let k = crate::kube().await?;
    let registry_url = crate::service_util::resolve_service("registry", "registry").await?;
    let creds = get_registry_credentials(&k).await?;
    let username = creds["USERNAME"].clone();
    let password = creds["PASSWORD"].clone();
    println!("Credentials: {}:{}", username, password);
    println!("Pushing credentials to k8s");
    println!("Creating docker registry certificates");
    setup_certs().await?;
    println!("Registry url is {}", registry_url);
    println!("Waiting for registry to become ready");
    crate::watch::watch::<k8s_openapi::api::apps::v1::Deployment>(&k, "registry", "registry", 90)
        .await?;
    println!("Logging in to registry");
    xshell::cmd!("docker login {registry_url} -u {username} -p {password}").run()?;
    Ok(())
}

async fn issue_certs(sans: &[&str], name: &str, push_to: (&str, &str)) -> anyhow::Result<()> {
    let csr = serde_json::json! ({
        "hosts": sans,
        "key": {
            "algo": "rsa",
            "size": 2048,
        },
        "names": [
            {
                "C": "RU",
                "L": "Moscow",
                "O": "mb",
                "OU": name,
                "ST": "MOS",
            }
        ]
    });
    let csr = serde_json::to_string(&csr)?;
    let csr_path = format!("/tmp/d-k8s-csr-{}.json", name);
    tokio::fs::write(&csr_path, &csr).await?;

    let _p = xshell::pushd(&*crate::ROOT)?;
    let ca_settings: crate::config_defs::CaSettings =
        serde_json::from_slice(&tokio::fs::read("./etc/ca.json").await?)?;
    let ca_certificate = &ca_settings.certificate;
    let ca_private_key = &ca_settings.private_key;
    let certs =
        xshell::cmd!("cfssl gencert -ca {ca_certificate} -ca-key {ca_private_key} {csr_path}")
            .read()?;
    let certs: serde_json::Value = serde_json::from_str(&certs)?;
    let certs: BTreeMap<_, _> = vec![
        (
            "crt".to_string(),
            certs["cert"].as_str().unwrap().to_string(),
        ),
        (
            "key".to_string(),
            certs["key"].as_str().unwrap().to_string(),
        ),
    ]
    .into_iter()
    .collect();
    let certs_secret = v1::Secret {
        string_data: Some(certs),
        metadata: ObjectMeta {
            name: Some(push_to.1.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let certs_secret = serde_json::to_vec(&certs_secret)?;
    println!("Pushing certificates to k8s");
    let k = crate::kube().await?;
    let secrets_api = Api::<v1::Secret>::namespaced(k, push_to.0);
    secrets_api
        .patch(push_to.1, &PatchParams::apply("d-k8s"), certs_secret)
        .await?;
    Ok(())
}

async fn setup_certs() -> anyhow::Result<()> {
    let vm_ip = crate::vm::vm_ip().await?;
    issue_certs(&[&vm_ip], "docker-registry", ("registry", "registry-certs")).await?;
    Ok(())
}
