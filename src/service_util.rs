use anyhow::Context as _;
use crate::vm::VmState;
use kube::Api;
use k8s_openapi::api::core::v1;

pub async fn resolve_service(ns: &str, name: &str) -> anyhow::Result<String> {
    let k = crate::kube().await?;
    let svc_api = Api::<v1::Service>::namespaced(k, ns);
    let dash = svc_api.get(name).await?;
    let port = get_node_port(&dash).context("NodePort missing")?;
    let vm_state: VmState = serde_json::from_str(&tokio::fs::read_to_string(crate::ROOT.join("state/vm.json")).await?)?;
    Ok(format!("{}:{}", vm_state.ip, port))
}


fn get_node_port(svc: &v1::Service) -> anyhow::Result<String> {
    let spec = svc.spec.as_ref().context("spec missing")?;
    let ports = spec.ports.as_ref().context(".spec.ports missing")?;
    anyhow::ensure!(ports.len() == 1, "expected 1 port in the service but got {}", ports.len());
    let port = &ports[0];
    let port_number = port.node_port.context(".spec.ports[0].nodePort missing")?;
    Ok(port_number.to_string())
}
