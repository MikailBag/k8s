mod admit;
mod copy_controller;
mod pv_controller;

use kube_utils::webhook::Server;
use rocket_contrib::json::Json;

#[tokio::main]
async fn main() {
    if let Ok(req) = std::env::var("PRINT") {
        handle_print_request(&req);
        return;
    }
    tracing_subscriber::fmt().init();
    let admission = async move {
        let rocket = try_rocket().await.expect("initialization error");
        let _ = rocket.launch().await;
    };
    let admission = tokio_compat_02::FutureExt::compat(admission);
    let kube_client = kube::Client::try_default()
        .await
        .expect("failed to connect to Kubernetes");
    let copy_controller = copy_controller::copy_to_ns_controller(&kube_client);
    let pv_controller = pv_controller::pv_controller(&kube_client);

    tokio::join!(admission, copy_controller, pv_controller);
}

fn handle_print_request(query: &str) {
    match query {
        "propagation-custom-resource-definition" => {
            let crd = copy_controller::crd();
            let crd = serde_json::to_string_pretty(&crd).expect("Failed to serialize CRD");
            println!("{}", crd);
        }
        _ => panic!("Unknown query '{}'", query),
    }
}

async fn try_rocket() -> anyhow::Result<rocket::Rocket> {
    Ok(rocket::ignite()
        .mount(
            "/",
            rocket::routes![health, admission_mutation, admission_validation],
        )
        .manage(make_server().await?))
}

async fn make_server() -> anyhow::Result<Server> {
    let resolver = admit::ImageRegistryResolver::new().await?;
    let mut server = Server::builder();
    server.add_reviewer(admit::PodReviewer::new(resolver));
    Ok(server.build())
}

#[rocket::get("/")]
fn health() -> &'static str {
    "OK"
}
/*
struct AnyhowResponder(anyhow::Error);

impl From<anyhow::Error> for AnyhowResponder {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}

impl<'r, 'o: 'r> rocket::response::Responder<'r, 'o> for AnyhowResponder {
    fn respond_to(self, _request: &'r rocket::Request<'_>) -> rocket::response::Result<'o> {
        tracing::error!("{:#}", self.0);
        Err(rocket::http::Status::InternalServerError)
    }
}
*/
#[rocket::post("/admission/mutate", data = "<review>")]
async fn admission_mutation(
    review: Json<kube_utils::webhook::apis::AdmissionReviewRequest>,
    server: rocket::State<'_, Server>,
) -> Json<kube_utils::webhook::apis::AdmissionReviewResponse> {
    Json(server.mutation(&review))
}
#[rocket::post("/admission/validate", data = "<review>")]
async fn admission_validation(
    review: Json<kube_utils::webhook::apis::AdmissionReviewRequest>,
    server: rocket::State<'_, Server>,
) -> Json<kube_utils::webhook::apis::AdmissionReviewResponse> {
    Json(server.validation(&review))
}
