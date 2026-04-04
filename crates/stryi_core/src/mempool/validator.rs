use crate::address::AccountAddress;
use crate::mempool::UtxoLookup;
use crate::mempool::storage::TransactionStorage;
use crate::transactions::{OutPoint, Transaction, TransactionKind, UTXO};
use thiserror::Error;

/// Errors that can arise during mempool validation of transactions.
#[derive(Debug, Error)]
pub enum MempoolValidationError {
    #[error("Missing UTXO for outpoint: {0:?}")]
    MissingOutPoint(OutPoint),

    #[error("Input sum ({inputs}) is smaller than output sum ({outputs})")]
    InsufficientSum { inputs: u64, outputs: u64 },

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

    #[error(
        "Mempool works only for Payment transactions, not Coinbase/Genesis, transaction hash: {0}"
    )]
    NonPaymentTx(String),
}

/// Validates transactions before they enter the mempool.
pub struct MempoolTxValidator {
    utxo_lookup: UtxoLookup,
}

impl MempoolTxValidator {
    /// Creates a validator with an async UTXO lookup.
    pub fn new(utxo_lookup: UtxoLookup) -> Self {
        Self { utxo_lookup }
    }

    /// Validates a transaction and returns the resolved input UTXOs.
    pub async fn validate(
        &self,
        tx: &Transaction,
        storage: &TransactionStorage,
    ) -> Result<Vec<UTXO>, MempoolValidationError> {
        // Check if the transaction kind is not payment
        if !matches!(tx.data.kind, TransactionKind::Payment) {
            return Err(MempoolValidationError::NonPaymentTx(
                tx.data.hash().to_string(),
            ));
        }

        // Payment transaction => check signature by recovering public key
        let rec_key = match tx.recover_public_key() {
            Ok(k) => k,
            Err(e) => return Err(MempoolValidationError::SignatureFailed(e.to_string())),
        };
        let recovered_addr = AccountAddress::from_public_key(&rec_key);

        // Gather input UTXOs
        let mut total_input_value = 0u64; // accumulator value
        let mut utxos = Vec::with_capacity(tx.data.inputs.len());
        let mut potential_rbf = false;

        for (i, input) in tx.data.inputs.iter().enumerate() {
            let outpoint = &input.previous_output;

            if let Some(spending_tx_hash) = storage.get_spending_tx(outpoint)
                && spending_tx_hash != &tx.data.hash()
            {
                potential_rbf = true;
            }

            let utxo = if let Some(tx_hash) = storage.get_creating_tx(outpoint) {
                if let Some(mem_tx) = storage.get(tx_hash) {
                    if outpoint.vout < mem_tx.transaction.data.outputs.len() as u32 {
                        let output = &mem_tx.transaction.data.outputs[outpoint.vout as usize];
                        UTXO {
                            txid: outpoint.txid,
                            vout: outpoint.vout,
                            value: output.value,
                            owner: output.recipient,
                        }
                    } else {
                        return Err(MempoolValidationError::InvalidOutpointIndex(*outpoint));
                    }
                } else {
                    return Err(MempoolValidationError::MissingOutPoint(*outpoint));
                }
            } else {
                // Try external UTXO lookup
                let utxo_lookup = &self.utxo_lookup;
                utxo_lookup(outpoint)
                    .await
                    .ok_or(MempoolValidationError::MissingOutPoint(*outpoint))?
            };

            // Check ownership
            if utxo.owner != recovered_addr {
                return Err(MempoolValidationError::OwnershipMismatch {
                    input_index: i,
                    expected: recovered_addr,
                    actual: utxo.owner,
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

        if potential_rbf {
            return Err(MempoolValidationError::PotentialRbf(utxos));
        }

        Ok(utxos)
    }
}
