use axum::{Json, extract::State};
use http::StatusCode;
use std::sync::Arc;
use tracing::error;

use crate::http::{StryiHttpService, model::NodeStateBody};
use stryi_core::storage::{StorageReader, StorageStats};

#[utoipa::path(
    get,
    path = "/api/nodestate",
    responses(
        (status = 200, description = "Current node state", body = NodeStateBody),
        (status = 500, description = "Internal storage error")
    )
)]
pub async fn get_nodestate<DB>(
    State(svc): State<Arc<StryiHttpService<DB>>>,
) -> Result<Json<NodeStateBody>, StatusCode>
where
    DB: StorageReader,
{
    let storage = svc.storage.clone();

    let state = tokio::task::spawn_blocking(move || {
        storage
            .read()?
            .chain_state()?
            .ok_or(stryi_core::storage::StorageError::Uninitialized)
    })
    .await
    .map_err(|e| {
        error!("node state worker failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .map_err(|e| {
        error!("cannot read chain state: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (height, hash) = state.latest_block;
    let body = NodeStateBody {
        chain_name: svc.config.chain_name.clone(),
        api_version: svc.config.api_version,
        height,
        latest_block_hash: hash.to_string(),
        chain_work: state.chain_work,
        last_update_time: state.last_update_time,
    };

    Ok(Json(body))
}
