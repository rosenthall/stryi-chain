use std::sync::Arc;
use axum::{extract::State, Json};
use axum::response::Response;
use axum::body::Body;
use base64::prelude::BASE64_STANDARD;
use base64::Engine;
use bincode::config::standard;
use bincode::serde::decode_borrowed_from_slice;
use http::StatusCode;
use tracing::{debug, info};
use stryi_core::address::AccountAddress;
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use stryi_core::transactions::Transaction;

use crate::http::StryiHttpService;
use crate::http::model::SendTransactionRequest;
use crate::http::error::{StryiNodeHttpApiError, BadTxReason};



/// Send a bincode-encoded transaction wrapped in base64 to the node.
///
/// The server decodes base64, deserializes bincode `Transaction`,
/// recovers & verifies its signature, and (for now) accepts it.
/// Returns plain text "Transaction accepted" on success.
#[utoipa::path(
    post,
    path = "/api/tx",
    tag = "transactions",
    request_body = SendTransactionRequest,
    responses(
        // Your handler returns plain text. Document it explicitly.
        (status = 200, description = "Transaction accepted", content_type = "text/plain", body = String, example = "Transaction accepted"),
        // Map your domain error into a public error body for docs.
        (status = 400, description = "Bad transaction", body = StryiNodeHttpApiError),
        (status = 500, description = "Internal server error", body = StryiNodeHttpApiError)
    )
)]
pub async fn send_tx<DB>(
    State(_state): State<Arc<StryiHttpService<DB>>>,
    Json(req): Json<SendTransactionRequest>,
) -> Result<Response, StryiNodeHttpApiError>
where
    DB: BlockStorage + UtxoStorage + StorageStats + Send + Sync + 'static,
{
    // decode the base64-encoded transaction
    let raw_tx = BASE64_STANDARD
        .decode(req.raw_tx)
        .map_err(|_| StryiNodeHttpApiError::BadTransaction {
            reason: BadTxReason::Base64Decode,
            message: Some("Cannot decode transaction (base64)".to_string()),
        })?;
    debug!("Successfully decoded transaction from base64, length: {}", raw_tx.len());

    // Try to deserialize the raw transaction into a Transaction object from bincode format
    let transaction: Transaction = decode_borrowed_from_slice(&raw_tx, standard())
        .map_err(|_| StryiNodeHttpApiError::BadTransaction {
            reason: BadTxReason::BincodeDeserialize,
            message: Some("Cannot deserialize transaction (bincode)".to_string()),
        })?;
    debug!("Successfully deserialized transaction, hash: {}", transaction.data.hash());

    // Validate transaction's author
    let author_verifying_key = transaction
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

    // TODO: Integrate mempool and storage to validate and handle new transactions in http service

    // For now, we will just pretty log the transaction and return a success response.
    info!("Received transaction from {}: {}", author_address, transaction.data.hash());

    // Return a success response
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Body::from("Transaction accepted"))
        .unwrap())
}

