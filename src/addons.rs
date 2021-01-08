use k8s_openapi::{api::core::v1, apimachinery::pkg::apis::meta::v1::ObjectMeta};
use kube::{api::PatchParams, Api};
use rand::Rng;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
pub async fn install(only_apply: bool) -> anyhow::Result<()> {
    crate::configure_kubectl();
    println!("Applying addons");

    let addons_path = crate::ROOT.join("addons");
    xshell::cmd!("kubectl apply --recursive -f {addons_path}").run()?;
    if only_apply {
        return Ok(());
    }
    println!("Setting up addons");
    install_docker_registry().await?;
    Ok(())
}

async fn install_docker_registry() -> anyhow::Result<()> {
    println!("------ Setting up docker registry ------");
    let username = "admin";
    let mut rng = rand::thread_rng();
    let password = std::iter::repeat(())
        .map(|_| rng.sample(rand::distributions::Alphanumeric))
        .map(char::from)
        .take(30)
        .collect::<String>();
    println!("Credentials: {}:{}", username, password);
    let credentials = xshell::cmd!("htpasswd -Bbn {username} {password}").read()?;
    println!("Pushing credentials to k8s");
    let mut secret_data = BTreeMap::new();
    secret_data.insert("credentials".to_string(), credentials.to_string());
    let secret_with_creds = v1::Secret {
        string_data: Some(secret_data),
        metadata: ObjectMeta {
            name: Some("registry-credentials".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let secret_with_creds = serde_json::to_vec(&secret_with_creds)?;
    let k = crate::kube().await?;
    let secrets_api = Api::<v1::Secret>::namespaced(k.clone(), "registry");
    secrets_api
        .patch(
            "registry-credentials",
            &PatchParams::apply("d-k8s"),
            secret_with_creds,
        )
        .await?;
    println!("Creating docker registry certificates");
    let tempdir = tempfile::TempDir::new()?;
    setup_certs(tempdir.path()).await?;

    println!("Obtaining url");
    let registry_url = crate::service_util::resolve_service("registry", "registry").await?;
    println!("Registry url is {}", registry_url);
    crate::deployment_util::restart_deployment(&k, "registry", "registry").await?;
    println!("Waiting for registry to become ready");
    crate::watch::watch::<k8s_openapi::api::apps::v1::Deployment>(&k, "registry", "registry", 30).await?;
    println!("Logging in to registry");
    xshell::cmd!("docker login {registry_url} -u {username} -p {password}").run()?;
    Ok(())
}

async fn setup_certs(path: &Path) -> anyhow::Result<()> {
    let vm_state: crate::vm::VmState =
        serde_json::from_slice(&tokio::fs::read(crate::ROOT.join("state/vm.json")).await?)?;
    let vm_ip = &vm_state.ip;
    let docker_cert_csr = serde_json::json! ({
        "hosts": [
            vm_ip,
        ],
        "key": {
            "algo": "rsa",
            "size": 2048,
        },
        "names": [
            {
                "C": "RU",
                "L": "Moscow",
                "O": "mb",
                "OU": "me",
                "ST": "MOS",
            }
        ]
    });
    let docker_cert_csr = serde_json::to_string(&docker_cert_csr)?;
    let docker_csr_path = path.join("docker-csr.json");
    tokio::fs::write(&docker_csr_path, &docker_cert_csr).await?;
    let docker_csr_path = docker_csr_path.display().to_string();

    let _p = xshell::pushd(&*crate::ROOT)?;
    let ca_settings: CaSettings = serde_json::from_slice(&tokio::fs::read("./etc/ca.json").await?)?;
    let ca_certificate = &ca_settings.certificate;
    let ca_private_key = &ca_settings.private_key;
    let docker_certs = xshell::cmd!(
        "cfssl gencert -ca {ca_certificate} -ca-key {ca_private_key} {docker_csr_path}"
    )
    .read()?;
    let docker_certs: serde_json::Value = serde_json::from_str(&docker_certs)?;
    let docker_certs: BTreeMap<_, _> = vec![
        (
            "crt".to_string(),
            docker_certs["cert"].as_str().unwrap().to_string(),
        ),
        (
            "key".to_string(),
            docker_certs["key"].as_str().unwrap().to_string(),
        ),
    ]
    .into_iter()
    .collect();
    let docker_certs = v1::Secret {
        string_data: Some(docker_certs),
        metadata: ObjectMeta {
            name: Some("registry-certs".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let docker_certs = serde_json::to_vec(&docker_certs)?;
    println!("Pushing certificates to k8s");
    let k = crate::kube().await?;
    let secrets_api = Api::<v1::Secret>::namespaced(k, "registry");
    secrets_api
        .patch("registry-certs", &PatchParams::apply("d-k8s"), docker_certs)
        .await?;
    println!("Certificates pushed");
    Ok(())
}

#[derive(serde::Deserialize)]
struct CaSettings {
    private_key: PathBuf,
    certificate: PathBuf,
}