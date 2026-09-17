use crate::address::AccountAddress;
use crate::block::{Block, BlockHash, meets_difficulty};
use crate::consensus::{
    ConsensusConsts, ConsensusEngine, ConsensusVerdict, StryiConsensusEngine, UtxoStorage,
    validate_block_structure,
};
use crate::error::StryiCoreError;
use crate::storage::StryiInMemoryStorage;
use crate::tests::{
    keypair_from_seed, make_block_mined, make_coinbase_tx, make_engine, make_genesis_tx,
    make_payment_tx, make_test_genesis,
};
use crate::transactions::{OutPoint, Transaction, TransactionHash, UTXO};
use std::collections::HashMap;

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

fn make_test_block(
    txs: Vec<Transaction>,
    height: u64,
    difficulty_bits: u8,
    previous_block_hash: BlockHash,
) -> Block {
    const MAX_ATTEMPTS: u32 = 100_000;
    let timestamp = 1_700_000_000 + height * 10;
    let mut block = Block::new(
        txs,
        previous_block_hash,
        height,
        difficulty_bits,
        timestamp,
        1,
    );
    for nonce in 0..MAX_ATTEMPTS {
        block.header.nonce = nonce;
        if meets_difficulty(&block.block_hash(), difficulty_bits) {
            return block;
        }
    }
    panic!("Failed to mine test block within {MAX_ATTEMPTS} attempts for {difficulty_bits} difficulty bits");
}

#[derive(Debug, PartialEq, Eq)]
struct EngineSnapshot {
    tip: Option<(u64, BlockHash, u128)>,
    alice_utxos: HashMap<OutPoint, UTXO>,
    miner_utxos: HashMap<OutPoint, UTXO>,
}

async fn capture_snapshot(
    engine: &StryiConsensusEngine<StryiInMemoryStorage>,
    alice_addr: AccountAddress,
    miner_addr: AccountAddress,
) -> EngineSnapshot {
    let db = engine.db.read().await;
    let tip = engine.tip();
    let alice_utxos = db.get_utxos_for_address(alice_addr).await.unwrap();
    let miner_utxos = db.get_utxos_for_address(miner_addr).await.unwrap();
    EngineSnapshot {
        tip,
        alice_utxos,
        miner_utxos,
    }
}

#[test]
fn test_validate_block_structure_scenarios() {
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (alice_key, _alice_addr) = keypair_from_seed(1);
    let (_, bob_addr) = keypair_from_seed(2);

    let coinbase1 = make_coinbase_tx(&miner_key, 50_000, miner_addr);
    let coinbase2 = make_coinbase_tx(&alice_key, 25_000, bob_addr);
    let dummy_op = OutPoint {
        txid: TransactionHash::new(&[0u8; 32]),
        vout: 0,
    };
    let payment = make_payment_tx(&alice_key, vec![dummy_op], vec![(100, bob_addr)]);
    let genesis_tx = make_genesis_tx(vec![(1_000, bob_addr)]);

    let cases = [
        ("[Coinbase] is allowed", vec![coinbase1.clone()], Ok(())),
        (
            "[Coinbase, Payment] is allowed",
            vec![coinbase1.clone(), payment.clone()],
            Ok(()),
        ),
        (
            "[] is rejected (empty transaction list)",
            vec![],
            Err("Block transaction list cannot be empty"),
        ),
        (
            "[Payment] is rejected (first tx is not Coinbase)",
            vec![payment.clone()],
            Err("First transaction must be a Coinbase transaction"),
        ),
        (
            "[Genesis] is rejected for normal block",
            vec![genesis_tx.clone()],
            Err("Genesis transaction is forbidden in non-genesis blocks"),
        ),
        (
            "[Coinbase, second different Coinbase] is rejected",
            vec![coinbase1.clone(), coinbase2.clone()],
            Err("Block cannot contain multiple Coinbase transactions (extra Coinbase at index 1)"),
        ),
        (
            "[Coinbase, Genesis] is rejected",
            vec![coinbase1.clone(), genesis_tx],
            Err("Genesis transaction is forbidden in non-genesis blocks (found at index 1)"),
        ),
        (
            "[Coinbase, Payment, additional Coinbase] is rejected",
            vec![coinbase1, payment, coinbase2],
            Err("Block cannot contain multiple Coinbase transactions (extra Coinbase at index 2)"),
        ),
    ];

    for (name, txs, expected) in cases {
        let block = Block::new(txs, BlockHash::empty(), 1, 0, 1_700_000_000, 1);
        let res = validate_block_structure(&block);
        match (res, expected) {
            (Ok(()), Ok(())) => {}
            (
                Err(StryiCoreError::ConsensusValidationFailed { details }),
                Err(expected_msg),
            ) => {
                assert_eq!(details, expected_msg, "{name}: mismatch in failure details");
            }
            (actual, expected) => {
                panic!("{name}: expected {expected:?}, got {actual:?}");
            }
        }
    }
}

#[tokio::test]
async fn test_on_block_rejects_canonical_blocks_with_extra_minting_txs() {
    let consts = ConsensusConsts::default();
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (miner_key, miner_addr) = keypair_from_seed(0);

    let cases = [
        (
            "extra Coinbase transaction",
            make_coinbase_tx(&alice_key, 1_000, alice_addr),
            "Block cannot contain multiple Coinbase transactions (extra Coinbase at index 1)",
        ),
        (
            "Genesis transaction in normal block",
            make_genesis_tx(vec![(5_000, alice_addr)]),
            "Genesis transaction is forbidden in non-genesis blocks (found at index 1)",
        ),
    ];

    for (name, extra_tx, expected_error) in cases {
        let (genesis, db) = make_test_genesis(&[(alice_addr, 100_000)], consts);
        let mut engine = make_engine(db, consts).await;
        let snapshot_before = capture_snapshot(&engine, alice_addr, miner_addr).await;

        let coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(1), miner_addr);
        let block = make_test_block(
            vec![coinbase, extra_tx],
            1,
            consts.difficulty_bits_for_height(1),
            genesis.block_hash(),
        );

        let verdict = engine.on_block(block).await.unwrap();
        match verdict {
            ConsensusVerdict::Rejected(StryiCoreError::ConsensusValidationFailed { details }) => {
                assert_eq!(details, expected_error, "{name}: unexpected rejection reason");
            }
            other => panic!("{name}: expected Rejected(ConsensusValidationFailed), got: {other:?}"),
        }

        let snapshot_after = capture_snapshot(&engine, alice_addr, miner_addr).await;
        assert_eq!(
            snapshot_before, snapshot_after,
            "{name}: canonical tip and UTXO state must remain unchanged"
        );
    }
}

#[tokio::test]
async fn test_on_block_rejects_fork_block_with_additional_coinbase() {
    let consts = ConsensusConsts::default();
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (genesis, db) = make_test_genesis(&[(miner_addr, 100_000)], consts);
    let mut engine = make_engine(db, consts).await;

    let mut prev = genesis.block_hash();
    for h in 1..=2 {
        let coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(h), miner_addr);
        let block = make_test_block(
            vec![coinbase],
            h,
            consts.difficulty_bits_for_height(h),
            prev,
        );
        prev = block.block_hash();
        let v = engine.on_block(block).await.unwrap();
        assert!(matches!(v, ConsensusVerdict::Applied { .. }));
    }

    let snapshot_before = capture_snapshot(&engine, alice_addr, miner_addr).await;

    // Fork block branches from genesis (known canonical ancestor that is not current tip).
    let fork_coinbase1 = make_coinbase_tx(&miner_key, consts.block_subsidy(1), miner_addr);
    let fork_coinbase2 = make_coinbase_tx(&alice_key, 1_000, alice_addr);
    let fork_block = make_test_block(
        vec![fork_coinbase1, fork_coinbase2],
        1,
        consts.difficulty_bits_for_height(1),
        genesis.block_hash(),
    );

    let verdict = engine.on_block(fork_block).await.unwrap();
    match verdict {
        ConsensusVerdict::Rejected(StryiCoreError::ConsensusValidationFailed { details }) => {
            assert_eq!(
                details,
                "Block cannot contain multiple Coinbase transactions (extra Coinbase at index 1)",
                "Fork block rejection reason mismatch"
            );
        }
        other => panic!("Expected Rejected(ConsensusValidationFailed) for fork block, got: {other:?}"),
    }

    let snapshot_after = capture_snapshot(&engine, alice_addr, miner_addr).await;
    assert_eq!(
        snapshot_before, snapshot_after,
        "Canonical tip and UTXOs must remain unchanged after rejecting fork block"
    );
    assert_eq!(
        engine.forks.len(),
        0,
        "Invalid fork block must not be buffered in fork registry"
    );
}

#[tokio::test]
async fn test_genesis_initialization_succeeds() {
    let consts = ConsensusConsts::default();
    let (_, alice_addr) = keypair_from_seed(1);
    let (genesis, db) = make_test_genesis(&[(alice_addr, 50_000)], consts);
    assert!(genesis.header.is_genesis());
    assert!(validate_block_structure(&genesis).is_ok());

    let engine = make_engine(db, consts).await;
    let tip = engine.tip();
    assert_eq!(tip, Some((0, genesis.block_hash(), 1)));
    let alice_utxos = engine
        .db
        .read()
        .await
        .get_utxos_for_address(alice_addr)
        .await
        .unwrap();
    assert_eq!(alice_utxos.len(), 1);
    assert_eq!(alice_utxos.values().next().unwrap().value, 50_000);
}
