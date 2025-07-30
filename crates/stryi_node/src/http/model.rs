use serde::Serialize;
use utoipa::ToSchema;
/// Value returned by `/version`.
#[derive(Serialize, ToSchema)]
pub struct VersionBody {
    pub(crate) version: u32,
}

