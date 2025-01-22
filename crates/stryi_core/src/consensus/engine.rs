use crate::{
    block::{meets_difficulty, Block},
    consensus::{ConsensusEngine, ConsensusRules},
    error::StryiCoreError,
    storage::UtxoStorage,
    address::AccountAddress,
};

use crate::storage::in_memory_utxo::InMemoryUtxoStorage;

pub struct StryiConsensusEngine {
    pub rules: ConsensusRules,
}

impl StryiConsensusEngine {
    /// Creates a new engine with the given rules.
    pub fn new(rules: ConsensusRules) -> Self {
        Self { rules }
    }

    /// Sums 2^(difficulty_bits) for each block to get chain work.
    fn compute_chain_difficulty(&self, chain: &[Block]) -> u128 {
        let mut total: u128 = 0;
        for block in chain {
            let bits = block.header.difficulty_bits;
            // naive shift approach (assuming bits < 128)
            total = total.saturating_add(1u128 << bits);
        }
        total
    }
}
impl ConsensusEngine for StryiConsensusEngine {
    type Error = StryiCoreError;
    type UtxoDatabase = InMemoryUtxoStorage; // TODO: StryiStorage via lib.rs/sled

    /// Validates a block’s PoW, merkle root, and transaction ownership/spend logic.
    async fn validate_block(
        &self,
        block: &Block,
        utxo_database: &mut Self::UtxoDatabase,
    ) -> Result<(), Self::Error> {
        // 1) Check if the block's difficulty matches our current rules
        if block.header.difficulty_bits != self.rules.current_difficulty {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: format!(
                    "Block difficulty {} != current difficulty {}",
                    block.header.difficulty_bits,
                    self.rules.current_difficulty
                ),
            });
        }

        // 2) Check PoW
        let block_hash = block.block_hash();
        if !meets_difficulty(&block_hash, block.header.difficulty_bits) {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Block does not meet required difficulty".to_string(),
            });
        }

        // 3) Validate the Merkle root
        if !block.validate_merkle_root() {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Merkle root mismatch".to_string(),
            });
        }

        // 4) For each transaction in the block:
        for tx in &block.data.transactions {
            // (a) Recover the public key from the single signature
            let author_key = tx
                .recover_public_key()
                .map_err(|_| StryiCoreError::ConsensusValidationFailed {
                    details: "Cannot restore public key from tx signature".into(),
                })?;

            // (b) Verify that signature is indeed valid with that key
            tx.verify_signature(&author_key).map_err(|err| {
                StryiCoreError::ConsensusValidationFailed {
                    details: format!("Transaction signature invalid: {err}"),
                }
            })?;

            // (c) Convert to address
            let tx_author_address = AccountAddress::from_public_key(&author_key);

            // (d) For each input, fetch the UTXO and check ownership
            let mut input_sum: u64 = 0;
            for input in &tx.data.inputs {
                let utxo = utxo_database
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

                // accumulate the input value
                input_sum = input_sum.checked_add(utxo.value).ok_or_else(|| {
                    StryiCoreError::Other {
                        msg: "Overflow while summing inputs".into(),
                    }
                })?;
            }

            // (e) Sum outputs and check
            let output_sum: u64 = tx.data.outputs.iter().map(|o| o.value).sum();
            if input_sum < output_sum {
                return Err(StryiCoreError::TxInsufficientInputValue {
                    input_sum,
                    output_sum,
                });
            }
        }

        // If everything is good, return Ok
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

    /// Selects the chain with the highest cumulative difficulty.
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



#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Block, BlockHeader, BlockData, BlockHash};

    // make_block is a helper that fabricates blocks with a certain difficulty_bits
    fn make_block(difficulty_bits: u8) -> Block {
        let header = BlockHeader {
            version: 1,
            merkle_root_hash: [0u8; 32],
            previous_block_hash: BlockHash::empty(),
            height: 0,
            difficulty_bits,
            timestamp: 0,
            nonce: 0,
        };
        let data = BlockData { transactions: vec![] };
        Block { header, data }
    }

    #[tokio::test]
    async fn test_select_chain_by_cumulative_difficulty() {
        // Set up the engine
        let rules = ConsensusRules::new(4, 1000);
        let engine = StryiConsensusEngine::new(rules);

        // chainA => bits=4,4 => total ~ 2^4 + 2^4 = 32
        let chain_a = vec![make_block(4), make_block(4)];

        // chainB => bits=5,5,5 => total ~ 3*(2^5)=96
        let chain_b = vec![make_block(5), make_block(5), make_block(5)];

        // chainC => bits=1 repeated 10 => total ~ 10*(2^1)=20
        let chain_c = vec![make_block(1); 10];

        let best_chain = engine
            .select_chain(vec![chain_a.clone(), chain_b.clone(), chain_c.clone()])
            .await
            .expect("Should find best chain");

        let best_diff = engine.compute_chain_difficulty(&best_chain);
        let b_diff = engine.compute_chain_difficulty(&chain_b);
        assert_eq!(best_diff, b_diff, "select_chain did not pick chain B");
    }

}
