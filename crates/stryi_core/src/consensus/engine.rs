use std::marker::PhantomData;
use crate::{
    block::Block,
    consensus::{ConsensusEngine, ConsensusRules},
    error::StryiCoreError,
};
use crate::block::BlockValidator;
use crate::storage::in_memory_utxo::InMemoryUtxoStorage;
use crate::storage::UtxoStorage;
use crate::transactions::UtxoProcessor;

/// StryiConsensusEngine is responsible for validating and processing blocks according to the consensus rules.
///
/// It is generic over a type parameter `DB` that implements the `UtxoStorage` trait. This enables the engine
/// to work with any storage backend that conforms to the UTXO interface (e.g. InMemoryUtxoStorage, StryiStorage, etc.).
///
/// The `_phantom` field is a PhantomData marker used to hold the generic type parameter `DB` without storing an actual instance.
pub struct StryiConsensusEngine<DB: UtxoStorage> {
    /// Consensus rules object defining parameters like current difficulty and adjustment intervals.
    pub(crate) rules: ConsensusRules,

    /// Block validator used to verify block-level properties such as proof-of-work, merkle root correctness,
    /// coinbase placement, and transaction ordering.
    pub(crate) block_validator: BlockValidator,

    /// UTXO processor that applies transactions within a block to update the UTXO set.
    // TODO: consider renaming it later, maybe in TransactionsProcessor? Current name is a little weird
    pub(crate) utxo_processor: UtxoProcessor,

    /// PhantomData marker to associate the generic storage type DB with this engine.
    _phantom: PhantomData<DB>,
}

impl<DB: UtxoStorage> StryiConsensusEngine<DB> {
    /// Constructs a new StryiConsensusEngine using the provided consensus rules.
    ///
    /// The engine is initialized with:
    /// - A copy of the consensus rules.
    /// - A new BlockValidator instance (initialized with the current difficulty).
    /// - A new UtxoProcessor.
    /// - A PhantomData marker for the DB type.
    pub fn new(rules: ConsensusRules) -> Self {
        Self {
            rules: rules.clone(),
            block_validator: BlockValidator::new(rules),
            utxo_processor: UtxoProcessor::new(),
            _phantom: PhantomData,
        }
    }

    /// Computes the cumulative chain work for a given slice of blocks.
    ///
    /// For each block, the work is defined as 2^(difficulty_bits).
    /// The total chain work is the sum of these values.
    /// This metric is used in chain selection algorithms to determine which fork is "heavier."
    pub fn compute_chain_difficulty(&self, chain: &[Block]) -> u128 {
        let mut total = 0u128;
        for block in chain {
            let bits = block.header.difficulty_bits;
            total = total.saturating_add(1u128 << bits);
        }
        total
    }
}



impl<DB: UtxoStorage> ConsensusEngine for StryiConsensusEngine<DB> {
    type Error = StryiCoreError;
    type UtxoDatabase = DB;

    /// Adjusts difficulty by incrementing once every N blocks.
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


    /// Validates and applies a block to the blockchain atomically.
    ///
    /// This method first validates the block. If validation succeeds,
    /// it applies the block to the UTXO set. The entire operation is atomic;
    /// if application fails, no changes are made to the UTXO set.
    async fn validate_and_apply_block(
        &self,
        block: &Block,
        utxo_storage: &mut Self::UtxoDatabase,
    ) -> Result<(), Self::Error> {
        // Step 1: Validate the block using BlockValidator
        self.validate_block(block, utxo_storage).await?;

        // Step 2: Apply the block using UtxoProcessor
        self.utxo_processor
            .apply_block(block, utxo_storage)
            .await
            .map_err(|e| StryiCoreError::ConsensusBlockApplyingFailed {
                details: format!("Failed to apply block: {}", e),
            })
    }
    /// Validates a given block according to consensus rules and sanity of transactions
    async fn validate_block(
        &self,
        block: &Block,
        utxo_storage: &mut Self::UtxoDatabase,
    ) -> Result<(), Self::Error> {
        self.block_validator.validate_block(block, utxo_storage).await
    }

}


#[cfg(test)]
impl StryiConsensusEngine<InMemoryUtxoStorage> {
    /// Testing-only constructor that creates a StryiConsensusEngine using InMemoryUtxoStorage.
    ///
    /// This simplifies testing by allowing you to instantiate the consensus engine
    /// without manually specifying the storage type.
    pub fn new_with_inmemory_storage(rules: ConsensusRules) -> Self {
        // We simply call the generic new() method.
        Self::new(rules)
    }
}


#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use super::*;
    use crate::block::{Block, BlockData, BlockHeader, BlockHash};
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


    // Hard-coded meets_difficulty = true. We'll skip real PoW in the test to focus on TX logic
    #[tokio::test]
    async fn test_select_chain_by_cumulative_difficulty() {
        // This test remains as is, from your code, no changes, verifying chain selection logic
        let rules = ConsensusRules::new_test(4);
        let engine = StryiConsensusEngine::<InMemoryUtxoStorage>::new_with_inmemory_storage(rules);
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
        let rules = ConsensusRules::new_test(0);
        let engine = StryiConsensusEngine::<InMemoryUtxoStorage>::new_with_inmemory_storage(rules);
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

