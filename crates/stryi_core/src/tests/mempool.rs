use crate::block::BlockData;
use crate::consensus::ConsensusConsts;
use crate::mempool::{MemPool, MemPoolConfig, MemPoolError, MempoolValidationError};
use crate::tests::{keypair_from_seed, make_payment_tx, make_test_genesis, make_utxo_lookup};
use crate::transactions::{OutPoint, UTXO};

/// Builds a mempool with default config and a lookup backed by given UTXOs.
fn setup_mempool(utxos: Vec<(OutPoint, UTXO)>) -> MemPool {
    let lookup = make_utxo_lookup(utxos);
    MemPool::new(MemPoolConfig::default(), lookup)
}

/// Creates a genesis block and returns (outpoint, utxo) for Alice's balance.
fn alice_funded(balance: u64) -> (OutPoint, UTXO, crate::block::Block) {
    let (_, alice_addr) = keypair_from_seed(1);
    let consts = ConsensusConsts::default();
    let (genesis, _db) = make_test_genesis(&[(alice_addr, balance)], consts);

    let tx = &genesis.data.transactions[0];
    let txid = tx.data.hash();
    let op = OutPoint { txid, vout: 0 };
    let utxo = UTXO {
        txid,
        vout: 0,
        value: balance,
        owner: alice_addr,
    };
    (op, utxo, genesis)
}

#[tokio::test]
async fn test_add_transaction_rejects_wrong_owner() {
    let (_, _alice_addr) = keypair_from_seed(1);
    let (bob_key, bob_addr) = keypair_from_seed(2);
    let (op, utxo, _) = alice_funded(100_000);

    let mut pool = setup_mempool(vec![(op, utxo)]);

    // Bob tries to spend Alice's UTXO
    let tx = make_payment_tx(&bob_key, vec![op], vec![(50_000, bob_addr)]);
    let err = pool.add_transaction(tx).await.unwrap_err();

    match err {
        MemPoolError::ValidationError(MempoolValidationError::OwnershipMismatch {
            expected,
            actual,
            ..
        }) => {
            // In mempool validation: expected = recovered_addr (signer = Bob),
            // actual = utxo.owner (Alice)
            assert_eq!(expected, bob_addr);
            assert_eq!(actual, utxo.owner);
        }
        other => panic!("Expected ValidationError(OwnershipMismatch), got: {other:?}"),
    }
}

#[tokio::test]
async fn test_remove_transaction_cascades_to_dependents() {
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (_, bob_addr) = keypair_from_seed(2);
    let (op, utxo, _) = alice_funded(100_000);

    let mut pool = setup_mempool(vec![(op, utxo)]);

    // tx1: alice sends 90k to bob
    let tx1 = make_payment_tx(&alice_key, vec![op], vec![(90_000, bob_addr)]);
    let tx1_hash = tx1.data.hash();
    pool.add_transaction(tx1).await.unwrap();

    // tx2: bob spends tx1's output (depends on tx1 being in mempool)
    let (bob_key, _) = keypair_from_seed(2);
    let tx1_outpoint = OutPoint {
        txid: tx1_hash,
        vout: 0,
    };
    let tx2 = make_payment_tx(&bob_key, vec![tx1_outpoint], vec![(80_000, alice_addr)]);
    let tx2_hash = tx2.data.hash();
    pool.add_transaction(tx2).await.unwrap();

    // tx3: alice spends tx2's output (depends on tx2)
    let tx2_outpoint = OutPoint {
        txid: tx2_hash,
        vout: 0,
    };
    let tx3 = make_payment_tx(&alice_key, vec![tx2_outpoint], vec![(70_000, bob_addr)]);
    pool.add_transaction(tx3).await.unwrap();

    assert_eq!(pool.transaction_count(), 3);

    pool.remove_transaction(tx1_hash);
    assert_eq!(pool.transaction_count(), 0);
}

#[tokio::test]
async fn test_get_best_transactions_respects_dependency_order() {
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (_, bob_addr) = keypair_from_seed(2);
    let (op, utxo, _) = alice_funded(100_000);

    let mut pool = setup_mempool(vec![(op, utxo)]);

    // chain: tx1 (spends genesis) then tx2 (spends tx1) then tx3 (spends tx2)
    let tx1 = make_payment_tx(&alice_key, vec![op], vec![(90_000, bob_addr)]);
    let tx1_hash = tx1.data.hash();
    pool.add_transaction(tx1).await.unwrap();

    let (bob_key, _) = keypair_from_seed(2);
    let tx2 = make_payment_tx(
        &bob_key,
        vec![OutPoint {
            txid: tx1_hash,
            vout: 0,
        }],
        vec![(80_000, alice_addr)],
    );
    let tx2_hash = tx2.data.hash();
    pool.add_transaction(tx2).await.unwrap();

    let tx3 = make_payment_tx(
        &alice_key,
        vec![OutPoint {
            txid: tx2_hash,
            vout: 0,
        }],
        vec![(70_000, bob_addr)],
    );
    let tx3_hash = tx3.data.hash();
    pool.add_transaction(tx3).await.unwrap();

    let best = pool.get_best_transactions(10);
    assert_eq!(best.len(), 3);

    let pos1 = best.iter().position(|t| t.data.hash() == tx1_hash).unwrap();
    let pos2 = best.iter().position(|t| t.data.hash() == tx2_hash).unwrap();
    let pos3 = best.iter().position(|t| t.data.hash() == tx3_hash).unwrap();

    assert!(pos1 < pos2, "tx1 must appear before tx2");
    assert!(pos2 < pos3, "tx2 must appear before tx3");
}

#[tokio::test]
async fn test_update_on_block_removes_confirmed_transactions() {
    let (alice_key, _) = keypair_from_seed(1);
    let (_, bob_addr) = keypair_from_seed(2);
    let (op, utxo, _) = alice_funded(100_000);

    let mut pool = setup_mempool(vec![(op, utxo)]);

    let tx = make_payment_tx(&alice_key, vec![op], vec![(90_000, bob_addr)]);
    pool.add_transaction(tx.clone()).await.unwrap();
    assert_eq!(pool.transaction_count(), 1);

    let block_data = BlockData {
        transactions: vec![tx],
    };
    pool.update_on_block(block_data);
    assert_eq!(pool.transaction_count(), 0);
}

#[tokio::test]
async fn test_update_on_block_keeps_mempool_children() {
    let (alice_key, alice_addr) = keypair_from_seed(1);
    let (_, bob_addr) = keypair_from_seed(2);
    let (op, utxo, _) = alice_funded(100_000);

    let mut pool = setup_mempool(vec![(op, utxo)]);

    let parent = make_payment_tx(&alice_key, vec![op], vec![(90_000, bob_addr)]);
    let parent_hash = parent.data.hash();
    pool.add_transaction(parent.clone()).await.unwrap();

    let (bob_key, _) = keypair_from_seed(2);
    let child = make_payment_tx(
        &bob_key,
        vec![OutPoint {
            txid: parent_hash,
            vout: 0,
        }],
        vec![(80_000, alice_addr)],
    );
    let child_hash = child.data.hash();
    pool.add_transaction(child).await.unwrap();

    pool.update_on_block(BlockData {
        transactions: vec![parent],
    });

    assert_eq!(pool.transaction_count(), 1);

    let best = pool.get_best_transactions(10);
    assert_eq!(best.len(), 1);
    assert_eq!(best[0].data.hash(), child_hash);
}

#[tokio::test]
async fn test_rbf_replaces_transaction_with_higher_fee() {
    let (alice_key, _) = keypair_from_seed(1);
    let (_, bob_addr) = keypair_from_seed(2);
    let (op, utxo, _) = alice_funded(100_000);

    let mut pool = setup_mempool(vec![(op, utxo)]);

    // tx1 has a low implicit fee (output close to input value)
    let tx1 = make_payment_tx(&alice_key, vec![op], vec![(99_000, bob_addr)]);
    pool.add_transaction(tx1).await.unwrap();
    assert_eq!(pool.transaction_count(), 1);

    // tx2 has the same input but with the much higher fee (output much lower than input value)
    let tx2 = make_payment_tx(&alice_key, vec![op], vec![(50_000, bob_addr)]);
    pool.add_transaction(tx2.clone()).await.unwrap();

    // tx2 should have replaced tx1
    assert_eq!(pool.transaction_count(), 1);

    let best = pool.get_best_transactions(10);
    assert_eq!(best.len(), 1);
    assert_eq!(best[0].data.hash(), tx2.data.hash());
}
