use anyhow::Context as _;
use k8s_openapi::api::core::v1;
use kube::Api;
use std::collections::HashSet;

fn get_owner_name(p: &v1::Pod) -> Option<&str> {
    let meta = &p.metadata;
    let owner_refs = meta.owner_references.as_ref()?;
    if owner_refs.len() > 1 {
        eprintln!(
            "Warning: pod {} has >1 owners {:?}",
            meta.name.as_deref().unwrap_or("<unnamed>"),
            owner_refs
        )
    }
    let owner_ref = owner_refs.get(0)?;
    Some(&owner_ref.name)
}

pub async fn restart_deployment(k: &kube::Client, ns: &str, name: &str) -> anyhow::Result<()> {
    println!("Restarting deployment {}/{}", ns, name);

    println!("Scanning pods");
    let pods_api = Api::<v1::Pod>::namespaced(k.clone(), ns);
    let all_pods = pods_api
        .list(&Default::default())
        .await
        .context("Failed to list all pods")?
        .items;

    let mut evicted = HashSet::<String>::new();

    for pod in all_pods {
        let pod_name = pod.metadata.name.as_ref().unwrap();
        let owner = get_owner_name(&pod);
        let managed_msg = match owner {
            Some(owner) => format!("managed by {}", owner),
            None => "unmanaged".to_string(),
        };

        println!("{}: {}", pod_name, managed_msg);
        if let Some(owner) = owner {
            if owner.starts_with(name) {
                println!("Deleting {}", pod_name);
                pods_api.delete(pod_name, &Default::default()).await?;
                evicted.insert(pod_name.clone());
            }
        }
    }
    println!("Waiting until all pods are gone");
    loop {
        println!("Listing pods");
        let current_pods = pods_api.list(&Default::default()).await?.items;
        let mut has_running = false;

        for pod in current_pods {
            let pod_name = pod.metadata.name.as_ref().unwrap();
            if evicted.contains(pod_name) {
                println!("{} still alive", pod_name);
                has_running = true;
            }
        }

        if !has_running {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    Ok(())
}
