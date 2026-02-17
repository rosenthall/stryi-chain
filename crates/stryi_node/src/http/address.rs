use crate::http::StryiHttpService;
use crate::http::error::StryiNodeHttpApiError;
use crate::http::model::{AddressBalanceResponse, ApiErrorBody, UtxoEntry};
use axum::Json;
use axum::extract::{Path, State};
use std::sync::Arc;
use stryi_core::address::AccountAddress;
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use tracing::error;

/// GET /api/address/{addr}/balance
/// Returns the total balance and list of UTXOs for a given address.
#[utoipa::path(
    get,
    path = "/api/address/{addr}/balance",
    tag = "address",
    params(
        (
            "addr" = String,
            Path,
            description = "Account address",
            example = "@dd0ca8155d946853106a5a5fb126ce17fdd9136c"
        )
    ),
    responses(
        (status = 200, description = "Balance and UTXOs for the address", body = AddressBalanceResponse),
        (status = 400, description = "Invalid address format", body = ApiErrorBody,
            example = json!({
                "error": "invalid_resource_id",
                "message": "Expected addres in StryiChain format (@...)",
                "details": { "resource": { "kind": "account" }, "requested": "invalid" }
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
pub async fn get_balance<DB>(
    State(svc): State<Arc<StryiHttpService<DB>>>,
    Path(addr): Path<String>,
) -> Result<Json<AddressBalanceResponse>, StryiNodeHttpApiError>
where
    DB: BlockStorage + UtxoStorage + StorageStats + Send + Sync + 'static,
{
    let address = AccountAddress::from_hash_string(addr.as_str()).map_err(|e| {
        StryiNodeHttpApiError::InvalidResourceId {
            resource_kind: "account".to_string(),
            requested: addr.clone(),
            message: e.to_string(),
        }
    })?;

    let store = svc.storage.read().await;
    let utxo_map = store.get_utxos_for_address(address).await.map_err(|e| {
        error!("get_utxos_for_address({address}) failed: {e}");
        StryiNodeHttpApiError::Unexpected(e.to_string())
    })?;

    let mut balance: u64 = 0;
    let utxos: Vec<UtxoEntry> = utxo_map
        .into_iter()
        .map(|(outpoint, utxo)| {
            balance = balance.saturating_add(utxo.value);
            UtxoEntry {
                txid: outpoint.txid,
                vout: outpoint.vout,
                value: utxo.value,
            }
        })
        .collect();

    Ok(Json(AddressBalanceResponse {
        address: address.to_string(),
        balance,
        utxos,
    }))
}
