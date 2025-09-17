use crate::http::StryiHttpService;
use crate::http::error::{BlockIdentifier, ResourceKind, StryiNodeHttpApiError};
use crate::http::model::ApiErrorBody;
use crate::http::model::BlockResponse;
use axum::Json;
use axum::extract::{Path, State};
use std::sync::Arc;
use stryi_core::block::BlockHash;
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use tracing::error;

/// GET /api/block/{param}
/// `param` is either block height (u64) or block hash (hex).
#[utoipa::path(
    get,
    path = "/api/block/{param}",
    tag = "blocks",
    params(
        (
            "param" = String,
            Path,
            description = "Block height (u64) or block hash (string). Examples: `42`, `Bx7e09ff05219c8e14e8ffe148a9b23a824748cfb77bf0d424f4aff4d2b5b30d73`",
            format = "u64 | string",
            example = "42"
        )
    ),

    responses(
        (status = 200, description = "Block found", body = BlockResponse),
        (status = 400, description = "Invalid block identifier", body = ApiErrorBody,
            example = json!({
                "error": "invalid_resource_id",
                "message": "Expected block height (u64) or block hash in StryiChain's format (Bx...)",
                "details": { "resource": { "kind": "block" }, "requested": "forty-two" }
            })
        ),
        (status = 404, description = "Block not found", body = ApiErrorBody,
            example = json!({
                "error": "resource_not_found",
                "message": "Block 42 was not found.",
                "details": { "resource": { "kind": "block", "label": "Block 42" } }
            })
        ),
        (status = 500, description = "Internal server error", body = ApiErrorBody,
            example = json!({
                "error": "unexpected_error",
                "message": "An unexpected error occurred."
            })
        )
    )
)]
pub async fn get_block<DB>(
    State(svc): State<Arc<StryiHttpService<DB>>>,
    Path(param): Path<String>,
) -> Result<Json<BlockResponse>, StryiNodeHttpApiError>
where
    DB: BlockStorage + UtxoStorage + StorageStats + Send + Sync + 'static,
{
    let store = svc.storage.read().await;

    // Try as height first
    if let Ok(height) = param.parse::<u64>() {
        return match store.get_block_by_height(height).await {
            Ok(Some(block)) => {
                let hash = block.block_hash();
                Ok(Json(BlockResponse {
                    hash,
                    block: serde_json::to_value(block)
                        .map_err(|e| StryiNodeHttpApiError::Unexpected(e.to_string()))?,
                }))
            }

            Ok(None) => Err(StryiNodeHttpApiError::ResourceNotFound {
                resource: ResourceKind::Block(BlockIdentifier::Height(height)),
            }),

            Err(e) => {
                error!("get_block_by_height({height}) failed: {e}");
                Err(StryiNodeHttpApiError::Unexpected(e.to_string()))
            }
        };
    }

    // If not a height, try as hash
    let hash = BlockHash::from_hash_string(param.as_str()).map_err(|e| {
        StryiNodeHttpApiError::InvalidResourceId {
            resource_kind: "block".to_string(),
            requested: param,
            message: e.to_string(),
        }
    })?;

    match store.get_block_by_hash(hash).await {
        // If the block is found, serialize it to JSON and return
        Ok(Some(block)) => Ok(Json(BlockResponse {
            hash,
            block: serde_json::to_value(block)
                .map_err(|e| StryiNodeHttpApiError::Unexpected(e.to_string()))?,
        })),

        // If the block is not found, return a 404 error
        Ok(None) => Err(StryiNodeHttpApiError::ResourceNotFound {
            resource: ResourceKind::Block(BlockIdentifier::Hash(hash)),
        }),

        // If there was an error retrieving the block, log it and return an error
        Err(e) => {
            error!("get_block_by_hash({hash}) failed: {e}");
            Err(StryiNodeHttpApiError::Unexpected(e.to_string()))
        }
    }
}
