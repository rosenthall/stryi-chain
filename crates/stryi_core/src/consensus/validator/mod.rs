use crate::{
    block::Block,
    consensus::ConsensusRules,
    error::StryiCoreError,
    storage::UtxoStorage,
};

mod header;
pub use header::validate_header;
pub mod block;
pub mod tx;

/// Validates a block against the supplied consensus rules.
///
/// The pipeline:
/// 1. header checks              (`header::validate_header`);
/// 2. static block structure     (`block::validate_block_structure`);
/// 3. dynamic, UTXO-dependent    (`block::validate_transactions`).
pub struct BlockValidator {
    pub rules: ConsensusRules,
}

impl BlockValidator {
    pub fn new(rules: ConsensusRules) -> Self {
        Self { rules }
    }

    /// Runs the full consensus pipeline.
    pub async fn validate<US>(
        &self,
        block: &Block,
        utxo_storage: &mut US,
    ) -> Result<(), StryiCoreError>
    where
        US: UtxoStorage + Send,
    {
        header::validate_header(block, &self.rules)?;
        block::validate_block_structure(block)?;
        block::validate_transactions(block, &self.rules, utxo_storage).await
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        address::AccountAddress,
        block::Block,
        consensus::ConsensusRules,
        storage::{StryiInMemoryStorage, UtxoStorage},
        transactions::{
            OutPoint, StryiSignature, Transaction, TransactionData, TransactionIn,
            TransactionKind, TransactionOut, UTXO,
        },
    };
    use k256::{ecdsa::SigningKey, elliptic_curve::rand_core::OsRng};
    use std::collections::HashMap;
    use crate::block::mine_block_in_parallel;

    /// Helper: after a block is proven valid, “commit” every new output
    /// into the in-memory UTXO set and spend the inputs of payment txs.
    async fn commit(block: &Block, storage: &mut StryiInMemoryStorage) {
        for tx in &block.data.transactions {
            // add fresh UTXOs
            for (vout, o) in tx.data.outputs.iter().enumerate() {
                storage
                    .put_utxo(
                        OutPoint { txid: tx.data.hash(), vout: vout as u32 },
                        UTXO {
                            txid: tx.data.hash(),
                            vout: vout as u32,
                            value: o.value,
                            owner: o.recipient,
                        },
                    )
                    .await
                    .unwrap();
            }
            // spend inputs
            if tx.data.kind == TransactionKind::Payment {
                for i in &tx.data.inputs {
                    storage.remove_utxo(i.previous_output).await.unwrap();
                }
            }
        }
    }

    // full happy-path (genesis -> payment block)
    #[tokio::test]
    async fn validator_accepts_normal_block_chain() {
        // 1. network rules – tiny PoW so mining is fast
        let rules = ConsensusRules {
            current_difficulty: 2,
            difficulty_adjustment_interval_blocks: 0,
            initial_subsidy: 0,
            decay_interval: 0,
            decay_step: 0,
        };
        let validator = BlockValidator::new(rules.clone());

        // 2. keys / accounts
        let sk_genesis = SigningKey::random(&mut OsRng);
        let addr_genesis = AccountAddress::from_public_key(sk_genesis.verifying_key());

        let sk_alice = SigningKey::random(&mut OsRng);
        let addr_alice = AccountAddress::from_public_key(sk_alice.verifying_key());

        // 3. genesis block: 1000 coins to @genesis
        let mut balances = HashMap::new();
        balances.insert(addr_genesis, 1_000);
        let genesis = Block::new_genesis(1, rules.current_difficulty, balances);

        // 4. storage + validate + commit
        let mut store = StryiInMemoryStorage::default();
        validator.validate(&genesis, &mut store).await.unwrap();
        commit(&genesis, &mut store).await;

        // 5. build a payment tx (genesis → alice 600, change 400)
        let spend_op = OutPoint { txid: genesis.data.transactions[0].data.hash(), vout: 0 };
        let pay_data = TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![TransactionIn { previous_output: spend_op, sequence: 0xFFFF_FFFF }],
            outputs: vec![
                TransactionOut { value: 600, recipient: addr_alice },
                TransactionOut { value: 400, recipient: addr_genesis },
            ],
        };
        let pay_tx = pay_data.sign(&sk_genesis);

        // 6. coinbase (reward 0 for test)
        let coinbase = Transaction {
            data: TransactionData {
                version: 1,
                kind: TransactionKind::Coinbase,
                inputs: vec![],
                outputs: vec![TransactionOut { value: 0, recipient: addr_genesis }],
            },
            signature: StryiSignature(Box::new([0u8; 65])),
        };

        // 7. assemble + mine the block
        let mut blk = Block::new(
            vec![coinbase, pay_tx.clone()],
            genesis.block_hash(),
            1,
            rules.current_difficulty,
            1_700_000_001,
            7,
        );
        mine_block_in_parallel(&mut blk, u64::MAX);

        // 8. validate & commit
        validator.validate(&blk, &mut store).await.unwrap();
        commit(&blk, &mut store).await;
    }

    // difficulty mismatch must be rejected
    #[tokio::test]
    async fn validator_rejects_wrong_difficulty() {
        let rules = ConsensusRules {
            current_difficulty: 1,
            difficulty_adjustment_interval_blocks: 0,
            initial_subsidy: 0,
            decay_interval: 0,
            decay_step: 0,
        };
        let validator = BlockValidator::new(rules.clone());

        // tiny genesis
        let mut balances = HashMap::new();
        balances.insert(AccountAddress::new(&[1u8; 20]), 10);
        let genesis = Block::new_genesis(1, rules.current_difficulty, balances);

        let mut store = StryiInMemoryStorage::default();
        validator.validate(&genesis, &mut store).await.unwrap();
        commit(&genesis, &mut store).await;

        // clone genesis but bump difficulty bits
        let bad = Block::new(
            genesis.data.transactions.clone(),
            genesis.block_hash(),
            1,
            rules.current_difficulty + 3, // wrong bits
            1_700_000_999,
            42,
        );

        let res = validator.validate(&bad, &mut store).await;
        assert!(res.is_err(), "block with wrong difficulty must be rejected");
    }

    // Test the simplest transactions rejections 
    // (A) duplicate-tx rejection, 
    // (B) in-block double-spend rejection
    #[tokio::test]
    async fn validator_duplicate_and_double_spend() {

        // Common set-up: consensus rules + helper constructor shortcuts
        let rules = ConsensusRules { current_difficulty: 2, difficulty_adjustment_interval_blocks: 0, initial_subsidy: 0, decay_interval: 0, decay_step: 0 };
        let validator = BlockValidator::new(rules.clone());

        let coinbase_template = |recipient: [u8; 20]| Transaction {
            data: TransactionData {
                version: 1,
                kind: TransactionKind::Coinbase,
                inputs: vec![],
                outputs: vec![TransactionOut { value: 0, recipient: AccountAddress::new(&recipient) }],
            },
            signature: StryiSignature(Box::new([0u8; 65])),
        };

        // CASE A — duplicate transaction
        {
            let mut store = StryiInMemoryStorage::default();

            // genesis with one spendable output
            let mut balances = HashMap::new();
            balances.insert(AccountAddress::new(&[7u8; 20]), 500);
            let genesis = Block::new_genesis(1, rules.current_difficulty, balances);
            validator.validate(&genesis, &mut store).await.unwrap();
            commit(&genesis, &mut store).await;

            // prepare *one* payment tx …
            let spend = OutPoint { txid: genesis.data.transactions[0].data.hash(), vout: 0 };
            let pay = TransactionData {
                version: 1,
                kind: TransactionKind::Payment,
                inputs: vec![TransactionIn { previous_output: spend, sequence: 0 }],
                outputs: vec![TransactionOut { value: 500, recipient: AccountAddress::new(&[9u8; 20]) }],
            }
                .sign(&SigningKey::random(&mut OsRng));

            // ... but put it into the block twice
            let mut blk = Block::new(
                vec![coinbase_template([8u8; 20]), pay.clone(), pay],
                genesis.block_hash(),
                1,
                rules.current_difficulty,
                1_700_000_123,
                42,
            );
            mine_block_in_parallel(&mut blk, u64::MAX);

            let err = validator.validate(&blk, &mut store).await
                .expect_err("duplicate-tx block must be rejected");
            assert!(
                matches!(err, StryiCoreError::ConsensusValidationFailed { .. }),
                "expected consensus-failure, got {err:?}"
            );
        }

        // CASE B — two different txs spend SAME outpoint (double-spend)
        {
            let mut store = StryiInMemoryStorage::default();

            // fresh genesis
            let mut balances = HashMap::new();
            balances.insert(AccountAddress::new(&[2u8; 20]), 900);
            let genesis = Block::new_genesis(1, rules.current_difficulty, balances);
            validator.validate(&genesis, &mut store).await.unwrap();
            commit(&genesis, &mut store).await;

            // build two independent payments referencing identical input
            let src = OutPoint { txid: genesis.data.transactions[0].data.hash(), vout: 0 };
            let mk_pay = |recipient: [u8; 20]| TransactionData {
                version: 1,
                kind: TransactionKind::Payment,
                inputs: vec![TransactionIn { previous_output: src, sequence: 0 }],
                outputs: vec![TransactionOut { value: 450, recipient: AccountAddress::new(&recipient) }],
            }
                .sign(&SigningKey::random(&mut OsRng));

            let pay1 = mk_pay([3u8; 20]);
            let pay2 = mk_pay([4u8; 20]);

            let mut blk = Block::new(
                vec![coinbase_template([5u8; 20]), pay1, pay2],
                genesis.block_hash(),
                1,
                rules.current_difficulty,
                1_700_000_124,
                77,
            );
            mine_block_in_parallel(&mut blk, u64::MAX);

            let err = validator.validate(&blk, &mut store).await
                .expect_err("double-spend block must be rejected");
            assert!(
                matches!(err, StryiCoreError::TxDoubleSpend { .. }),
                "expected TxDoubleSpend, got {err:?}"
            );
        }
    }
}