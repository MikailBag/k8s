//use anyhow::Context as _;

pub async fn push(image: &str, name: &str) -> anyhow::Result<()> {
    // let k = crate::kube().await?;
    let registry_addr = crate::service_util::resolve_service("registry", "registry").await?;
    let new_tag = format!("{}/{}", registry_addr, name);
    xshell::cmd!("docker tag {image} {new_tag}").run()?;
    xshell::cmd!("docker push {new_tag}").run()?;
    xshell::cmd!("docker rmi {new_tag}").run()?;
    Ok(())
}
