use axum::extract::State;
use std::sync::Arc;
use axum::Json;
use stryi_core::storage::{BlockStorage, UtxoStorage};
use crate::http::model::VersionBody;
use crate::http::StryiHttpService;

/// `/version` endpoint handler
#[utoipa::path(
    get,
    path = "/version",
    responses(
        (status = 200, description = "Current HTTP API version", body = VersionBody)
    )
)]
pub async fn get_version<DB>(
    State(svc): State<Arc<StryiHttpService<DB>>>,
) -> Json<VersionBody>
where
    DB: BlockStorage + UtxoStorage + Send + Sync + 'static,
{
    Json(VersionBody {
        version: svc.config.api_version,
    })
}