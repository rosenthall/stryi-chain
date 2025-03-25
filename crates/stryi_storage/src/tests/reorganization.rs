use std::error::Error;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::{
    reorganizer::ChainReorganizer,
    StryiStorage,
    blocks::tests::create_test_storage,
};
use stryi_core::{
    block::{Block, BlockHeader, BlockHash},
    address::AccountAddress,
    transactions::{
        Transaction, TransactionData, TransactionKind,
        TransactionIn, TransactionOut, OutPoint, StryiSignature,
    },
    consensus::{ConsensusRules, StryiConsensusEngine, ConsensusEngine},
    storage::BlockStorage,
};

#[tokio::test]
async fn test_reorganizer_simple() -> Result<(), Box<dyn Error>> {
    // 1) StryiStorage in a temp dir with stats init
    let (raw_storage, _tempdir) = create_test_storage(true);
    let storage = Arc::new(RwLock::new(raw_storage));

    // 2) StryiConsensusEngine pinned to StryiStorage
    //    We'll set the start difficulty=0 in the rules so we expect block bits=0
    let rules = ConsensusRules::new(0, 1000);
    let engine = StryiConsensusEngine::<StryiStorage>::new(rules);

    // 3) Make a genesis block
    let genesis_block = create_genesis_block();
    {
        let mut store = storage.write().await;
        store.put_block(&genesis_block).await?;
        engine.validate_and_apply_block(&genesis_block, &mut *store).await?;
    }

    // 4) Build main chain: genesis -> main1 -> main2
    let main1 = create_block_with_coinbase(&genesis_block, 1, 0, /*with_payments=*/false);
    let main2 = create_block_with_coinbase(&main1,        2, 0, /*with_payments=*/false);
    {
        let mut store = storage.write().await;
        store.put_block(&main1).await?;
        engine.validate_and_apply_block(&main1, &mut *store).await?;

        store.put_block(&main2).await?;
        engine.validate_and_apply_block(&main2, &mut *store).await?;
    }
    let old_tip = main2.block_hash();

    // 5) Build heavier fork: genesis -> fork1 -> fork2 -> fork3
    let fork1 = create_block_with_coinbase(&genesis_block, 1, 0, false);
    let fork2 = create_block_with_coinbase(&fork1,        2, 0, false);
    let fork3 = create_block_with_coinbase(&fork2,        3, 0, false);

    // store them but do not apply
    {
        let mut store = storage.write().await;
        store.put_block(&fork1).await?;
        store.put_block(&fork2).await?;
        store.put_block(&fork3).await?;
    }
    let new_tip = fork3.block_hash();

    // 6) Reorg
    let reorganizer = ChainReorganizer::new(Arc::clone(&storage));
    reorganizer
        .reorganize_with_validation(old_tip, new_tip, &engine)
        .await?;

    // 7) Confirm best tip is fork3
    {
        let store = storage.read().await;
        let state = store.get_current_storage_state()?;
        assert_eq!(state.latest_block.1, fork3.block_hash(), "best tip is fork3");
        info!("Reorg success with zero-difficulty blocks => final tip: {}", fork3.block_hash());
    }

    Ok(())
}

/// Creates a block0 with `TransactionKind::Genesis` + difficulty_bits=0, is_genesis=true
fn create_genesis_block() -> Block {
    let genesis_tx = Transaction {
        data: TransactionData {
            version: 1,
            kind: TransactionKind::Genesis,
            inputs: vec![],
            outputs: vec![TransactionOut {
                value: 10_000,
                recipient: AccountAddress::new(&[0xAA;20]),
            }],
        },
        signature: StryiSignature::default(),
    };
    let mut blk = Block {
        header: BlockHeader {
            version: 1,
            merkle_root_hash: [0u8; 32],
            previous_block_hash: BlockHash::empty(),
            height: 0,
            difficulty_bits: 0, // <== zero difficulty
            timestamp: 1660000000,
            nonce: 0,
            is_genesis: true,
        },
        data: stryi_core::block::BlockData {
            transactions: vec![genesis_tx],
        },
    };
    blk.update_merkle_root();
    blk
}

/// Creates a non-genesis block with coinbase as first transaction,
/// difficulty_bits=0 => trivially meets PoW
/// Optionally add a second Payment tx referencing the parent's coinbase out.
fn create_block_with_coinbase(
    parent: &Block,
    height: u64,
    difficulty: u8,
    with_payments: bool
) -> Block {
    // (1) coinbase
    let coinbase = Transaction {
        data: TransactionData {
            version: 1,
            kind: TransactionKind::Coinbase,
            inputs: vec![],
            outputs: vec![
                TransactionOut {
                    value: 500, // reward
                    recipient: AccountAddress::new(&[0xCC; 20]),
                }
            ],
        },
        signature: StryiSignature::default(),
    };
    let mut txs = vec![coinbase];

    // (2) optional Payment referencing parent's coinbase outpoint(0)
    if with_payments {
        let parent_txid = parent.data.transactions[0].data.hash();
        let tx_in = TransactionIn {
            previous_output: OutPoint {
                txid: parent_txid,
                vout: 0,
            },
            sequence: 0xFFFFFFFF,
        };
        let tx_out = TransactionOut {
            value: 300,
            recipient: AccountAddress::new(&[0xDD;20]),
        };
        let pay_data = TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![tx_in],
            outputs: vec![tx_out],
        };
        let pay_tx = Transaction {
            data: pay_data,
            signature: StryiSignature::default(),
        };
        txs.push(pay_tx);
    }

    let parent_hash = parent.block_hash();
    let mut blk = Block {
        header: BlockHeader {
            version: 1,
            merkle_root_hash: [0u8; 32],
            previous_block_hash: parent_hash,
            height,
            difficulty_bits: difficulty, // zero => no PoW needed
            timestamp: 1660000000 + height,
            nonce: 0,
            is_genesis: false,
        },
        data: stryi_core::block::BlockData {
            transactions: txs,
        },
    };
    blk.update_merkle_root();
    blk
}
