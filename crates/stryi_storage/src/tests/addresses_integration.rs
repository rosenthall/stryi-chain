use tempfile::TempDir;
use tokio::test;

use stryi_core::{
    address::AccountAddress,
    storage::UtxoStorage,
    transactions::{OutPoint, TransactionHash, UTXO},
};

use crate::{GenesisInitConfig, StryiStorage, StryiStorageError};

/// Vary the txid bytes so each fixture outpoint stays distinct.
fn make_test_outpoint(txid_first_byte: u8, vout: u32) -> OutPoint {
    let mut txid_arr = [0u8; 32];
    txid_arr[0] = txid_first_byte;

    for (i, byte) in txid_arr.iter_mut().enumerate() {
        *byte = txid_first_byte.wrapping_add(i as u8);
    }
    let txid = TransactionHash::new(&txid_arr);
    OutPoint { txid, vout }
}

fn create_test_utxo(op: &OutPoint, owner: AccountAddress) -> UTXO {
    UTXO {
        txid: op.txid,
        vout: op.vout,
        value: 123_456,
        owner,
    }
}

#[test]
async fn test_addresses_integration() -> Result<(), StryiStorageError> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let genesis_config = GenesisInitConfig::new_test();
    let mut storage =
        StryiStorage::initialize_in_path(temp_dir.path().to_owned(), Some(genesis_config)).await?;

    let alice_addr = AccountAddress::new(&[0xA1; 20]);
    let bob_addr = AccountAddress::new(&[0xB2; 20]);
    let charlie_addr = AccountAddress::new(&[0xC3; 20]);

    let alice_op1 = make_test_outpoint(1, 0);
    let alice_op2 = make_test_outpoint(1, 1);
    let alice_op3 = make_test_outpoint(1, 2);

    let bob_op1 = make_test_outpoint(2, 10);
    let bob_op2 = make_test_outpoint(2, 11);

    let charlie_op1 = make_test_outpoint(3, 20);

    storage
        .put_utxo(alice_op1, create_test_utxo(&alice_op1, alice_addr))
        .await?;
    let alice_uts = storage.get_utxos_for_address(alice_addr).await?;
    assert_eq!(
        alice_uts.len(),
        1,
        "Alice should have one UTXO after the first insert"
    );
    assert!(
        storage.get_utxos_for_address(bob_addr).await?.is_empty(),
        "Bob should still be empty"
    );
    assert!(
        storage
            .get_utxos_for_address(charlie_addr)
            .await?
            .is_empty(),
        "Charlie should still be empty"
    );

    storage
        .put_utxo(bob_op1, create_test_utxo(&bob_op1, bob_addr))
        .await?;
    let bob_uts = storage.get_utxos_for_address(bob_addr).await?;
    assert_eq!(
        bob_uts.len(),
        1,
        "Bob should have one UTXO after the first insert"
    );
    assert_eq!(storage.get_utxos_for_address(alice_addr).await?.len(), 1);

    // Single and batch writes should land in the same address view.
    let batch_put = vec![
        (alice_op2, create_test_utxo(&alice_op2, alice_addr)),
        (alice_op3, create_test_utxo(&alice_op3, alice_addr)),
        (bob_op2, create_test_utxo(&bob_op2, bob_addr)),
        (charlie_op1, create_test_utxo(&charlie_op1, charlie_addr)),
    ];
    storage.batch_put_utxos(batch_put).await?;

    let alice_count = storage.get_utxos_for_address(alice_addr).await?.len();
    assert_eq!(
        alice_count, 3,
        "Alice should have three UTXOs after the batch insert"
    );

    let bob_count = storage.get_utxos_for_address(bob_addr).await?.len();
    assert_eq!(
        bob_count, 2,
        "Bob should have two UTXOs after the batch insert"
    );

    let charlie_count = storage.get_utxos_for_address(charlie_addr).await?.len();
    assert_eq!(
        charlie_count, 1,
        "Charlie should have one UTXO after the batch insert"
    );

    storage.remove_utxo(bob_op2).await?;
    let bob_after = storage.get_utxos_for_address(bob_addr).await?;
    assert_eq!(
        bob_after.len(),
        1,
        "Bob should have one UTXO after removing bob_op2"
    );
    let bob_op2_check = storage.get_utxo(bob_op2).await;
    assert!(
        bob_op2_check.is_err(),
        "removed Bob outpoint should stay absent"
    );

    storage
        .batch_remove_utxos(vec![alice_op2, alice_op3])
        .await?;
    let alice_after = storage.get_utxos_for_address(alice_addr).await?;
    assert_eq!(
        alice_after.len(),
        1,
        "batch remove should leave only Alice's first outpoint"
    );
    let only_op = &alice_after
        .get(&alice_op1)
        .expect("Alice's outpoint 1 must exist at this point");
    assert_eq!(only_op.vout, alice_op1.vout);
    assert_eq!(only_op.txid.data, alice_op1.txid.data);

    let fake_op = make_test_outpoint(99, 999);
    let rem_result = storage.remove_utxo(fake_op).await;
    assert!(
        rem_result.is_err(),
        "removing a missing outpoint should fail"
    );

    // Failed removals should not disturb address buckets.
    assert_eq!(
        storage.get_utxos_for_address(alice_addr).await?.len(),
        1,
        "Alice should stay unchanged after a failed remove"
    );
    assert_eq!(
        storage.get_utxos_for_address(bob_addr).await?.len(),
        1,
        "Bob should stay unchanged after a failed remove"
    );
    assert_eq!(
        storage.get_utxos_for_address(charlie_addr).await?.len(),
        1,
        "Charlie should stay unchanged after a failed remove"
    );

    for op in [&alice_op1, &bob_op1, &charlie_op1] {
        let res = storage.get_utxo(*op).await;
        assert!(
            res.is_ok(),
            "surviving outpoint should remain in the database"
        );
    }
    for removed_op in [&bob_op2, &alice_op2, &alice_op3] {
        let res = storage.get_utxo(*removed_op).await;
        assert!(res.is_err(), "removed outpoint should not be found");
    }

    Ok(())
}
