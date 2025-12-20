use tempfile::TempDir;
use tokio::test;

use stryi_core::{
    address::AccountAddress,
    storage::UtxoStorage,
    transactions::{OutPoint, TransactionHash, UTXO},
};

use crate::{GenesisInitConfig, StryiStorage, StryiStorageError};

/// Creates an `OutPoint` by combining a specific first byte in its txid plus
/// the provided `vout`. This ensures each outpoint is unique for testing.
fn make_test_outpoint(txid_first_byte: u8, vout: u32) -> OutPoint {
    let mut txid_arr = [0u8; 32];
    txid_arr[0] = txid_first_byte;
    for i in 1..32 {
        txid_arr[i] = txid_first_byte.wrapping_add(i as u8);
    }
    let txid = TransactionHash::new(&txid_arr);
    OutPoint { txid, vout }
}

/// Creates a `UTXO` with a fixed value, owned by `owner`.
fn create_test_utxo(op: &OutPoint, owner: AccountAddress) -> UTXO {
    UTXO {
        txid: op.txid,
        vout: op.vout,
        value: 123_456,
        owner,
    }
}

/// Demonstrates how single and batch put/remove operations affect multiple addresses.
/// Logs details about each step of the process for clarity.
#[test]
async fn test_addresses_integration() -> Result<(), StryiStorageError> {
    println!("=== Starting addresses integration test ===");

    // 1) Create a temp directory and initialize StryiStorage with default genesis config

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let genesis_config = GenesisInitConfig::new_test();
    let mut storage =
        StryiStorage::initialize_in_path(temp_dir.path().to_owned(), Some(genesis_config)).await?;
    println!("Initialized StryiStorage at: {:?}", temp_dir.path());

    // 2) Create distinct addresses
    let alice_addr = AccountAddress::new(&[0xA1; 20]);
    let bob_addr = AccountAddress::new(&[0xB2; 20]);
    let charlie_addr = AccountAddress::new(&[0xC3; 20]);

    println!("Created addresses for Alice, Bob, and Charlie");

    // 3) Prepare outpoints
    let alice_op1 = make_test_outpoint(1, 0);
    let alice_op2 = make_test_outpoint(1, 1);
    let alice_op3 = make_test_outpoint(1, 2);

    let bob_op1 = make_test_outpoint(2, 10);
    let bob_op2 = make_test_outpoint(2, 11);

    let charlie_op1 = make_test_outpoint(3, 20);

    println!("Prepared outpoints for Alice, Bob, and Charlie");

    // ------------------
    // Step A: Single put for Alice
    // ------------------
    println!("Step A: Single put for Alice (alice_op1)");
    storage
        .put_utxo(alice_op1, create_test_utxo(&alice_op1, alice_addr))
        .await?;
    let alice_uts = storage.get_utxos_for_address(alice_addr).await?;
    println!("After put, Alice has {} UTXOs", alice_uts.len());
    assert_eq!(
        alice_uts.len(),
        1,
        "Alice should have 1 UTXO after first put"
    );
    assert!(
        storage.get_utxos_for_address(bob_addr).await?.is_empty(),
        "Bob is empty initially"
    );
    assert!(
        storage
            .get_utxos_for_address(charlie_addr)
            .await?
            .is_empty(),
        "Charlie is empty initially"
    );

    // ------------------
    // Step B: Single put for Bob
    // ------------------
    println!("Step B: Single put for Bob (bob_op1)");
    storage
        .put_utxo(bob_op1, create_test_utxo(&bob_op1, bob_addr))
        .await?;
    let bob_uts = storage.get_utxos_for_address(bob_addr).await?;
    println!("After put, Bob has {} UTXOs", bob_uts.len());
    assert_eq!(bob_uts.len(), 1, "Bob should have 1 UTXO after first put");
    // Alice's set remains 1
    assert_eq!(storage.get_utxos_for_address(alice_addr).await?.len(), 1);

    // ------------------
    // Step C: Batch put for more UTXOs
    // ------------------
    println!("Step C: Batch put for Alice (op2, op3), Bob (op2), Charlie (op1)");
    let batch_put = vec![
        (alice_op2, create_test_utxo(&alice_op2, alice_addr)),
        (alice_op3, create_test_utxo(&alice_op3, alice_addr)),
        (bob_op2, create_test_utxo(&bob_op2, bob_addr)),
        (charlie_op1, create_test_utxo(&charlie_op1, charlie_addr)),
    ];
    storage.batch_put_utxos(batch_put).await?;

    // Verify
    let alice_count = storage.get_utxos_for_address(alice_addr).await?.len();
    println!("After batch put, Alice has {} UTXOs", alice_count);
    assert_eq!(alice_count, 3, "Alice now has 3 UTXOs total");

    let bob_count = storage.get_utxos_for_address(bob_addr).await?.len();
    println!("Bob has {} UTXOs now", bob_count);
    assert_eq!(bob_count, 2, "Bob now has 2 UTXOs total");

    let charlie_count = storage.get_utxos_for_address(charlie_addr).await?.len();
    println!("Charlie has {} UTXOs now", charlie_count);
    assert_eq!(charlie_count, 1, "Charlie now has 1 UTXO total");

    // ------------------
    // Step D: Single remove for Bob (bob_op2)
    // ------------------
    println!("Step D: Removing bob_op2 singly");
    storage.remove_utxo(bob_op2).await?;
    let bob_after = storage.get_utxos_for_address(bob_addr).await?;
    println!("Bob has {} UTXOs after removing bob_op2", bob_after.len());
    assert_eq!(
        bob_after.len(),
        1,
        "Bob should have 1 left after removing bob_op2"
    );
    // Confirm bob_op2 is gone
    let bob_op2_check = storage.get_utxo(bob_op2).await;
    assert!(bob_op2_check.is_err(), "bob_op2 not found after removal");

    // ------------------
    // Step E: Batch remove for Alice (alice_op2, alice_op3)
    // ------------------
    println!("Step E: Batch removing alice_op2 and alice_op3");
    storage
        .batch_remove_utxos(vec![alice_op2, alice_op3])
        .await?;
    let alice_after = storage.get_utxos_for_address(alice_addr).await?;
    println!(
        "Alice has {} UTXOs after removing op2 and op3",
        alice_after.len()
    );
    assert_eq!(alice_after.len(), 1, "Alice should be back to 1 UTXO");
    // The only remaining is alice_op1
    let only_op = &alice_after
        .get(&alice_op1)
        .expect("Alice's outpoint 1 must exist at this point");
    assert_eq!(only_op.vout, alice_op1.vout);
    assert_eq!(only_op.txid.data, alice_op1.txid.data);

    // ------------------
    // Step F: Removing a nonexistent outpoint
    // ------------------
    println!("Step F: Attempt removing a nonexistent outpoint");
    let fake_op = make_test_outpoint(99, 999);
    let rem_result = storage.remove_utxo(fake_op).await;
    assert!(
        rem_result.is_err(),
        "Removing nonexistent outpoint should fail"
    );

    // Confirm no partial changes
    assert_eq!(
        storage.get_utxos_for_address(alice_addr).await?.len(),
        1,
        "Alice remains stable"
    );
    assert_eq!(
        storage.get_utxos_for_address(bob_addr).await?.len(),
        1,
        "Bob remains stable"
    );
    assert_eq!(
        storage.get_utxos_for_address(charlie_addr).await?.len(),
        1,
        "Charlie remains stable"
    );

    // ------------------
    // Step G: Final verification
    // ------------------
    println!("Step G: Final verification of all addresses");
    for op in [&alice_op1, &bob_op1, &charlie_op1] {
        let res = storage.get_utxo(*op).await;
        assert!(res.is_ok(), "Outpoint should remain in DB");
    }
    for removed_op in [&bob_op2, &alice_op2, &alice_op3] {
        let res = storage.get_utxo(*removed_op).await;
        assert!(res.is_err(), "Removed outpoint should not be found");
    }

    println!("=== Test addresses_integration completed successfully ===");
    Ok(())
}
