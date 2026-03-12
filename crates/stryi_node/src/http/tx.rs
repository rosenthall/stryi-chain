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
use bincode::config::standard;
use bincode::serde::borrow_decode_from_slice;
use futures_util::future::BoxFuture;
use http::StatusCode;
use std::sync::Arc;
use stryi_core::address::AccountAddress;
use stryi_core::mempool::{MemPoolError, MempoolValidationError};
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use stryi_core::transactions::{Transaction, TransactionHash};
use stryi_storage::{StryiStorage, StryiStorageError};
use tracing::{debug, info};

#[derive(Debug, Clone)]
pub(crate) struct ConfirmedTransactionRecord {
    pub tx_hash: TransactionHash,
    pub transaction: Transaction,
    pub block_hash: stryi_core::block::BlockHash,
    pub block_height: u64,
    pub tx_index: u32,
}

pub(crate) trait ConfirmedTransactionLookup {
    type LookupError: std::error::Error + Send + Sync + 'static;

    fn get_confirmed_transaction(
        &self,
        tx_hash: TransactionHash,
    ) -> BoxFuture<'_, Result<Option<ConfirmedTransactionRecord>, Self::LookupError>>;
}

impl ConfirmedTransactionLookup for StryiStorage {
    type LookupError = StryiStorageError;

    fn get_confirmed_transaction(
        &self,
        tx_hash: TransactionHash,
    ) -> BoxFuture<'_, Result<Option<ConfirmedTransactionRecord>, Self::LookupError>> {
        Box::pin(async move {
            let Some(index) = self.get_transaction_index(&tx_hash)? else {
                return Ok(None);
            };

            let Some(block) = self.get_block_by_hash(index.block_hash).await? else {
                return Err(StryiStorageError::NotFound(format!(
                    "block {} for transaction {}",
                    index.block_hash, tx_hash
                )));
            };

            let Some(transaction) = block.data.transactions.get(index.tx_index as usize) else {
                return Err(StryiStorageError::NotFound(format!(
                    "transaction index {} in block {}",
                    index.tx_index, index.block_hash
                )));
            };

            if transaction.data.hash() != tx_hash {
                return Err(StryiStorageError::NotFound(format!(
                    "transaction {} no longer matches block {} at index {}",
                    tx_hash, index.block_hash, index.tx_index
                )));
            }

            Ok(Some(ConfirmedTransactionRecord {
                tx_hash,
                transaction: transaction.clone(),
                block_hash: index.block_hash,
                block_height: index.block_height,
                tx_index: index.tx_index,
            }))
        })
    }
}

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
    DB: BlockStorage + UtxoStorage + StorageStats + Send + Sync + 'static,
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

    // Try to deserialize the raw transaction into a Transaction object from bincode format
    let transaction: Transaction = borrow_decode_from_slice(&raw_tx, standard())
        .map_err(|_| StryiNodeHttpApiError::BadTransaction {
            reason: BadTxReason::BincodeDeserialize,
            message: Some("Cannot deserialize transaction (bincode)".to_string()),
        })?
        .0;
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
    DB: BlockStorage
        + UtxoStorage
        + StorageStats
        + ConfirmedTransactionLookup
        + Send
        + Sync
        + 'static,
{
    let tx_hash = TransactionHash::from_hash_string(hash.as_str()).map_err(|e| {
        StryiNodeHttpApiError::InvalidResourceId {
            resource_kind: "transaction".to_string(),
            requested: hash,
            message: e.to_string(),
        }
    })?;

    let storage = state.storage.read().await;
    let record = storage
        .get_confirmed_transaction(tx_hash)
        .await
        .map_err(|e| StryiNodeHttpApiError::Unexpected(e.to_string()))?;
    drop(storage);

    if let Some(record) = record {
        return Ok(Json(TransactionQueryResponse {
            status: TransactionQueryStatus::Confirmed,
            tx_hash: record.tx_hash,
            transaction: TransactionResponse::from(record.transaction),
            block_hash: Some(record.block_hash),
            block_height: Some(record.block_height),
            tx_index: Some(record.tx_index),
        }));
    }

    {
        let mempool = state.mempool.read().await;
        if let Some(transaction) = mempool.get_transaction(&tx_hash).await {
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
        // duplicate tx -> 400
        MemPoolError::DuplicateTransaction { .. } => StryiNodeHttpApiError::BadTransaction {
            reason: DuplicateTx,
            message: Some("Such transaction already in mempool".into()),
        },
        // already-spent input -> 400
        MemPoolError::DoubleSpend(outpoint) => StryiNodeHttpApiError::BadTransaction {
            reason: DoubleSpend,
            message: format!("Double spend detected: {:?}", outpoint).into(),
        },
        // mempool size cap hit -> 400
        MemPoolError::PoolFull { .. } => StryiNodeHttpApiError::BadTransaction {
            reason: PoolFull,
            message: Some("mempool is full".into()),
        },
        // insufficient fee for RBF -> 400
        MemPoolError::InsufficientFee { required, actual } => {
            let msg = format!("required {required}, provided {actual}");
            StryiNodeHttpApiError::BadTransaction {
                reason: InsufficientFee,
                message: Some(msg),
            }
        }
        // validation error -> map inner enum
        MemPoolError::ValidationError(inner) => match inner {
            MempoolValidationError::SignatureFailed(_) => StryiNodeHttpApiError::BadTransaction {
                reason: InvalidSignature,
                message: None,
            },
            // the rest fall back to generic invalid-signature bucket
            _ => StryiNodeHttpApiError::BadTransaction {
                reason: InvalidSignature,
                message: Some(inner.to_string()),
            },
        },
        // anything else → 500
        other => StryiNodeHttpApiError::Unexpected(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::StryiHttpServiceConfig;
    use crate::http::TxBroadcaster;
    use std::path::PathBuf;
    use stryi_core::address::AccountAddress;
    use stryi_core::mempool::{MemPool, MemPoolConfig, MemPoolSyncData, UtxoLookup};
    use stryi_core::transactions::{TransactionData, TransactionKind, TransactionOut};
    use stryi_network::NetworkCommand;
    use stryi_storage::StryiStorage;
    use tokio::sync::{RwLock, mpsc};

    fn dummy_lookup() -> UtxoLookup {
        Box::new(|_| Box::pin(async { None }))
    }

    fn make_test_tx(value: u64, recipient_seed: u8) -> Transaction {
        Transaction::new_unsigned(TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![],
            outputs: vec![TransactionOut {
                value,
                recipient: AccountAddress::new(&[recipient_seed; 20]),
            }],
        })
    }

    fn temp_storage_path() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("stryi-node-http-tx-{nanos}"))
    }

    async fn make_service() -> Arc<StryiHttpService<StryiStorage>> {
        let storage = StryiStorage::initialize_in_path(temp_storage_path(), None)
            .await
            .expect("storage");
        let mempool = MemPool::new(MemPoolConfig::default(), dummy_lookup());
        let (sender, _receiver) = mpsc::channel::<NetworkCommand>(1);

        let svc = StryiHttpService {
            config: StryiHttpServiceConfig {
                address: "127.0.0.1:0".parse().expect("socket"),
                chain_name: "test".to_string(),
                peer_id: stryi_network::PeerId::random(),
                api_version: 1,
            },
            mempool: Arc::new(RwLock::new(mempool)),
            storage: Arc::new(RwLock::new(storage)),
            tx_broadcaster: TxBroadcaster::new(sender),
        };

        Arc::new(svc)
    }

    #[tokio::test]
    async fn query_pending_transaction_returns_pending_status() {
        let svc = make_service().await;
        let tx = make_test_tx(10, 1);
        let tx_hash = tx.data.hash();

        svc.mempool
            .write()
            .await
            .restore_state(
                bincode::serde::encode_to_vec(
                    MemPoolSyncData {
                        transactions: vec![tx.clone()],
                        timestamp: 0,
                    },
                    standard(),
                )
                .expect("encode mempool sync state"),
            )
            .await
            .expect("mempool restore");

        let response = get_tx::<StryiStorage>(State(svc), Path(tx_hash.to_string()))
            .await
            .expect("pending tx response");

        assert_eq!(response.0.status, TransactionQueryStatus::Pending);
        assert_eq!(response.0.tx_hash, tx_hash);
        assert!(response.0.block_hash.is_none());
    }

    #[tokio::test]
    async fn query_confirmed_transaction_returns_location() {
        let svc = make_service().await;
        let tx = make_test_tx(20, 2);
        let block = stryi_core::block::Block::new(
            vec![tx.clone()],
            stryi_core::block::BlockHash::empty(),
            1,
            1,
            1_700_000_010,
            1,
        );

        let tx_hash = tx.data.hash();
        svc.storage
            .write()
            .await
            .put_block(&block)
            .await
            .expect("put block");

        let response = get_tx::<StryiStorage>(State(svc), Path(tx_hash.to_string()))
            .await
            .expect("confirmed tx response");

        assert_eq!(response.0.status, TransactionQueryStatus::Confirmed);
        assert_eq!(response.0.tx_hash, tx_hash);
        assert_eq!(response.0.block_hash, Some(block.block_hash()));
        assert_eq!(response.0.block_height, Some(1));
        assert_eq!(response.0.tx_index, Some(0));
    }

    #[tokio::test]
    async fn query_confirmed_transaction_wins_over_mempool_copy() {
        let svc = make_service().await;
        let tx = make_test_tx(30, 3);
        let tx_hash = tx.data.hash();
        let block = stryi_core::block::Block::new(
            vec![tx.clone()],
            stryi_core::block::BlockHash::empty(),
            1,
            1,
            1_700_000_020,
            1,
        );

        svc.storage
            .write()
            .await
            .put_block(&block)
            .await
            .expect("put block");

        svc.mempool
            .write()
            .await
            .restore_state(
                bincode::serde::encode_to_vec(
                    MemPoolSyncData {
                        transactions: vec![tx],
                        timestamp: 0,
                    },
                    standard(),
                )
                .expect("encode mempool sync state"),
            )
            .await
            .expect("mempool restore");

        let response = get_tx::<StryiStorage>(State(svc), Path(tx_hash.to_string()))
            .await
            .expect("confirmed tx response");

        assert_eq!(response.0.status, TransactionQueryStatus::Confirmed);
        assert_eq!(response.0.block_hash, Some(block.block_hash()));
    }

    #[tokio::test]
    async fn query_unknown_transaction_returns_not_found() {
        let svc = make_service().await;
        let err = get_tx::<StryiStorage>(
            State(svc),
            Path(TransactionHash::new(&[9u8; 32]).to_string()),
        )
        .await
        .expect_err("missing tx");

        assert!(matches!(
            err,
            StryiNodeHttpApiError::ResourceNotFound {
                resource: ResourceKind::Transaction(_)
            }
        ));
    }
}
