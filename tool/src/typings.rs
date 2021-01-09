#[derive(serde::Deserialize)]
pub struct AdmissionReviewRequest {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    pub request: Request,
}

impl AdmissionReviewRequest {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.api_version != "admission.k8s.io/v1" {
            anyhow::bail!("unexpected apiVersion {}", self.api_version);
        }
        if self.kind != "AdmissionReview" {
            anyhow::bail!("unexpected kind {}", self.kind);
        }
        Ok(())
    }

    pub fn allow(self, new_object: &serde_json::Value) -> AdmissionReviewResponse {
        let patch = json_patch::diff(&self.request.object, new_object);
        let patch = serde_json::to_string(&patch).expect("failed to serialize a json patch");
        AdmissionReviewResponse::wrap(Response {
            allowed: true,
            uid: self.request.uid,
            status: None,
            patch: Some(Patch {
                patch_type: "JSONPatch".to_string(),
                patch: base64::encode(&patch),
            }),
        })
    }

    pub fn reject(self, message: &str) -> AdmissionReviewResponse {
        AdmissionReviewResponse::wrap(Response {
            allowed: false,
            uid: self.request.uid,
            status: Some(Status {
                code: 400,
                message: message.to_string(),
            }),
            patch: None,
        })
    }
}

#[derive(serde::Deserialize)]
pub struct Request {
    pub uid: String,
    pub object: serde_json::Value,
    pub namespace: String,
}

#[derive(serde::Serialize)]
pub struct AdmissionReviewResponse {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    response: Response,
}

impl AdmissionReviewResponse {
    fn wrap(response: Response) -> Self {
        AdmissionReviewResponse {
            api_version: "admission.k8s.io/v1".to_string(),
            kind: "AdmissionReview".to_string(),
            response,
        }
    }
}

#[derive(serde::Serialize)]
pub struct Response {
    uid: String,
    allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(flatten)]
    patch: Option<Patch>,
}
#[derive(serde::Serialize)]
pub struct Status {
    pub code: u16,
    pub message: String,
}

#[derive(serde::Serialize)]
pub struct Patch {
    #[serde(rename = "patchType")]
    patch_type: String,
    patch: String,
}
