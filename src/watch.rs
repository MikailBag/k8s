use anyhow::Context as _;
use kube::Api;

pub trait Health:
    k8s_openapi::Resource + Clone + serde::de::DeserializeOwned + kube::api::Meta
{
    fn is_healthy(&self) -> anyhow::Result<bool>;
}

impl Health for k8s_openapi::api::apps::v1::Deployment {
    fn is_healthy(&self) -> anyhow::Result<bool> {
        let status = self.status.as_ref().context(".status missing")?;
        let conditions = status
            .conditions
            .as_ref()
            .context(".status.conditions missing")?;
        for cond in conditions {
            if cond.type_ == "Available" {
                return Ok(cond.status == "True");
            }
        }
        anyhow::bail!("Condition 'Available' missing");
    }
}

const SLEEP_TIME_SECS: u64 = 3;

pub async fn watch<H: Health>(
    k: &kube::Client,
    ns: &str,
    name: &str,
    timeout: u64,
) -> anyhow::Result<()> {
    println!(
        "Waiting for {} {}/{} with timeout of {} seconds",
        H::KIND,
        ns,
        name,
        timeout
    );

    let api = Api::<H>::namespaced(k.clone(), ns);
    let _initial_state = api
        .get(name)
        .await
        .context("Resource does not exist or is not available")?;
    let deadline = std::time::Duration::from_secs(timeout);
    let begin = std::time::Instant::now();
    loop {
        if std::time::Instant::now().duration_since(begin) > deadline {
            anyhow::bail!("Deadline exceeded");
        }

        let state = api.get(name).await?;
        let health = state.is_healthy();
        match health {
            Ok(true) => {
                println!("Resource ready");
                return Ok(());
            }
            Ok(false) => {
                println!("Resource not ready yet");
            }
            Err(err) => {
                println!("Warning: {:#}", err);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(SLEEP_TIME_SECS)).await;
    }
}
