mod typings;

use rocket_contrib::json::Json;

#[rocket::launch]
fn rocket() -> rocket::Rocket {
    rocket::ignite().mount("/", rocket::routes![health, admission])
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
        Err(rocket::http::Status::BadRequest)
    }
}

type Resp<T> = Result<Json<T>, AnyhowResponder>;

#[rocket::post("/admission", data = "<review>")]
async fn admission(
    review: Json<typings::AdmissionReviewRequest>,
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
    return Ok(Json(review.reject("ReJeCtEd")));
}
