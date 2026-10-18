use crate::http::StryiHttpService;
use crate::http::error::{BadTxReason, ResourceKind, StryiNodeHttpApiError};
use crate::http::model::{
    ApiErrorBody, SendTransactionRequest, TransactionQueryResponse, TransactionQueryStatus,
    TransactionResponse,
};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::response::Response;
use base64::Engine;
use base64::prelude::BASE64_STANDARD;

use http::StatusCode;
use std::sync::Arc;
use stryi_core::address::AccountAddress;
use stryi_core::mempool::{MemPoolError, MempoolValidationError};
use stryi_core::storage::{BlockStorage, StorageReader};
use stryi_core::transactions::{Transaction, TransactionHash};
use tracing::{debug, info};

/// Send a postcard-encoded transaction wrapped in base64 to the node.
///
/// The server decodes base64, deserializes `Transaction` via postcard,
/// recovers & verifies its signature, and (for now) accepts it.
/// Returns plain text "Transaction accepted" on success.
#[utoipa::path(
    post,
    path = "/api/tx",
    tag = "transactions",
    request_body = SendTransactionRequest,
    responses(
        (status = 200, description = "Transaction accepted",
            content_type = "text/plain", body = String, example = "Transaction accepted"),
        (status = 400, description = "Bad transaction", body = ApiErrorBody,
            example = json!({
                "error": "bad_transaction",
                "message": "Invalid transaction signature.",
                "details": { "reason": "invalid_signature" }
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
pub async fn send_tx<DB>(
    State(state): State<Arc<StryiHttpService<DB>>>,
    Json(req): Json<SendTransactionRequest>,
) -> Result<Response, StryiNodeHttpApiError>
where
    DB: StorageReader,
{
    // decode the base64-encoded transaction
    let raw_tx =
        BASE64_STANDARD
            .decode(req.raw_tx)
            .map_err(|_| StryiNodeHttpApiError::BadTransaction {
                reason: BadTxReason::Base64Decode,
                message: Some("Cannot decode transaction (base64)".to_string()),
            })?;
    debug!(
        "Successfully decoded transaction from base64, length: {}",
        raw_tx.len()
    );

    // Try to deserialize the raw transaction into a Transaction object from postcard format
    let transaction: Transaction =
        postcard::from_bytes(&raw_tx).map_err(|_| StryiNodeHttpApiError::BadTransaction {
            reason: BadTxReason::Deserialize,
            message: Some("Cannot deserialize transaction (postcard)".to_string()),
        })?;
    debug!(
        "Successfully deserialized transaction, hash: {}",
        transaction.data.hash()
    );

    // Validate transaction's author
    let author_verifying_key =
        transaction
            .recover_public_key()
            .map_err(|_| StryiNodeHttpApiError::BadTransaction {
                reason: BadTxReason::RecoverPublicKey,
                message: Some("Cannot recover public key from transaction".to_string()),
            })?;
    debug!("Recovered tx author public key!");

    // Verify the transaction's signature using the recovered public key
    transaction
        .verify_signature(&author_verifying_key)
        .map_err(|_| StryiNodeHttpApiError::BadTransaction {
            reason: BadTxReason::InvalidSignature,
            message: Some("Invalid transaction signature".to_string()),
        })?;
    debug!("Transaction signature is valid!");

    // Validate the transaction's author to ensure it matches the expected author

    // Create AccountAddress based on the verifying key just to log it
    let author_address = AccountAddress::new(&author_verifying_key.to_sec1_bytes());
    debug!("Transaction author address: {}", author_address);

    // Add the transaction to the mempool
    let mut mem = state.mempool.write().await;
    mem.add_transaction(transaction.clone())
        .await
        .map_err(map_mempool)?; // Map MemPoolError to StryiNodeHttpApiError if it occurs

    drop(mem);

    info!(
        "Received and added to mempool transaction from {}: {}",
        author_address,
        transaction.data.hash()
    );

    // Broadcast to network peers
    state.tx_broadcaster.publish_tx(transaction).await;

    // Return a success response
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Body::from("Transaction accepted"))
        .unwrap())
}

#[utoipa::path(
    get,
    path = "/api/tx/{hash}",
    tag = "transactions",
    params(
        (
            "hash" = String,
            Path,
            description = "Transaction hash in StryiChain format (Tx...)",
            example = "Tx68e8dfa2225f7776c2f141e500bb27da09cb9e241dfc67b82573d7439ced28b2"
        )
    ),
    responses(
        (status = 200, description = "Transaction found", body = TransactionQueryResponse),
        (status = 400, description = "Invalid transaction hash", body = ApiErrorBody,
            example = json!({
                "error": "invalid_resource_id",
                "message": "Expected transaction hash in StryiChain's format (Tx...)",
                "details": { "resource": { "kind": "transaction" }, "requested": "oops" }
            })
        ),
        (status = 404, description = "Transaction not found", body = ApiErrorBody,
            example = json!({
                "error": "resource_not_found",
                "message": "Transaction Tx68e8dfa2225f7776c2f141e500bb27da09cb9e241dfc67b82573d7439ced28b2 was not found.",
                "details": { "resource": { "kind": "transaction", "label": "Transaction Tx68e8dfa2225f7776c2f141e500bb27da09cb9e241dfc67b82573d7439ced28b2" } }
            })
        )
    )
)]
pub async fn get_tx<DB>(
    State(state): State<Arc<StryiHttpService<DB>>>,
    Path(hash): Path<String>,
) -> Result<Json<TransactionQueryResponse>, StryiNodeHttpApiError>
where
    DB: StorageReader,
{
    let tx_hash = TransactionHash::from_hash_string(hash.as_str()).map_err(|e| {
        StryiNodeHttpApiError::InvalidResourceId {
            resource_kind: "transaction".to_string(),
            requested: hash,
            message: e.to_string(),
        }
    })?;

    let storage = state.storage.clone();

    let confirmed = tokio::task::spawn_blocking(move || {
        let view = storage
            .read()
            .map_err(|e| StryiNodeHttpApiError::Unexpected(e.to_string()))?;

        view.get_transaction(&tx_hash)
            .map_err(|e| StryiNodeHttpApiError::Unexpected(e.to_string()))
    })
    .await
    .map_err(|e| StryiNodeHttpApiError::Unexpected(e.to_string()))??;

    if let Some(record) = confirmed {
        return Ok(Json(TransactionQueryResponse {
            status: TransactionQueryStatus::Confirmed,
            tx_hash,
            transaction: TransactionResponse::from(record.transaction),
            block_hash: Some(record.location.block_hash),
            block_height: Some(record.location.block_height),
            tx_index: Some(record.location.tx_index),
        }));
    }

    {
        let mempool = state.mempool.read().await;

        if let Some(transaction) = mempool.get_transaction(&tx_hash) {
            return Ok(Json(TransactionQueryResponse {
                status: TransactionQueryStatus::Pending,
                tx_hash,
                transaction: TransactionResponse::from(transaction),
                block_hash: None,
                block_height: None,
                tx_index: None,
            }));
        }
    }

    Err(StryiNodeHttpApiError::ResourceNotFound {
        resource: ResourceKind::Transaction(tx_hash),
    })
}

/// Maps MemPoolError to StryiNodeHttpApiError for HTTP responses
fn map_mempool(err: MemPoolError) -> StryiNodeHttpApiError {
    use BadTxReason::*;
    match err {
        MemPoolError::DuplicateTransaction { .. } => StryiNodeHttpApiError::BadTransaction {
            reason: DuplicateTx,
            message: Some("Such transaction already in mempool".into()),
        },
        MemPoolError::DoubleSpend(outpoint) => StryiNodeHttpApiError::BadTransaction {
            reason: DoubleSpend,
            message: format!("Double spend detected: {:?}", outpoint).into(),
        },
        MemPoolError::PoolFull { .. } => StryiNodeHttpApiError::BadTransaction {
            reason: PoolFull,
            message: Some("mempool is full".into()),
        },
        MemPoolError::InsufficientFee { required, actual } => {
            let msg = format!("required {required}, provided {actual}");
            StryiNodeHttpApiError::BadTransaction {
                reason: InsufficientFee,
                message: Some(msg),
            }
        }
        MemPoolError::ValidationError(inner) => match inner {
            MempoolValidationError::SignatureFailed(_) => StryiNodeHttpApiError::BadTransaction {
                reason: InvalidSignature,
                message: None,
            },
            _ => StryiNodeHttpApiError::BadTransaction {
                reason: InvalidTransaction,
                message: Some(inner.to_string()),
            },
        },
        other => StryiNodeHttpApiError::Unexpected(other.to_string()),
    }
}
