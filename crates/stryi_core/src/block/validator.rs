use crate::error::StryiCoreError;
use crate::storage::UtxoStorage;
use crate::transactions::{Transaction, TransactionKind};
use crate::block::Block;
use crate::address::AccountAddress;
use crate::block::mining::meets_difficulty;

/// BlockValidator is responsible for validating blocks against consensus rules.
pub struct BlockValidator {
    /// Current consensus difficulty bits.
    pub current_difficulty: u8,
}

impl BlockValidator {
    /// Creates a new BlockValidator with the specified difficulty.
    pub fn new(current_difficulty: u8) -> Self {
        Self { current_difficulty }
    }

    /// Validates an entire block.
    ///
    /// # Parameters
    ///
    /// - `block`: Reference to the block to be validated.
    /// - `utxo_storage`: Mutable reference to the UTXO storage.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the block is valid.
    /// - `Err(StryiCoreError)` if the block is invalid.
    pub async fn validate_block<S: UtxoStorage>(
        &self,
        block: &Block,
        utxo_storage: &mut S,
    ) -> Result<(), StryiCoreError> {
        // 1. Difficulty Verification
        self.verify_difficulty(block)?;
        
        // 2. Proof-of-Work Verification
        self.verify_proof_of_work(block)?;

        // 3. Coinbase Transaction Verification
        self.verify_coinbase_transaction(block)?;

        // 4. Merkle Root Verification
        self.verify_merkle_root(block)?;

        // 5. Transaction Order and Content Verification
        self.verify_transactions(block, utxo_storage).await?;

        Ok(())
    }

    /// Verifies that the block's difficulty bits match the current consensus rules.
    fn verify_difficulty(&self, block: &Block) -> Result<(), StryiCoreError> {
        if block.header.difficulty_bits != self.current_difficulty {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: format!(
                    "Block difficulty ({}) does not match current difficulty ({})",
                    block.header.difficulty_bits, self.current_difficulty
                ),
            });
        }
        Ok(())
    }

    /// Verifies that the block's hash meets the Proof-of-Work difficulty.
    fn verify_proof_of_work(&self, block: &Block) -> Result<(), StryiCoreError> {
        
        
        // Genesis block does not require this check, so skip
        if block.header.is_genesis {
            return Ok(())
        }
        
        let block_hash = block.block_hash();
        if !meets_difficulty(&block_hash, block.header.difficulty_bits) {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Block does not meet required Proof-of-Work difficulty".to_string(),
            });
        }
        Ok(())
    }

    /// Ensures that the first transaction is a Coinbase transaction in non-genesis blocks.
    fn verify_coinbase_transaction(&self, block: &Block) -> Result<(), StryiCoreError> {
        if !block.header.is_genesis {
            if let Some(first_tx) = block.data.transactions.first() {
                if first_tx.data.kind != TransactionKind::Coinbase {
                    return Err(StryiCoreError::ConsensusValidationFailed {
                        details: "First transaction must be a Coinbase transaction".to_string(),
                    });
                }
            } else {
                return Err(StryiCoreError::ConsensusValidationFailed {
                    details: "Block contains no transactions".to_string(),
                });
            }
        }
        Ok(())
    }

    /// Validates the Merkle root in the block header.
    fn verify_merkle_root(&self, block: &Block) -> Result<(), StryiCoreError> {
        let computed_merkle_root = Block::compute_merkle_root(&block.data.transactions);
        if block.header.merkle_root_hash != computed_merkle_root {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Merkle root mismatch".to_string(),
            });
        }
        Ok(())
    }
    
    
    /// Verifies all transactions within the block. 
    async fn verify_transactions<S: UtxoStorage>(
        &self,
        block: &Block,
        utxo_storage: &mut S,
    ) -> Result<(), StryiCoreError> {
        for (tx_index, tx) in block.data.transactions.iter().enumerate() {
            match tx.data.kind {
                TransactionKind::Genesis => {
                    // Genesis transactions are only valid in genesis blocks
                    if block.header.height != 0 {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: "Genesis transaction found in non-genesis block".to_string(),
                        });
                    }

                    // Genesis transactions should have no inputs
                    if !tx.data.inputs.is_empty() {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: "Genesis transaction must not have any inputs".to_string(),
                        });
                    }
                }
                TransactionKind::Coinbase => {
                    // Coinbase transactions must be the first transaction in non-genesis blocks
                    if !block.header.is_genesis && tx_index != 0 {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: format!(
                                "Coinbase transaction must be the first transaction, found at index {}",
                                tx_index
                            ),
                        });
                    }

                    // Coinbase transactions should have no inputs
                    if !tx.data.inputs.is_empty() {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: "Coinbase transaction should not have any inputs".to_string(),
                        });
                    }

                    // Coinbase transactions must have exactly one output (miner reward)
                    if tx.data.outputs.len() != 1 {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: "Coinbase transaction must have exactly one output".to_string(),
                        });
                    }
                }
                TransactionKind::Payment => {
                    // For Payment transactions, perform signature and ownership checks
                    self.validate_payment_transaction(tx, utxo_storage).await?;
                }
            }
        }
        Ok(())
    }

    /// Validates a Payment transaction's signatures, ownership, and input/output sums.
    async fn validate_payment_transaction<S: UtxoStorage>(
        &self,
        tx: &Transaction,
        utxo_storage: &mut S,
    ) -> Result<(), StryiCoreError> {
        // 1. Recover the public key from the transaction's signature
        let author_key = tx.recover_public_key().map_err(|_| {
            StryiCoreError::ConsensusValidationFailed {
                details: "Failed to recover public key from transaction signature".to_string(),
            }
        })?;

        // 2. Verify the transaction's signature
        tx.verify_signature(&author_key).map_err(|_| {
            StryiCoreError::ConsensusValidationFailed {
                details: "Invalid transaction signature".to_string(),
            }
        })?;

        // 3. Derive the author's address from the public key
        let author_address = AccountAddress::from_public_key(&author_key);

        // 4. Calculate the sum of input values and verify ownership
        let mut input_sum: u64 = 0;
        for input in &tx.data.inputs {
            let utxo = utxo_storage.get_utxo(&input.previous_output).await.map_err(|_| {
                StryiCoreError::TxMissingUtxo {
                    txid: input.previous_output.txid,
                    vout: input.previous_output.vout,
                }
            })?;

            // Verify that the UTXO belongs to the transaction's author
            if utxo.owner != author_address {
                return Err(StryiCoreError::TxWrongOwner {
                    expected: utxo.owner,
                    actual: author_address,
                });
            }

            // Accumulate the input sums, checking for overflow
            input_sum = input_sum.checked_add(utxo.value).ok_or(StryiCoreError::ConsensusValidationFailed {
                details: "Overflow occurred while summing transaction inputs".to_string(),
            })?;
        }

        // 5. Calculate the sum of output values
        let output_sum: u64 = tx.data.outputs.iter().map(|o| o.value).sum();

        // 6. Ensure that the sum of outputs does not exceed the sum of inputs
        if output_sum > input_sum {
            return Err(StryiCoreError::TxInsufficientInputValue {
                input_sum,
                output_sum,
            });
        }

        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::in_memory_utxo::InMemoryUtxoStorage;
    use crate::transactions::{Transaction, TransactionData, TransactionKind, TransactionOut, TransactionIn, OutPoint, UTXO, StryiSignature};
    use crate::address::AccountAddress;
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::rand_core::OsRng;
    use std::collections::HashMap;
    use crate::block::mining::mine_block_in_parallel;

    #[tokio::test]
    async fn test_block_validator() {
        // 1. Initialize consensus difficulty
        let current_difficulty = 2; // require 2 leading zero bits
        let block_validator = BlockValidator::new(current_difficulty);

        // 2. Initialize UTXO storage
        let mut utxo_storage = InMemoryUtxoStorage::new();

        // 3. Create signing keys for two accounts
        let sk_genesis = SigningKey::random(&mut OsRng);
        let vk_genesis = sk_genesis.verifying_key();
        let addr_genesis = AccountAddress::from_public_key(&vk_genesis);

        let sk_alice = SigningKey::random(&mut OsRng);
        let vk_alice = sk_alice.verifying_key();
        let addr_alice = AccountAddress::from_public_key(&vk_alice);

        // 4. Create a genesis block with addr_genesis having 1000 coins
        let mut balances = HashMap::new();
        balances.insert(addr_genesis.clone(), 1000);

        let genesis_block = Block::new_genesis(1, current_difficulty, balances);

        
        // Any genesis block can contain only 1 transaction
        assert_eq!(genesis_block.data.transactions.len(), 1);
        
        
        // 5. Validate the genesis block
        let result = block_validator.validate_block(&genesis_block, &mut utxo_storage).await;
        assert!(result.is_ok(), "Genesis block should be valid");

        // 6. Update UTXO storage with genesis transactions
        for tx in &genesis_block.data.transactions {
            for (vout, output) in tx.data.outputs.iter().enumerate() {
                let out_point = OutPoint {
                    txid: tx.data.hash(),
                    vout: vout as u32,
                };
                let utxo = UTXO {
                    txid: out_point.txid.clone(),
                    vout: out_point.vout,
                    value: output.value,
                    owner: output.recipient.clone(),
                };
                utxo_storage.put_utxo(&out_point, utxo).await.unwrap();
            }
        }

        // 7. Create a payment transaction from addr_genesis to addr_alice
        // For this, need to spend one UTXO from genesis
        let genesis_tx = &genesis_block.data.transactions[0];
        let genesis_out_point = OutPoint {
            txid: genesis_tx.data.hash(),
            vout: 0,
        };

        // Create transaction inputs and outputs
        let tx_data = TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![
                TransactionIn {
                    previous_output: genesis_out_point.clone(),
                    sequence: 0xFFFFFFFF,
                }
            ],
            outputs: vec![
                TransactionOut {
                    value: 600,
                    recipient: addr_alice.clone(),
                },
                TransactionOut {
                    value: 400,
                    recipient: addr_genesis.clone(),
                },
            ],
        };

        // 8. Sign the transaction using the existing sign_transaction function
        let payment_tx = tx_data.sign(&sk_genesis);

        // 9. Create a coinbase transaction (no signature required)
        let coinbase_tx_data = TransactionData {
            version: 1,
            kind: TransactionKind::Coinbase,
            inputs: vec![],
            outputs: vec![
                TransactionOut {
                    value: 50, // Miner reward
                    recipient: addr_genesis.clone(),
                }
            ],
        };

        let coinbase_tx = Transaction {
            data: coinbase_tx_data,
            signature: StryiSignature(Box::new([0u8; 65])), // Dummy signature since it's not required
        };

        // 10. Assemble the new block's transactions: coinbase first, then payment
        let new_block_transactions = vec![coinbase_tx, payment_tx.clone()];

        // 11. Create the new block
        let previous_block_hash = genesis_block.block_hash();
        let mut new_block = Block::new(new_block_transactions, previous_block_hash, 1, current_difficulty, 1_700_000_001, 1);
        mine_block_in_parallel(&mut new_block, u64::MAX); // Mine to prevent PoW validation error
        
        // 12. Validate the new block
        let validation_result = block_validator.validate_block(&new_block, &mut utxo_storage).await;
        assert!(validation_result.is_ok(), "New block with valid payment should be valid");

        // 13. Update UTXO storage with new block's transactions
        for tx in &new_block.data.transactions {
            for (vout, output) in tx.data.outputs.iter().enumerate() {
                let out_point = OutPoint {
                    txid: tx.data.hash(),
                    vout: vout as u32,
                };
                let utxo = UTXO {
                    txid: out_point.txid.clone(),
                    vout: out_point.vout,
                    value: output.value,
                    owner: output.recipient.clone(),
                };
                utxo_storage.put_utxo(&out_point, utxo).await.unwrap();
            }
            // Remove spent UTXOs for payment transactions
            if tx.data.kind == TransactionKind::Payment {
                for input in &tx.data.inputs {
                    utxo_storage.remove_utxo(&input.previous_output).await.unwrap();
                }
            }
        }

        // 14. Now, test an invalid block: wrong difficulty
        let invalid_block = Block::new(new_block.data.transactions.clone(), new_block.block_hash(), new_block.header.height + 1, current_difficulty + 1, 1_700_000_002, 1);

        let invalid_result = block_validator.validate_block(&invalid_block, &mut utxo_storage).await;
        assert!(invalid_result.is_err(), "Block with incorrect difficulty should be invalid");
    }
}
