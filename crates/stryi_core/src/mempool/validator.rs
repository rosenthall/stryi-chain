use thiserror::Error;
use crate::address::AccountAddress;
use crate::mempool::storage::TransactionStorage;
use crate::mempool::UtxoLookup;
use crate::transactions::{Transaction, TransactionKind, OutPoint, UTXO, TransactionHash};

/// Errors that can arise during mempool validation of transactions.
#[derive(Debug, Error)]
pub enum MempoolValidationError {
    #[error("Missing UTXO for outpoint: {0:?}")]
    MissingOutPoint(OutPoint),

    #[error("Input sum ({inputs}) is smaller than output sum ({outputs})")]
    InsufficientSum {
        inputs: u64,
        outputs: u64,
    },

    #[error("Signature verification failed: {0}")]
    SignatureFailed(String),

    #[error("Ownership mismatch on input {input_index}: expected {expected}, got {actual}")]
    OwnershipMismatch {
        input_index: usize,
        expected: AccountAddress,
        actual: AccountAddress,
    },
    
    #[error("Invalid outpoint index: {0:?}")]
    InvalidOutpointIndex(OutPoint),

    #[error("Potential RBF transaction")]
    PotentialRbf(Vec<UTXO>),

    #[error("Outpoint {0:?} is already spent by another transaction in mempool")]
    AlreadySpent(OutPoint),

    #[error("Mempool works only for Payment transactions, not Coinbase/Genesis, transaction hash: {0}")]
    NonPaymentTx(String)
}

/// MempoolTxValidator validates any transaction so we can add it mempool without risks or inconsistencies.
/// Recovers the public key (verifying ECDSA signature).
/// Then checks each input's UTXO.owner matches the recovered address.
/// Also ensures total inputs >= total outputs and detects double-spends. 
pub struct MempoolTxValidator {
    utxo_lookup: UtxoLookup,
}

impl MempoolTxValidator {
    /// Creates a validator with a user-provided async function that fetches UTXOs from some hypothetical storage.
    pub fn new(utxo_lookup: UtxoLookup) -> Self {
        Self { utxo_lookup }
    }

    /// Validates a transaction inputs.
    /// Returns error if transaction kind is not payment, signature is invalid,
    /// inputs are insufficient, or if double-spends are detected.
    // In validator.rs, modify the validate method

    pub async fn validate(
        &self,
        tx: &Transaction,
        storage: &TransactionStorage
    ) -> Result<Vec<UTXO>, MempoolValidationError> {
        // Check if transaction kind is not payment
        if !matches!(tx.data.kind, TransactionKind::Payment) {
            return Err(MempoolValidationError::NonPaymentTx(tx.data.hash().to_string()))
        }

        // Payment transaction => check signature by recovering public key
        let rec_key = match tx.recover_public_key() {
            Ok(k) => k,
            Err(e) => return Err(MempoolValidationError::SignatureFailed(e.to_string())),
        };

        // Derive address from the verifying key
        let recovered_addr = AccountAddress::from_public_key(&rec_key);

        // Gather input UTXOs
        let mut total_input_value = 0u64; // accumulator value
        let mut utxos = Vec::with_capacity(tx.data.inputs.len());
        let mut potential_rbf = false;

        for (i, input) in tx.data.inputs.iter().enumerate() {
            let outpoint = &input.previous_output;

            // Check if the outpoint is already spent by another transaction
            if let Some(spending_tx_hash) = storage.get_spending_tx(outpoint) {
                // If this outpoint is spent by another transaction (not the current one)
                if spending_tx_hash != &tx.data.hash() {
                    // Mark as potential RBF instead of failing immediately
                    potential_rbf = true;
                    // Continue with validation to collect all potential conflicts
                }
            }

            // Try to find the UTXO either in mempool or external storage
            let utxo = if let Some(tx_hash) = storage.get_creating_tx(outpoint) {
                // UTXO is from a transaction in mempool
                if let Some(mem_tx) = storage.get(tx_hash) {
                    if outpoint.vout < mem_tx.transaction.data.outputs.len() as u32 {
                        let output = &mem_tx.transaction.data.outputs[outpoint.vout as usize];
                        UTXO {
                            txid: outpoint.txid.clone(),
                            vout: outpoint.vout,
                            value: output.value,
                            owner: output.recipient.clone(),
                        }
                    } else {
                        return Err(MempoolValidationError::InvalidOutpointIndex(outpoint.clone()));
                    }
                } else {
                    return Err(MempoolValidationError::MissingOutPoint(outpoint.clone()));
                }
            } else {
                // Try external UTXO lookup
                let utxo_lookup = &self.utxo_lookup;
                utxo_lookup(outpoint).await.ok_or_else(|| {
                    MempoolValidationError::MissingOutPoint(outpoint.clone())
                })?
            };

            // Check ownership
            if utxo.owner != recovered_addr {
                return Err(MempoolValidationError::OwnershipMismatch {
                    input_index: i,
                    expected: recovered_addr.clone(),
                    actual: utxo.owner.clone(),
                });
            }

            total_input_value = total_input_value.saturating_add(utxo.value);
            utxos.push(utxo);
        }

        // Check total_input_value >= sum of outputs
        let outputs_sum: u64 = tx.data.outputs.iter().map(|o| o.value).sum();
        if total_input_value < outputs_sum {
            return Err(MempoolValidationError::InsufficientSum {
                inputs: total_input_value,
                outputs: outputs_sum,
            });
        }

        // If we detected potential RBF but there were other validation errors, we would have returned early
        // So if we get here and potential_rbf is true, we'll return a special result
        if potential_rbf {
            return Err(MempoolValidationError::PotentialRbf(utxos));
        }

        // If all checks pass, we return the input UTXOs
        Ok(utxos)
    }
}