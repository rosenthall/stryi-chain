use crate::consensus::ConsensusConsts;
use crate::storage::{InMemoryStorageError, UtxoStorage};
use crate::tests::{
    keypair_from_seed, make_block_mined, make_coinbase_tx, make_payment_tx, make_test_genesis,
};
use crate::transactions::{OutPoint, UtxoProcessor};

/// Gets the genesis UTXO outpoint for the first output of genesis tx.
fn genesis_outpoint(genesis: &crate::block::Block) -> OutPoint {
    let tx = &genesis.data.transactions[0];
    OutPoint {
        txid: tx.data.hash(),
        vout: 0,
    }
}

/// Helper to check whether a UTXO exists in InMemoryStorage.
/// InMemoryStorage's batch_get_utxos returns Err(UtxoNotFound) for missing
/// outpoints, so get_utxo propagates that error instead of returning Ok(None).
/// This helper normalizes that behavior.
async fn utxo_exists(
    db: &crate::storage::StryiInMemoryStorage,
    op: OutPoint,
) -> Option<crate::transactions::UTXO> {
    match db.get_utxo(op).await {
        Ok(opt) => opt,
        Err(InMemoryStorageError::UtxoNotFound(_)) => None,
        Err(e) => panic!("Unexpected storage error: {e}"),
    }
}

#[tokio::test]
async fn test_apply_block_creates_utxos_and_returns_undo() {
    let consts = ConsensusConsts::default();
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (genesis, mut db) = make_test_genesis(&[(alice_addr, 50_000)], consts);

    let coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(1), miner_addr);
    let payment = make_payment_tx(
        &alice_key,
        vec![genesis_outpoint(&genesis)],
        vec![(40_000, miner_addr)],
    );

    let block = make_block_mined(
        vec![coinbase.clone(), payment.clone()],
        1,
        consts.difficulty_bits_for_height(1),
        genesis.block_hash(),
    );

    let processor = UtxoProcessor::new();
    let undo = processor.apply_block(&block, &mut db).await.unwrap();

    // coinbase created 1 output, payment created 1 output = 2 created outpoints
    assert_eq!(undo.created_outpoints.len(), 2);

    // payment spent 1 genesis UTXO
    assert_eq!(undo.spent_utxos.len(), 1);

    // the new coinbase output should exist in storage
    let coinbase_op = OutPoint {
        txid: coinbase.data.hash(),
        vout: 0,
    };
    let coinbase_utxo = utxo_exists(&db, coinbase_op).await;
    assert!(coinbase_utxo.is_some());
    assert_eq!(coinbase_utxo.unwrap().value, consts.block_subsidy(1));

    // the new payment output should exist in storage
    let payment_op = OutPoint {
        txid: payment.data.hash(),
        vout: 0,
    };
    let payment_utxo = utxo_exists(&db, payment_op).await;
    assert!(payment_utxo.is_some());
    assert_eq!(payment_utxo.unwrap().value, 40_000);
}

#[tokio::test]
async fn test_rewind_block_restores_previous_state() {
    let consts = ConsensusConsts::default();
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (genesis, mut db) = make_test_genesis(&[(alice_addr, 50_000)], consts);

    let alice_op = genesis_outpoint(&genesis);

    let coinbase = make_coinbase_tx(&miner_key, consts.block_subsidy(1), miner_addr);
    let payment = make_payment_tx(&alice_key, vec![alice_op], vec![(40_000, miner_addr)]);

    let block = make_block_mined(
        vec![coinbase.clone(), payment.clone()],
        1,
        consts.difficulty_bits_for_height(1),
        genesis.block_hash(),
    );

    let processor = UtxoProcessor::new();
    let undo = processor.apply_block(&block, &mut db).await.unwrap();

    // after apply: alice's UTXO gone, new outputs exist
    assert!(utxo_exists(&db, alice_op).await.is_none());

    // rewind
    processor.rewind_block(undo, &mut db).await.unwrap();

    // alice's UTXO should be restored
    let restored = utxo_exists(&db, alice_op).await;
    assert!(
        restored.is_some(),
        "Alice's UTXO should be restored after rewind"
    );
    assert_eq!(restored.unwrap().value, 50_000);

    // newly created outputs should be removed
    let coinbase_op = OutPoint {
        txid: coinbase.data.hash(),
        vout: 0,
    };
    assert!(
        utxo_exists(&db, coinbase_op).await.is_none(),
        "Coinbase output should be removed after rewind"
    );

    let payment_op = OutPoint {
        txid: payment.data.hash(),
        vout: 0,
    };
    assert!(
        utxo_exists(&db, payment_op).await.is_none(),
        "Payment output should be removed after rewind"
    );
}
