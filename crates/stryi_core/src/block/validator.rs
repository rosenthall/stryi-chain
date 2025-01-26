use crate::address::AccountAddress;
use crate::block::{Block, BlockData, meets_difficulty};
use crate::dependencies::DependencyGraph;
use crate::error::StryiCoreError;
use crate::storage::UtxoStorage;
use crate::transactions::{
    OutPoint, Transaction, TransactionKind, TransactionData, UTXO,
};
use dashmap::{DashMap, DashSet};
use std::sync::Arc;
use tokio::task::JoinSet;

/// BlockValidator is responsible for validating blocks against consensus rules.
#[derive(Clone)]
pub struct BlockValidator {
    /// Current consensus difficulty bits.
    pub current_difficulty: u8,
}

impl BlockValidator {
    /// Creates a new BlockValidator with the specified difficulty.
    pub fn new(current_difficulty: u8) -> Self {
        Self { current_difficulty }
    }

    /// Validates an entire block by sequentially executing all validation steps.
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
        
        // Verify if difficulty in header matches with current difficulty of `BlockValidator`
        self.verify_difficulty(block)?;
        // Verify if block actually can be hashed to get provided hash
        self.verify_proof_of_work(block)?;
        // Check if coinbase transaction is first in the block. TODO: Somehow make reward system actually work
        self.verify_coinbase_transaction(block)?;
        // Verify consistency of provided transaction and merkle root in header
        self.verify_merkle_root(block)?;

        
        // Construct dependency graph from all the required UTXO's in block, including current block's and references to older ones
        let (dep_graph, managed_utxos) = self
            .validate_dependencies(&block.data, utxo_storage)
            .await?;

        
        // Verify transactions
        self.verify_transactions(&block.data, &dep_graph, &managed_utxos).await?;

        Ok(())
    }

    /// Verifies that the block's difficulty bits match the current consensus rules.
    ///
    /// # Parameters
    ///
    /// - `block`: Reference to the block to be validated.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the difficulty matches.
    /// - `Err(StryiCoreError)` if the difficulty does not match.
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
    ///
    /// # Parameters
    ///
    /// - `block`: Reference to the block to be validated.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the block meets the difficulty.
    /// - `Err(StryiCoreError)` if the block does not meet the difficulty.
    fn verify_proof_of_work(&self, block: &Block) -> Result<(), StryiCoreError> {
        // Genesis block does not require this check, so skip
        if block.header.is_genesis {
            return Ok(());
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
    ///
    /// # Parameters
    ///
    /// - `block`: Reference to the block to be validated.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the Coinbase transaction is correctly placed.
    /// - `Err(StryiCoreError)` if the Coinbase transaction is missing or incorrectly placed.
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
    ///
    /// # Parameters
    ///
    /// - `block`: Reference to the block to be validated.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the Merkle root matches.
    /// - `Err(StryiCoreError)` if the Merkle root does not match.
    fn verify_merkle_root(&self, block: &Block) -> Result<(), StryiCoreError> {
        let computed_merkle_root = Block::compute_merkle_root(&block.data.transactions);
        if block.header.merkle_root_hash != computed_merkle_root {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Merkle root mismatch".to_string(),
            });
        }
        Ok(())
    }

    /// Validates transaction dependencies and aggregates required external UTXOs.
    ///
    /// # Returns
    ///
    /// - A tuple containing the validated `DependencyGraph` and an `Arc<DashMap>` of managed UTXOs.
    pub(crate) async fn validate_dependencies<S: UtxoStorage>(
        &self,
        block_data: &BlockData,
        utxo_storage: &mut S,
    ) -> Result<(DependencyGraph, Arc<DashMap<OutPoint, UTXO>>), StryiCoreError> {
        // Build dependency graph
        let mut dep_graph = DependencyGraph::build(block_data)?;

        // Validate transaction ordering and check for cyclic dependencies
        dep_graph.validate_order(block_data)?;

        // Identify external dependencies (UTXOs from previous blocks)
        let external_deps = dep_graph.get_external_dependencies();
        let managed_utxos = Arc::new(DashMap::<OutPoint, UTXO>::new());

        if !external_deps.is_empty() {
            // Fetch all external UTXOs in a single operation to minimize storage accesses
            let utxos_result = utxo_storage.get_utxos(&external_deps).await;

            match utxos_result {
                Ok(existing_utxos) => {
                    for (outpoint, utxo) in existing_utxos {
                        managed_utxos.insert(outpoint.clone(), utxo);
                    }
                }
                Err(_) => {
                    // Handle storage error by reporting the first missing UTXO
                    if let Some(first_missing) = external_deps.iter().next() {
                        return Err(StryiCoreError::TxMissingUtxo {
                            txid: first_missing.txid.clone(),
                            vout: first_missing.vout,
                        });
                    }
                }
            }
        }

        Ok((dep_graph, managed_utxos))
    }

    /// Verifies all transactions within the block using parallel execution groups.
    /// Utilizes Tokio's JoinSet and DashMap for managing concurrent transaction validations with improved lock scalability.
    ///
    /// # Parameters
    ///
    /// - `block_data`: Reference to the block's data, which includes all transactions.
    /// - `dep_graph`: Reference to the dependency graph of transactions.
    /// - `managed_utxos`: Reference to the managed UTXOs.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if all transactions are validated successfully.
    /// - `Err(StryiCoreError)` if any transaction fails validation.
    async fn verify_transactions(
        &self,
        block_data: &BlockData,
        dep_graph: &DependencyGraph,
        managed_utxos: &Arc<DashMap<OutPoint, UTXO>>,
    ) -> Result<(), StryiCoreError> {
        let parallel_groups = dep_graph.get_parallel_execution_groups();

        // Shared mutable state for internal UTXOs and spent UTXOs using DashMap and DashSet
        let internal_utxos = Arc::new(DashMap::<OutPoint, UTXO>::new());
        let spent_utxos = Arc::new(DashSet::<OutPoint>::new());

        for group in parallel_groups {
            // Validate the current group of transactions
            self.validate_group(
                &group,
                block_data,
                managed_utxos,
                Arc::clone(&internal_utxos),
                Arc::clone(&spent_utxos),
            )
                .await?;

            // Update state after successful validation
            self.update_state(&group, block_data, &internal_utxos, &spent_utxos).await;
        }

        Ok(())
    }

    /// Validates a single parallel execution group of transactions.
    ///
    /// # Parameters
    ///
    /// - `group`: A vector containing the indices of transactions within the group to be validated.
    /// - `block_data`: A reference to the block's data, which includes all transactions.
    /// - `managed_utxos`: An `Arc`-wrapped `DashMap` containing managed UTXOs required for validation.
    /// - `internal_utxos`: An `Arc`-wrapped `DashMap` representing internal UTXOs created within the current block.
    /// - `spent_utxos`: An `Arc`-wrapped `DashSet` tracking UTXOs that have been spent within the current block.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if all transactions in the group are validated successfully.
    /// - `Err(StryiCoreError)` if any transaction fails validation or if a task panics.
    ///
    /// # Description
    ///
    /// This method validates a group of transactions concurrently. It performs the following steps:
    ///
    /// 1. **Task Spawning**:
    ///     - For each transaction index in the group, spawn a separate asynchronous task to validate the transaction.
    ///     - Each task executes the `validate_transaction` method and returns a `Result<(), StryiCoreError>`.
    ///
    /// 2. **Task Monitoring**:
    ///     - Utilize a `JoinSet` to concurrently await the completion of all spawned tasks.
    ///     - As tasks complete, monitor their results:
    ///         - If a task succeeds (`Ok(())`), continue.
    ///         - If a task fails (`Err(StryiCoreError)`), abort all remaining tasks in the group and capture the error.
    ///         - If a task panics or is aborted, capture the panic and convert it into a `ConsensusValidationFailed` error.
    ///
    /// 3. **Early Termination**:
    ///     - Upon encountering the first validation failure or panic, abort all other ongoing tasks within the group to prevent unnecessary computations.
    ///
    /// 4. **Error Reporting**:
    ///     - Return the first encountered error.
    ///     - If all tasks succeed, return `Ok(())` indicating successful validation of the group.
    ///
    /// # Errors
    ///
    /// - Returns `StryiCoreError::ConsensusValidationFailed` if any transaction within the group fails validation.
    /// - Returns `StryiCoreError::ConsensusValidationFailed` if a task panics during execution.
    async fn validate_group(
        &self,
        group: &Vec<usize>,
        block_data: &BlockData,
        managed_utxos: &Arc<DashMap<OutPoint, UTXO>>,
        internal_utxos: Arc<DashMap<OutPoint, UTXO>>,
        spent_utxos: Arc<DashSet<OutPoint>>,
    ) -> Result<(), StryiCoreError> {
        // Initialize a JoinSet to manage multiple concurrent tasks
        let mut join_set = JoinSet::new();

        // Spawn a task for each transaction in the group
        for &tx_idx in group {
            // Clone the necessary data for the spawned task
            let tx = block_data.transactions[tx_idx].clone();
            let managed_utxos = Arc::clone(managed_utxos);
            let internal_utxos = Arc::clone(&internal_utxos);
            let spent_utxos = Arc::clone(&spent_utxos);
            let validator = self.clone();

            // Spawn the transaction validation task
            join_set.spawn(async move {
                // Execute the transaction validation
                validator
                    .validate_transaction(&tx, &managed_utxos, &internal_utxos, &spent_utxos)
                    .await
            });
        }

        // Iterate over tasks as they complete
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(Ok(())) => {
                    // Transaction validated successfully; continue
                }
                Ok(Err(e)) => {
                    // Transaction validation failed; abort remaining tasks
                    join_set.abort_all();
                    return Err(e);
                }
                Err(e) => {
                    // Task encountered an error during execution (e.g., panic)
                    join_set.abort_all();
                    return Err(StryiCoreError::ConsensusValidationFailed {
                        details: format!("Task join error: {:?}", e),
                    });
                }
            }
        }

        // All transactions validated successfully
        Ok(())
    }

    /// Updates the internal UTXOs and spent UTXOs after successful validation of a group.
    ///
    /// # Parameters
    ///
    /// - `group`: Vector of transaction indices in the group.
    /// - `block_data`: Reference to the block's data.
    /// - `internal_utxos`: Shared reference to internal UTXOs.
    /// - `spent_utxos`: Shared reference to spent UTXOs.
    async fn update_state(
        &self,
        group: &Vec<usize>,
        block_data: &BlockData,
        internal_utxos: &Arc<DashMap<OutPoint, UTXO>>,
        spent_utxos: &Arc<DashSet<OutPoint>>,
    ) {
        for &tx_idx in group {
            let tx = &block_data.transactions[tx_idx];

            // Add new UTXOs from transaction outputs
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
                internal_utxos.insert(out_point, utxo);
            }

            // Mark inputs as spent
            if tx.data.kind == TransactionKind::Payment {
                for input in &tx.data.inputs {
                    spent_utxos.insert(input.previous_output.clone());
                }
            }
        }
    }

    /// Validates a single transaction using aggregated UTXOs.
    ///
    /// # Parameters
    ///
    /// - `tx`: Reference to the transaction to validate.
    /// - `managed_utxos`: Reference to the managed UTXOs.
    /// - `internal_utxos`: Reference to internal UTXOs.
    /// - `spent_utxos`: Reference to spent UTXOs.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the transaction is valid.
    /// - `Err(StryiCoreError)` if the transaction is invalid.
    async fn validate_transaction(
        &self,
        tx: &Transaction,
        managed_utxos: &DashMap<OutPoint, UTXO>,
        internal_utxos: &DashMap<OutPoint, UTXO>,
        spent_utxos: &DashSet<OutPoint>,
    ) -> Result<(), StryiCoreError> {
        match tx.data.kind {
            TransactionKind::Genesis => self.validate_genesis_transaction(tx),
            TransactionKind::Coinbase => self.validate_coinbase_transaction(tx),
            TransactionKind::Payment => {
                self.validate_payment_transaction(tx, managed_utxos, internal_utxos, spent_utxos)
                    .await
            }
        }
    }

    /// Validates a Genesis transaction.
    ///
    /// # Parameters
    ///
    /// - `tx`: Reference to the Genesis transaction.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if valid.
    /// - `Err(StryiCoreError)` if invalid.
    fn validate_genesis_transaction(&self, tx: &Transaction) -> Result<(), StryiCoreError> {
        if tx.data.inputs.is_empty() && !tx.data.outputs.is_empty() {
            Ok(())
        } else {
            Err(StryiCoreError::ConsensusValidationFailed {
                details: "Invalid Genesis transaction structure".to_string(),
            })
        }
    }

    /// Validates a Coinbase transaction.
    ///
    /// # Parameters
    ///
    /// - `tx`: Reference to the Coinbase transaction.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if valid.
    /// - `Err(StryiCoreError)` if invalid.
    fn validate_coinbase_transaction(&self, tx: &Transaction) -> Result<(), StryiCoreError> {
        if tx.data.inputs.is_empty() && tx.data.outputs.len() == 1 {
            Ok(())
        } else {
            Err(StryiCoreError::ConsensusValidationFailed {
                details: "Invalid Coinbase transaction structure".to_string(),
            })
        }
    }

    /// Validates a Payment transaction's signatures, ownership, and input/output sums using aggregated UTXOs.
    ///
    /// # Parameters
    ///
    /// - `tx`: Reference to the Payment transaction.
    /// - `managed_utxos`: Reference to the managed UTXOs.
    /// - `internal_utxos`: Reference to internal UTXOs.
    /// - `spent_utxos`: Reference to spent UTXOs.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the transaction is valid.
    /// - `Err(StryiCoreError)` if the transaction is invalid.
    async fn validate_payment_transaction(
        &self,
        tx: &Transaction,
        managed_utxos: &DashMap<OutPoint, UTXO>,
        internal_utxos: &DashMap<OutPoint, UTXO>,
        spent_utxos: &DashSet<OutPoint>,
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

        // 4. Calculate the sum of input values and verify ownership with explicit overflow handling
        let mut input_sum: u64 = 0;
        for input in &tx.data.inputs {
            // Check if the UTXO is already spent in the current block
            if spent_utxos.contains(&input.previous_output) {
                return Err(StryiCoreError::TxDoubleSpend {
                    txid: input.previous_output.txid.clone(),
                    vout: input.previous_output.vout,
                });
            }

            // Retrieve UTXO from managed_utxos
            let utxo = managed_utxos.get(&input.previous_output).ok_or(
                StryiCoreError::TxMissingUtxo {
                    txid: input.previous_output.txid.clone(),
                    vout: input.previous_output.vout,
                },
            )?;

            // Verify that the UTXO belongs to the transaction's author
            if utxo.owner != author_address {
                return Err(StryiCoreError::TxWrongOwner {
                    expected: utxo.owner.clone(),
                    actual: author_address.clone(),
                });
            }

            // Accumulate the input sums with explicit overflow handling
            input_sum = input_sum.checked_add(utxo.value).ok_or(
                StryiCoreError::ConsensusValidationFailed {
                    details: "Overflow occurred while summing transaction inputs".to_string(),
                },
            )?;
        }

        // 5. Calculate the sum of output values with explicit overflow handling
        let output_sum: u64 = tx
            .data
            .outputs
            .iter()
            .try_fold(0u64, |acc, o| acc.checked_add(o.value))
            .ok_or(StryiCoreError::ConsensusValidationFailed {
                details: "Overflow occurred while summing transaction outputs".to_string(),
            })?;

        // 6. Ensure that the sum of outputs exactly equals the sum of inputs
        if output_sum != input_sum {
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
    use crate::block::mining::mine_block_in_parallel;
    use crate::storage::in_memory_utxo::InMemoryUtxoStorage;
    use crate::transactions::{
        OutPoint, StryiSignature, Transaction, TransactionData, TransactionIn, TransactionKind,
        TransactionOut, UTXO,
    };
    use crate::address::AccountAddress;
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::rand_core::OsRng;
    use std::collections::HashMap;

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
            inputs: vec![TransactionIn {
                previous_output: genesis_out_point.clone(),
                sequence: 0xFFFFFFFF,
            }],
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
            outputs: vec![TransactionOut {
                value: 50, // Miner reward
                recipient: addr_genesis.clone(),
            }],
        };

        let coinbase_tx = Transaction {
            data: coinbase_tx_data,
            signature: StryiSignature(Box::new([0u8; 65])), // Dummy signature since it's not required
        };

        // 10. Assemble the new block's transactions: coinbase first, then payment
        let new_block_transactions = vec![coinbase_tx, payment_tx.clone()];

        // 11. Create the new block
        let previous_block_hash = genesis_block.block_hash();
        let mut new_block = Block::new(
            new_block_transactions,
            previous_block_hash,
            1,
            current_difficulty,
            1_700_000_001,
            1,
        );
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
        let invalid_block = Block::new(
            new_block.data.transactions.clone(),
            new_block.block_hash(),
            new_block.header.height + 1,
            current_difficulty + 1, // Incorrect difficulty
            1_700_000_002,
            1,
        );

        let invalid_result = block_validator.validate_block(&invalid_block, &mut utxo_storage).await;
        assert!(
            invalid_result.is_err(),
            "Block with incorrect difficulty should be invalid"
        );
    }
}
