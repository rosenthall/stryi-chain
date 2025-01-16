//! tests/transactions.rs
//!
//! Integration tests for transaction logic. 

use std::collections::HashMap;

use p256::ecdsa::{SigningKey, VerifyingKey};
use p256::elliptic_curve::rand_core::OsRng;

use crate::error::StryiCoreError;
use crate::transactions::{
    Transaction, TransactionData, TransactionIn, TransactionOut, OutPoint, UTXO, TransactionHash
};
use crate::address::AccountAddress;

/// A simple, local UTXO set for testing purposes.
/// The key is `(TransactionHash, vout)`, and the value is the `UTXO`.
#[derive(Debug, Default)]
struct LocalUtxoSet {
    map: HashMap<(TransactionHash, u32), UTXO>,
}

impl LocalUtxoSet {
    fn add_utxo(&mut self, utxo: UTXO) {
        let key = (utxo.txid, utxo.vout);
        self.map.insert(key, utxo);
    }

    fn remove_utxo(&mut self, txid: &TransactionHash, vout: u32) {
        self.map.remove(&(*txid, vout));
    }

    fn get_utxo(&self, txid: &TransactionHash, vout: u32) -> Option<&UTXO> {
        self.map.get(&(*txid, vout))
    }

    fn sum_utxos_for_address(&self, address: &AccountAddress) -> u64 {
        self.map
            .values()
            .filter(|u| &u.owner == address)
            .map(|u| u.value)
            .sum()
    }
}

/// Validate a transaction with the following logic:
/// 1) Verify the **global** transaction signature (with `verifying_key`).
/// 2) For each input, confirm the UTXO exists and is owned by `signer_address`.
/// 3) Ensure sum(inputs) >= sum(outputs).
///
/// Returns `Ok(())` on success, or `Err(StryiCoreError)` on failure.
fn validate_transaction(
    tx: &Transaction,
    utxo_set: &LocalUtxoSet,
    verifying_key: &VerifyingKey,
    signer_address: &AccountAddress,
) -> Result<(), StryiCoreError> {
    // 1) Check the overall transaction signature
    if !tx.verify(verifying_key) {
        return Err(StryiCoreError::TxInvalidSignature);
    }

    let mut total_input_value = 0u64;
    for input in &tx.data.inputs {
        let outpoint = &input.previous_output;

        let maybe_utxo = utxo_set.get_utxo(&outpoint.txid, outpoint.vout);
        let utxo = match maybe_utxo {
            Some(u) => u,
            None => {
                return Err(StryiCoreError::TxMissingUtxo {
                    txid: outpoint.txid,
                    vout: outpoint.vout,
                });
            }
        };

        // Check ownership: all inputs must belong to the same `signer_address`
        if utxo.owner != *signer_address {
            return Err(StryiCoreError::TxWrongOwner {
                expected: utxo.owner.clone(),
                actual: signer_address.clone(),
            });
        }

        total_input_value = total_input_value.saturating_add(utxo.value);
    }

    // Sum outputs
    let total_output_value: u64 = tx.data.outputs.iter().map(|out| out.value).sum();
    if total_input_value < total_output_value {
        return Err(StryiCoreError::TxInsufficientInputValue {
            input_sum: total_input_value,
            output_sum: total_output_value,
        });
    }

    Ok(())
}

/// Apply a transaction to our local UTXO set:
/// - Remove each input's UTXO
/// - Create new UTXOs for each output
fn apply_transaction(tx: &Transaction, utxo_set: &mut LocalUtxoSet) {
    let tx_hash = tx.data.hash();

    // Spend (remove) each input
    for input in &tx.data.inputs {
        let outpoint = &input.previous_output;
        utxo_set.remove_utxo(&outpoint.txid, outpoint.vout);
    }

    // Create new outputs
    for (idx, out) in tx.data.outputs.iter().enumerate() {
        let new_utxo = UTXO {
            txid: tx_hash,
            vout: idx as u32,
            value: out.value,
            owner: out.recipient.clone(),
        };
        utxo_set.add_utxo(new_utxo);
    }
}

/// Demonstrates a normal transaction flow:
/// 1) Alice has a genesis UTXO (1000 coins).
/// 2) Alice -> Bob (600 coins).
/// 3) Bob -> Alice (300 coins).
#[test]
fn test_valid_transactions_flow() {
    // 1) Generate key pairs (Alice, Bob)
    let signing_key_alice = SigningKey::random(&mut OsRng);
    let verifying_key_alice = VerifyingKey::from(&signing_key_alice);
    let address_alice = AccountAddress::from_public_key(verifying_key_alice);

    let signing_key_bob = SigningKey::random(&mut OsRng);
    let verifying_key_bob = VerifyingKey::from(&signing_key_bob);
    let address_bob = AccountAddress::from_public_key(verifying_key_bob);

    // 2) Create a local UTXO set with a genesis UTXO for Alice (1000 coins)
    let mut utxo_set = LocalUtxoSet::default();

    let genesis_bytes = [1u8; 32];
    let genesis_hash = TransactionHash::try_from(genesis_bytes.as_slice())
        .expect("Failed to create TransactionHash from genesis bytes");

    let genesis_utxo = UTXO {
        txid: genesis_hash,
        vout: 0,
        value: 1000,
        owner: address_alice.clone(),
    };
    utxo_set.add_utxo(genesis_utxo);

    // 3) Alice -> Bob transaction (600 coins)
    let tx_data_alice_to_bob = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: genesis_hash,
                vout: 0,
            },
            signature: vec![],
            sequence: 0xFFFFFFFF,
        }],
        outputs: vec![TransactionOut {
            value: 600,
            recipient: address_bob,
        }],
    };
    let tx_alice_to_bob = tx_data_alice_to_bob.sign(&signing_key_alice);

    // Validate + apply
    validate_transaction(
        &tx_alice_to_bob,
        &utxo_set,
        &verifying_key_alice,
        &address_alice,
    )
        .expect("Alice->Bob tx should be valid");
    apply_transaction(&tx_alice_to_bob, &mut utxo_set);

    // 4) Bob -> Alice (300 coins)
    let bob_utxo_txid = tx_alice_to_bob.data.hash(); // Bob's new UTXO is at index 0
    let tx_data_bob_to_alice = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: bob_utxo_txid,
                vout: 0,
            },
            signature: vec![],
            sequence: 0xFFFFFFFF,
        }],
        outputs: vec![TransactionOut {
            value: 300,
            recipient: address_alice,
        }],
    };
    let tx_bob_to_alice = tx_data_bob_to_alice.sign(&signing_key_bob);

    // Validate + apply
    validate_transaction(
        &tx_bob_to_alice,
        &utxo_set,
        &verifying_key_bob,
        &address_bob,
    )
        .expect("Bob->Alice tx should be valid");
    apply_transaction(&tx_bob_to_alice, &mut utxo_set);

    // Done! (We could print or assert final balances, etc.)
    println!("Final UTXO set = {:#?}", utxo_set.map);
}

/// Tests negative scenarios: missing UTXOs, overspending, and tampering.
#[test]
fn test_negative_scenarios() {
    let signing_key_alice = SigningKey::random(&mut OsRng);
    let verifying_key_alice = VerifyingKey::from(&signing_key_alice);
    let address_alice = AccountAddress::from_public_key(verifying_key_alice);

    let mut utxo_set = LocalUtxoSet::default();

    // 1) Reference a non-existent UTXO
    let fake_txid = TransactionHash::try_from([9u8; 32].as_slice()).unwrap();

    let bad_tx_data = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: fake_txid,
                vout: 0,
            },
            signature: vec![],
            sequence: 0,
        }],
        outputs: vec![TransactionOut {
            value: 100,
            recipient: address_alice,
        }],
    };
    let bad_tx = bad_tx_data.sign(&signing_key_alice);

    // Should fail with TxMissingUtxo
    let err = validate_transaction(&bad_tx, &utxo_set, &verifying_key_alice, &address_alice)
        .expect_err("Missing UTXO should fail validation");
    match err {
        StryiCoreError::TxMissingUtxo { txid, vout } => {
            println!("Correctly failed for missing UTXO: ({:?}, {})", txid, vout);
        }
        _ => panic!("Expected TxMissingUtxo, got {:?}", err),
    }

    // 2) Put a valid UTXO but overspend
    let good_txid = TransactionHash::try_from([5u8; 32].as_slice()).unwrap();
    let good_utxo = UTXO {
        txid: good_txid,
        vout: 0,
        value: 50,
        owner: address_alice.clone(),
    };
    utxo_set.add_utxo(good_utxo);

    let overspend_data = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: good_txid,
                vout: 0,
            },
            signature: vec![],
            sequence: 0,
        }],
        // Trying to spend 50 but output is 100
        outputs: vec![TransactionOut {
            value: 100,
            recipient: address_alice,
        }],
    };
    let overspend_tx = overspend_data.sign(&signing_key_alice);

    let err = validate_transaction(&overspend_tx, &utxo_set, &verifying_key_alice, &address_alice)
        .expect_err("Overspending should fail");
    match err {
        StryiCoreError::TxInsufficientInputValue { input_sum, output_sum } => {
            println!(
                "Correctly failed overspend: input={}, output={}",
                input_sum, output_sum
            );
        }
        _ => panic!("Expected TxInsufficientInputValue, got {:?}", err),
    }

    // 3) Tampering after signing
    //   Create a valid tx (spend 50 exactly)
    let valid_data = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: good_txid,
                vout: 0,
            },
            signature: vec![],
            sequence: 0,
        }],
        outputs: vec![TransactionOut {
            value: 50,
            recipient: address_alice,
        }],
    };
    let mut valid_tx = valid_data.sign(&signing_key_alice);

    // Tamper with the output (change it to 9999) after signing
    valid_tx.data.outputs[0].value = 9999;

    let err = validate_transaction(&valid_tx, &utxo_set, &verifying_key_alice, &address_alice)
        .expect_err("Tampering breaks the signature");
    match err {
        StryiCoreError::TxInvalidSignature => {
            println!("Correctly failed after tampering the output post-signature");
        }
        _ => panic!("Expected TxInvalidSignature, got {:?}", err),
    }
}
