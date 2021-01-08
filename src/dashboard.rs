use k8s_openapi::api::{core::v1};
use kube::Api;
use anyhow::Context as _;

pub async fn open() -> anyhow::Result<()> {
    println!("Connecting to kube");
    let k = crate::kube().await?;
    
    println!("Obtaining service NodePort");
    
    let addr = format!("https://{}", crate::service_util::resolve_service("kubernetes-dashboard","kubernetes-dashboard").await?);

    println!("Searching for service account");
    let secrets_api = Api::<v1::Secret>::namespaced(k.clone(), "kubernetes-dashboard");
    let secrets = secrets_api.list(&Default::default()).await?;
    let admin_secret = secrets.items.into_iter().filter(|sec| sec.metadata.name.as_ref().map_or(false, |nam| nam.starts_with("admin-user-token"))).next().context("no secrets matched")?;
    let secret_data = admin_secret.data.as_ref().context("secret data is empty")?;
    let token = secret_data.get("token").context("token missing")?;
    let token = String::from_utf8(token.0.clone()).context("invalid utf8 in token")?;
    let cmd = format!("echo -n {} | xclip -i -selection clipboard", token);
    xshell::cmd!("bash -c {cmd}").run()?;
    // println!("Token is {}", token);
    println!("Opening Dashboard: {}", &addr);
    xshell::cmd!("xdg-open {addr}").run()?;

    Ok(())
}

