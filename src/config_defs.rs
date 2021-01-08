use std::path::PathBuf;
#[derive(serde::Deserialize)]
pub struct CaSettings {
    pub private_key: PathBuf,
    pub certificate: PathBuf,
}
