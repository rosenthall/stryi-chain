use crate::{
    block::{meets_difficulty, Block},
    consensus::{ConsensusEngine, ConsensusRules},
    error::StryiCoreError,
    storage::in_memory_utxo::InMemoryUtxoStorage,
    storage::UtxoStorage,
    address::AccountAddress,
    transactions::TransactionKind,
};

pub struct StryiConsensusEngine {
    pub rules: ConsensusRules,
}

impl StryiConsensusEngine {
    pub fn new(rules: ConsensusRules) -> Self {
        Self { rules }
    }

    /// Computes total chain work by summing 2^(bits).
    pub fn compute_chain_difficulty(&self, chain: &[Block]) -> u128 {
        let mut total = 0u128;
        for block in chain {
            let bits = block.header.difficulty_bits;
            total = total.saturating_add(1u128 << bits);
        }
        total
    }
    
    
}
impl ConsensusEngine for StryiConsensusEngine {
    type Error = StryiCoreError;
    type UtxoDatabase = InMemoryUtxoStorage;

    /// Single-path validation that checks PoW, merkle root, plus handles all TransactionKinds
    async fn validate_block(
        &self,
        block: &Block,
        utxo_db: &mut Self::UtxoDatabase,
    ) -> Result<(), Self::Error> {
        // 1) Check difficulty bits vs current difficulty
        if block.header.difficulty_bits != self.rules.current_difficulty {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: format!(
                    "Block difficulty {} != current difficulty {}",
                    block.header.difficulty_bits,
                    self.rules.current_difficulty
                ),
            });
        }

        // 2) PoW leading zeros
        let block_hash = block.block_hash();
        if !meets_difficulty(&block_hash, block.header.difficulty_bits) {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Block does not meet required difficulty".to_string(),
            });
        }

        
        // 3) Check if very first transaction in the block is coinbase if not genesis block.
        if !block.header.is_genesis && !is_first_transaction_coinbase(&block) { 
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "First transaction in the block must be coinbase".to_string() 
            })
        }
        
        // 4) Merkle root validation
        if !block.validate_merkle_root()  {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Merkle root mismatch".to_string(),
            });
        }
        

        // 5) Single pass for each transaction
        for (tx_index, tx) in block.data.transactions.iter().enumerate() {
            // (A) Basic checks for transaction kind
            match tx.data.kind {
                TransactionKind::Genesis => {
                    // only valid if block.header.height == 0
                    if block.header.height != 0 {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: "Genesis TX found in non-genesis block".to_string(),
                        });
                    }
                    
                    
                    // should have no inputs
                    if !tx.data.inputs.is_empty() {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: "Genesis TX must not have any real inputs".to_string(),
                        });
                    }
                }
                TransactionKind::Coinbase => {
                    // must be first TX if height>0
                    if block.header.height > 0 && tx_index != 0 {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: format!(
                                "Coinbase TX must be index=0 in normal block, but found index={}",
                                tx_index
                            ),
                        });
                    }
                    
                    // must be no inputs
                    if !tx.data.inputs.is_empty() {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: "Coinbase TX shouldn't have real inputs".to_string(),
                        });
                    }
                    
                    //  Requires exactly 1 output to reward last block's miner
                    if tx.data.outputs.len() != 1 {
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: "Coinbase must have exactly 1 output".to_string(),
                        });
                    }
                }
                // No additional checks for regular Payment tx are required
                _ => {}
            }

            // (B) If it's Payment, do signature checks. If it's Genesis or Coinbase, skip.
            let skip_signature = matches!(tx.data.kind, TransactionKind::Genesis | TransactionKind::Coinbase);
            if !skip_signature {
                // 1) recover public key
                let author_key = tx.recover_public_key().map_err(|_| {
                    StryiCoreError::ConsensusValidationFailed {
                        details: "Cannot restore public key from TX signature".into(),
                    }
                })?;

                // 2) verify signature
                tx.verify_signature(&author_key).map_err(|err| {
                    StryiCoreError::ConsensusValidationFailed {
                        details: format!("Transaction signature invalid: {err}"),
                    }
                })?;

                // 3) checking input: UTXO ownership, sum of inputs >= sum of outputs
                let tx_author_address = AccountAddress::from_public_key(&author_key);

                let mut input_sum: u64 = 0;
                for input in &tx.data.inputs {
                    let utxo = utxo_db
                        .get_utxo(&input.previous_output)
                        .await
                        .map_err(|_| StryiCoreError::TxMissingUtxo {
                            txid: input.previous_output.txid,
                            vout: input.previous_output.vout,
                        })?;

                    if utxo.owner != tx_author_address {
                        return Err(StryiCoreError::TxWrongOwner {
                            expected: utxo.owner,
                            actual: tx_author_address,
                        });
                    }
                    input_sum = input_sum.checked_add(utxo.value).ok_or_else(|| {
                        StryiCoreError::Other {
                            msg: "Overflow summing TX inputs".into(),
                        }
                    })?;
                }

                let output_sum: u64 = tx.data.outputs.iter().map(|o| o.value).sum();
                if input_sum < output_sum {
                    return Err(StryiCoreError::TxInsufficientInputValue {
                        input_sum,
                        output_sum,
                    });
                }
            }
        }
        Ok(())
    }

    /// Adjusts difficulty by incrementing once every N blocks (example).
    async fn adjust_difficulty(
        &mut self,
        chain: &[Block],
    ) -> Result<u8, Self::Error> {
        let current_difficulty = self.rules.current_difficulty;
        let interval = self.rules.difficulty_adjustment_interval_blocks;

        if !chain.is_empty() && chain.len() % interval == 0 {
            let new_difficulty = current_difficulty.checked_add(1);
            match new_difficulty {
                Some(val) => {
                    self.rules.update_difficulty(val);
                    Ok(val)
                }
                None => Err(StryiCoreError::InvalidDifficultyValue {
                    details: "Overflow incrementing difficulty".into(),
                }),
            }
        } else {
            Ok(current_difficulty)
        }
    }

    /// Picks chain with the highest total difficulty
    async fn select_chain(
        &self,
        chains: Vec<Vec<Block>>,
    ) -> Result<Vec<Block>, Self::Error> {
        chains
            .into_iter()
            .max_by_key(|chain| self.compute_chain_difficulty(chain))
            .ok_or_else(|| StryiCoreError::ConsensusChainSelectionFailed {
                details: "No chains provided".to_string(),
            })
    }
}


/// Helper function to check if first transaction of block is coinbase
fn is_first_transaction_coinbase(block: &Block) -> bool {
    block
        .data
        .transactions
        .first()
        .is_some_and(|tx| tx.data.kind == TransactionKind::Coinbase)
}


#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use super::*;
    use crate::block::{Block, BlockData, BlockHeader, BlockHash};
    use crate::transactions::{TransactionData, TransactionOut, TransactionKind};
    use crate::address::AccountAddress;
    use crate::storage::in_memory_utxo::InMemoryUtxoStorage;

    /// make_block is a helper that fabricates blocks with a certain difficulty_bits, height, etc.
    fn make_block(difficulty_bits: u8, height: u64) -> Block {
        let header = BlockHeader {
            version: 1,
            merkle_root_hash: [0u8; 32],
            previous_block_hash: BlockHash::empty(),
            height,
            difficulty_bits,
            timestamp: 0,
            nonce: 0,
            is_genesis: false,
        };
        let data = BlockData { transactions: vec![] };
        Block { header, data }
    }

    // A small helper for building transaction data of a specific kind, no inputs, one or more outputs
    fn build_tx_data(kind: TransactionKind, outputs: Vec<(u64, AccountAddress)>) -> TransactionData {
        TransactionData {
            version: 1,
            kind,
            inputs: vec![], // typically none for coinbase/genesis
            outputs: outputs
                .into_iter()
                .map(|(val, addr)| TransactionOut { value: val, recipient: addr })
                .collect(),
        }
    }

    // Hard-coded meets_difficulty = true. We'll skip real PoW in the test to focus on TX logic
    #[tokio::test]
    async fn test_select_chain_by_cumulative_difficulty() {
        // This test remains as is, from your code, no changes, verifying chain selection logic
        let rules = ConsensusRules::new(4, 1000);
        let engine = StryiConsensusEngine::new(rules);

        // chainA => bits=4,4 => total ~ 2^4 + 2^4 = 32
        let chain_a = vec![make_block(4,0), make_block(4,0)];

        // chainB => bits=5,5,5 => total ~ 3*(2^5)=96
        let chain_b = vec![make_block(5,0), make_block(5,0), make_block(5,0)];

        // chainC => bits=1 repeated 10 => total ~ 10*(2^1)=20
        let chain_c = vec![make_block(1,0); 10];

        let best_chain = engine
            .select_chain(vec![chain_a.clone(), chain_b.clone(), chain_c.clone()])
            .await
            .expect("Should find best chain");

        let best_diff = engine.compute_chain_difficulty(&best_chain);
        let b_diff = engine.compute_chain_difficulty(&chain_b);
        assert_eq!(best_diff, b_diff, "select_chain did not pick chain B");
    }

    // Test a valid genesis block at height=0 with a Genesis transaction
    #[tokio::test]
    async fn test_genesis_block() {
        // 1) Set up an engine with difficulty=0 so we skip real PoW.
        let rules = ConsensusRules::new(0, 1000);
        let engine = StryiConsensusEngine::new(rules);

        // 2) In-memory DB
        let mut store = InMemoryUtxoStorage::default();
        
        // 3) Insert a single Genesis TX with multiple outputs if you want
        let addr_alice = AccountAddress::new(&[1u8;20]);
        let addr_bob   = AccountAddress::new(&[2u8;20]);
        
        let mut balances: HashMap<AccountAddress, u64> = HashMap::new();
        balances.insert(addr_alice, 500);
        balances.insert(addr_bob, 1000);
        
        let block = Block::new_genesis(0, 0, balances);
        
        // 5) Validate block. Should pass if we allow genesis in height=0
        let res = engine.validate_block(&block, &mut store).await;
        assert!(res.is_ok(), "Genesis block should validate fine at height=0 with no inputs");
    }
}

