use axum::{Json, extract::State};
use http::StatusCode;
use std::sync::Arc;
use tracing::error;

use crate::http::{StryiHttpService, model::NodeStateBody};
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};

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
    DB: BlockStorage + UtxoStorage + StorageStats + Send + Sync + 'static,
{
    let store = svc.storage.read().await;

    // Helper macro to reduce boilerplate for each stat call.
    macro_rules! fetch {
        ($expr:expr, $label:literal) => {
            match $expr.await {
                Ok(v) => v,
                Err(e) => {
                    error!("StorageStats::{} failed: {}", $label, e);
                    return Err(StatusCode::INTERNAL_SERVER_ERROR);
                }
            }
        };
    }

    let (height, hash) = fetch!(store.tip(), "tip");
    let last_update = fetch!(store.last_updated(), "last_updated");
    let total_difficulty = fetch!(store.chain_difficulty(), "chain_difficulty");

    let body = NodeStateBody {
        chain_name: svc.config.chain_name.clone(),
        api_version: svc.config.api_version,
        height,
        latest_block_hash: hash.to_string(),
        total_difficulty,
        last_update_time: last_update,
    };

    Ok(Json(body))
}
