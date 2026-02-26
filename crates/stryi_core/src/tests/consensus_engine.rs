use crate::consensus::{ConsensusConsts, ConsensusEngine, ConsensusVerdict};
use crate::error::StryiCoreError;
use crate::tests::{
    keypair_from_seed, make_block_mined, make_coinbase_tx, make_engine, make_payment_tx,
    make_test_genesis,
};
use crate::transactions::OutPoint;

/// Gets the genesis UTXO outpoint for the first output of genesis tx.
fn genesis_outpoint(genesis: &crate::block::Block) -> OutPoint {
    let tx = &genesis.data.transactions[0];
    OutPoint {
        txid: tx.data.hash(),
        vout: 0,
    }
}

#[tokio::test]
async fn test_on_block_rejects_block_with_overspending_transaction() {
    let consts = ConsensusConsts::default();
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (genesis, db) = make_test_genesis(&[(alice_addr, 1_000)], consts);
    let mut engine = make_engine(db, consts).await;

    let coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(1), miner_addr);
    // Alice has 1000 but tries to send 1200
    let overspend_tx = make_payment_tx(
        &alice_key,
        vec![genesis_outpoint(&genesis)],
        vec![(1_200, miner_addr)],
    );

    let block = make_block_mined(
        vec![coinbase, overspend_tx],
        1,
        consts.difficulty_bits_for_height(1),
        genesis.block_hash(),
    );

    let verdict = engine.on_block(block).await.unwrap();
    match verdict {
        ConsensusVerdict::Rejected(StryiCoreError::TxInsufficientInputValue {
            input_sum,
            output_sum,
        }) => {
            assert_eq!(input_sum, 1_000);
            assert_eq!(output_sum, 1_200);
        }
        ConsensusVerdict::Rejected(other) => {
            panic!("Expected TxInsufficientInputValue, got: {other:?}")
        }
        other => panic!("Expected Rejected, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_on_block_rejects_block_with_wrong_owner() {
    let consts = ConsensusConsts::default();
    let (_, alice_addr) = keypair_from_seed(1);
    let (bob_key, bob_addr) = keypair_from_seed(2);
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (genesis, db) = make_test_genesis(&[(alice_addr, 100_000)], consts);
    let mut engine = make_engine(db, consts).await;

    let coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(1), miner_addr);
    // Bob tries to spend Alice's UTXO
    let theft_tx = make_payment_tx(
        &bob_key,
        vec![genesis_outpoint(&genesis)],
        vec![(50_000, bob_addr)],
    );

    let block = make_block_mined(
        vec![coinbase, theft_tx],
        1,
        consts.difficulty_bits_for_height(1),
        genesis.block_hash(),
    );

    let verdict = engine.on_block(block).await.unwrap();
    match verdict {
        // In block validation: expected = utxo.owner (Alice), actual = signer (Bob)
        ConsensusVerdict::Rejected(StryiCoreError::TxWrongOwner { expected, actual }) => {
            assert_eq!(expected, alice_addr);
            assert_eq!(actual, bob_addr);
        }
        ConsensusVerdict::Rejected(other) => panic!("Expected TxWrongOwner, got: {other:?}"),
        other => panic!("Expected Rejected, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_on_block_buffers_weaker_fork() {
    let consts = ConsensusConsts::default();
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (genesis, db) = make_test_genesis(&[(miner_addr, 100_000)], consts);
    let mut engine = make_engine(db, consts).await;

    // build a 3-block canonical chain
    let mut prev = genesis.block_hash();
    for h in 1..=3 {
        let coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(h), miner_addr);
        let block = make_block_mined(
            vec![coinbase],
            h,
            consts.difficulty_bits_for_height(h),
            prev,
        );
        prev = block.block_hash();
        let v = engine.on_block(block).await.unwrap();
        assert!(
            matches!(v, ConsensusVerdict::Applied { .. }),
            "Canonical block at height {h} should be Applied"
        );
    }

    // submit a fork block at height 1 branching from genesis (weaker than the 3-block chain)
    let fork_coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(1), miner_addr);
    let fork_block = make_block_mined(
        vec![fork_coinbase],
        1,
        consts.difficulty_bits_for_height(1),
        genesis.block_hash(),
    );

    let verdict = engine.on_block(fork_block).await.unwrap();
    assert_eq!(verdict, ConsensusVerdict::Buffered);
}

#[tokio::test]
async fn test_on_block_causes_reorganization_when_fork_is_heavier() {
    let consts = ConsensusConsts::default();
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (genesis, db) = make_test_genesis(&[(miner_addr, 100_000)], consts);
    let mut engine = make_engine(db, consts).await;

    // build 1-block canonical chain
    let canonical_coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(1), miner_addr);
    let canonical_block = make_block_mined(
        vec![canonical_coinbase],
        1,
        consts.difficulty_bits_for_height(1),
        genesis.block_hash(),
    );
    let canonical_hash = canonical_block.block_hash();
    let v = engine.on_block(canonical_block).await.unwrap();
    assert!(matches!(v, ConsensusVerdict::Applied { .. }));

    // build 2-block fork from genesis (total fork work > canonical work triggers reorg)
    let fork1_coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(1), miner_addr);
    let fork1 = make_block_mined(
        vec![fork1_coinbase],
        1,
        consts.difficulty_bits_for_height(1),
        genesis.block_hash(),
    );

    // the first fork block has the same work as canonical, so it gets buffered
    let v = engine.on_block(fork1.clone()).await.unwrap();
    assert_eq!(v, ConsensusVerdict::Buffered);

    let fork2_coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(2), miner_addr);
    let fork2 = make_block_mined(
        vec![fork2_coinbase],
        2,
        consts.difficulty_bits_for_height(2),
        fork1.block_hash(),
    );

    // the second fork block tips the balance and causes reorg
    let verdict = engine.on_block(fork2).await.unwrap();
    match verdict {
        ConsensusVerdict::CausedReorganization { deleted_blocks } => {
            assert!(
                deleted_blocks.values().any(|h| *h == canonical_hash),
                "Deleted blocks should contain the original canonical block. Got: {deleted_blocks:?}"
            );
        }
        other => panic!("Expected CausedReorganization, got: {other:?}"),
    }
}
