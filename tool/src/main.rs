mod typings;

use anyhow::Context as _;
use futures::stream::StreamExt;
use k8s_openapi::api::core::v1;
use rocket_contrib::json::Json;

#[tokio::main]
async fn main() {
    let fut = async move {
        let rocket = try_rocket().await.expect("initialization error");
        let _ = rocket.launch().await;
    };
    tokio_compat_02::FutureExt::compat(fut).await;
}

async fn try_rocket() -> anyhow::Result<rocket::Rocket> {
    Ok(rocket::ignite()
        .mount("/", rocket::routes![health, admission])
        .manage(ImageRegistryResolver::new().await?))
}

#[rocket::get("/")]
fn health() -> &'static str {
    "OK"
}

struct AnyhowResponder(anyhow::Error);

impl From<anyhow::Error> for AnyhowResponder {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}

impl<'r, 'o: 'r> rocket::response::Responder<'r, 'o> for AnyhowResponder {
    fn respond_to(self, _request: &'r rocket::Request<'_>) -> rocket::response::Result<'o> {
        log::error!("{:#}", self.0);
        Err(rocket::http::Status::InternalServerError)
    }
}

type Resp<T> = Result<Json<T>, AnyhowResponder>;

struct ImageRegistryResolver {
    services: kube_runtime::reflector::Store<v1::Service>,
    nodes: kube_runtime::reflector::Store<v1::Node>,
}

fn make_store<K: kube::api::Meta + Clone + Send + Sync + serde::de::DeserializeOwned>(
    api: kube::Api<K>,
) -> kube_runtime::reflector::Store<K> {
    let watcher = kube_runtime::watcher(api, Default::default());
    let writer = kube_runtime::reflector::store::Writer::default();
    let store = writer.as_reader();
    let reflector = kube_runtime::reflector::reflector(writer, watcher);
    tokio::task::spawn(async move {
        tokio::pin!(reflector);
        while let Some(item) = reflector.next().await {
            if let Err(e) = item {
                log::warn!("services watcher: error: {}", e);
            }
        }
    });
    store
}

impl ImageRegistryResolver {
    async fn new() -> anyhow::Result<ImageRegistryResolver> {
        let k = kube::Client::try_default().await?;
        Ok(ImageRegistryResolver {
            services: make_store(kube::Api::namespaced(k.clone(), "registry")),
            nodes: make_store(kube::Api::all(k)),
        })
    }

    fn resolve_svc(&self, ns: &str, name: &str) -> anyhow::Result<String> {
        let nodes = self.nodes.state();
        anyhow::ensure!(nodes.len() == 1);
        let ip = {
            let n = &nodes[0];
            let meta = &n.metadata;
            let anns = meta
                .annotations
                .as_ref()
                .context("no annotations on Node object")?;
            anns.get("d-k8s.io/public-ip")
                .context("annotation missing")?
                .clone()
        };
        let obj_ref = kube_runtime::reflector::ObjectRef::new(name).within(ns);
        let svc = self.services.get(&obj_ref).context("unknown service")?;
        let ports = svc
            .spec
            .context("service spec missing")?
            .ports
            .context("service ports missing")?;
        anyhow::ensure!(ports.len() == 1);
        let port = &ports[0];
        let port = port.node_port.context("NodePort missing")?;
        Ok(format!("{}:{}", ip, port))
    }
}

fn patch_pod(obj: &mut serde_json::Value, repo_addr: &str) -> Option<()> {
    let containers = obj.pointer_mut("/spec/containers")?;
    let containers = containers.as_array_mut()?;
    for container in containers {
        let container = container.as_object_mut()?;
        let image = container.get_mut("image")?;
        let cur_image = image.as_str()?;
        if let Some(suf) = cur_image.strip_prefix("cr.local/") {
            *image = format!("{}/{}", repo_addr, suf).into();
        }
    }
    Some(())
}

#[rocket::post("/admission", data = "<review>")]
async fn admission(
    review: Json<typings::AdmissionReviewRequest>,
    resolver: rocket::State<'_, ImageRegistryResolver>,
) -> Resp<typings::AdmissionReviewResponse> {
    let review = review.into_inner();
    review.validate()?;
    const SKIP_LIST: &[&str] = &["kube-system", "admission"];
    if SKIP_LIST.contains(&review.request.namespace.as_str()) {
        log::info!(
            "skipping request: critical namespace {}",
            review.request.namespace
        );
        let obj = review.request.object.clone();
        return Ok(Json(review.allow(&obj)));
    }
    let mut obj = review.request.object.clone();
    let registry_addr = resolver.resolve_svc("registry", "registry")?;
    patch_pod(&mut obj, &registry_addr);
    Ok(Json(review.allow(&obj)))
}
