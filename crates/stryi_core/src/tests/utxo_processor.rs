use crate::consensus::ConsensusConsts;
use crate::storage::{StorageReader, UtxoStorage};
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

#[test]
fn test_apply_block_creates_utxos_and_returns_undo() {
    let consts = ConsensusConsts::default();
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (genesis, db) = make_test_genesis(&[(alice_addr, 50_000)], consts);

    let coinbase = make_coinbase_tx(
        &miner_key,
        consts.block_subsidy(1),
        miner_addr,
        1,
        genesis.block_hash(),
    );
    let payment = make_payment_tx(
        &alice_key,
        vec![genesis_outpoint(&genesis)],
        vec![(40_000, miner_addr)],
    );

    let block = make_block_mined(
        vec![coinbase.clone(), payment.clone()],
        1,
        consts.pow.initial_hash_threshold,
        genesis.block_hash(),
    );

    let mut db = db.read().unwrap();
    let processor = UtxoProcessor::new();
    let undo = processor.apply_block(&block, &mut db).unwrap();

    // coinbase created 1 output, payment created 1 output = 2 created outpoints
    assert_eq!(undo.created_outpoints.len(), 2);

    // payment spent 1 genesis UTXO
    assert_eq!(undo.spent_utxos.len(), 1);

    // the new coinbase output should exist in storage
    let coinbase_op = OutPoint {
        txid: coinbase.data.hash(),
        vout: 0,
    };
    let coinbase_utxo = db.get_utxo(coinbase_op).unwrap();
    assert!(coinbase_utxo.is_some());
    assert_eq!(coinbase_utxo.unwrap().value, consts.block_subsidy(1));

    // the new payment output should exist in storage
    let payment_op = OutPoint {
        txid: payment.data.hash(),
        vout: 0,
    };
    let payment_utxo = db.get_utxo(payment_op).unwrap();
    assert!(payment_utxo.is_some());
    assert_eq!(payment_utxo.unwrap().value, 40_000);
}

#[test]
fn test_rewind_block_restores_previous_state() {
    let consts = ConsensusConsts::default();
    let (miner_key, miner_addr) = keypair_from_seed(0);
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (genesis, db) = make_test_genesis(&[(alice_addr, 50_000)], consts);

    let alice_op = genesis_outpoint(&genesis);

    let coinbase = make_coinbase_tx(
        &miner_key,
        consts.block_subsidy(1),
        miner_addr,
        1,
        genesis.block_hash(),
    );
    let payment = make_payment_tx(&alice_key, vec![alice_op], vec![(40_000, miner_addr)]);
    let payment_op = OutPoint {
        txid: payment.data.hash(),
        vout: 0,
    };
    let child = make_payment_tx(&miner_key, vec![payment_op], vec![(30_000, alice_addr)]);

    let block = make_block_mined(
        vec![coinbase.clone(), payment.clone(), child.clone()],
        1,
        consts.pow.initial_hash_threshold,
        genesis.block_hash(),
    );

    let mut db = db.read().unwrap();
    let processor = UtxoProcessor::new();
    let undo = processor.apply_block(&block, &mut db).unwrap();

    // Outputs consumed within the block must not be restored by its undo.
    assert_eq!(undo.created_outpoints.len(), 2);
    assert_eq!(undo.spent_utxos.len(), 1);
    assert!(db.get_utxo(alice_op).unwrap().is_none());
    assert!(db.get_utxo(payment_op).unwrap().is_none());

    // rewind
    processor.rewind_block(undo, &mut db).unwrap();

    // alice's UTXO should be restored
    let restored = db.get_utxo(alice_op).unwrap();
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
        db.get_utxo(coinbase_op).unwrap().is_none(),
        "Coinbase output should be removed after rewind"
    );

    assert!(
        db.get_utxo(payment_op).unwrap().is_none(),
        "Payment output should be removed after rewind"
    );

    let child_op = OutPoint {
        txid: child.data.hash(),
        vout: 0,
    };
    assert!(db.get_utxo(child_op).unwrap().is_none());
}
