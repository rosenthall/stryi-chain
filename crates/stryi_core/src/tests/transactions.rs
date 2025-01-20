//! tests/transactions.rs
//!
//! Integration tests for transaction logic using K256 ECDSA.
//! This file tests various transaction scenarios, including valid flows and negative cases.

use std::collections::HashMap;
use k256::{
    ecdsa::{SigningKey, VerifyingKey},
    elliptic_curve::rand_core::OsRng,
};
use crate::error::StryiCoreError;
use crate::transactions::{
    Transaction, TransactionData, TransactionIn, TransactionOut, OutPoint, UTXO, TransactionHash,
};
use crate::address::AccountAddress;

/// Simple, local UTXO set for testing purposes.
/// Maps `(TransactionHash, vout)` to `UTXO`.
#[derive(Debug, Default)]
struct LocalUtxoSet {
    map: HashMap<(TransactionHash, u32), UTXO>,
}

impl LocalUtxoSet {
    /// Adds a UTXO to the set.
    fn add_utxo(&mut self, utxo: UTXO) {
        let key = (utxo.txid.clone(), utxo.vout);
        self.map.insert(key, utxo);
    }

    /// Removes a UTXO from the set by its transaction ID and output index.
    fn remove_utxo(&mut self, txid: &TransactionHash, vout: u32) {
        self.map.remove(&(txid.clone(), vout));
    }

    /// Retrieves a UTXO by its transaction ID and output index.
    fn get_utxo(&self, txid: &TransactionHash, vout: u32) -> Option<&UTXO> {
        self.map.get(&(txid.clone(), vout))
    }

    /// Sums the total value of UTXOs belonging to a given address.
    fn sum_utxos_for_address(&self, address: &AccountAddress) -> u64 {
        self.map
            .values()
            .filter(|u| &u.owner == address)
            .map(|u| u.value)
            .sum()
    }
}

/// Validates a transaction:
/// - Verifies the signature using the provided `VerifyingKey`.
/// - Checks that all referenced UTXOs exist and belong to the signer.
/// - Ensures the total input value is not less than the total output value.
fn validate_transaction(
    tx: &Transaction,
    utxo_set: &LocalUtxoSet,
    verifying_key: &VerifyingKey,
    signer_address: &AccountAddress,
) -> Result<(), StryiCoreError> {
    // Verify the transaction's signature.
    tx.verify_signature(verifying_key)?;

    let mut total_input_value = 0u64;
    for input in &tx.data.inputs {
        let outpoint = &input.previous_output;
        let utxo = utxo_set
            .get_utxo(&outpoint.txid, outpoint.vout)
            .ok_or(StryiCoreError::TxMissingUtxo {
                txid: outpoint.txid.clone(),
                vout: outpoint.vout,
            })?;

        // Ensure the signer owns the UTXO.
        if utxo.owner != *signer_address {
            return Err(StryiCoreError::TxWrongOwner {
                expected: utxo.owner.clone(),
                actual: signer_address.clone(),
            });
        }

        total_input_value = total_input_value.saturating_add(utxo.value);
    }

    let total_output_value: u64 = tx.data.outputs.iter().map(|out| out.value).sum();

    // Validate that inputs cover outputs.
    if total_input_value < total_output_value {
        return Err(StryiCoreError::TxInsufficientInputValue {
            input_sum: total_input_value,
            output_sum: total_output_value,
        });
    }

    Ok(())
}

/// Applies a valid transaction to the local UTXO set:
/// - Removes consumed UTXOs.
/// - Adds new UTXOs from the transaction outputs.
fn apply_transaction(tx: &Transaction, utxo_set: &mut LocalUtxoSet) {
    let tx_hash = tx.data.hash();

    // Remove all UTXOs referenced by inputs.
    for input in &tx.data.inputs {
        let outpoint = &input.previous_output;
        utxo_set.remove_utxo(&outpoint.txid, outpoint.vout);
    }

    // Create new UTXOs for each transaction output.
    for (idx, out) in tx.data.outputs.iter().enumerate() {
        let new_utxo = UTXO {
            txid: tx_hash.clone(),
            vout: idx as u32,
            value: out.value,
            owner: out.recipient.clone(),
        };
        utxo_set.add_utxo(new_utxo);
    }
}

/// Tests a valid transaction flow between two parties, Alice and Bob.
#[test]
fn test_valid_transactions_flow() {
    // Generate key pairs for Alice and Bob.
    let signing_key_alice = SigningKey::random(&mut OsRng);
    let verifying_key_alice = signing_key_alice.verifying_key();
    let address_alice = AccountAddress::new(&verifying_key_alice.to_sec1_bytes());

    let signing_key_bob = SigningKey::random(&mut OsRng);
    let verifying_key_bob = signing_key_bob.verifying_key();
    let address_bob = AccountAddress::new(&verifying_key_bob.to_sec1_bytes());

    // Initialize UTXO set with a genesis UTXO giving Alice 1000 coins.
    let mut utxo_set = LocalUtxoSet::default();
    let genesis_bytes = [1u8; 32];
    let genesis_hash = TransactionHash::try_from(genesis_bytes.as_slice())
        .expect("Failed to create TransactionHash from genesis bytes");
    let genesis_utxo = UTXO {
        txid: genesis_hash.clone(),
        vout: 0,
        value: 1000,
        owner: address_alice.clone(),
    };
    utxo_set.add_utxo(genesis_utxo);

    // Create a transaction: Alice sends 600 coins to Bob, 400 coins back to herself.
    let tx_data_alice_to_bob = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: genesis_hash.clone(),
                vout: 0,
            },
            // Placeholder signature; will be replaced by `sign`.
            sequence: 0xFFFFFFFF,
        }],
        outputs: vec![
            TransactionOut {
                value: 600,
                recipient: address_bob.clone(),
            },
            TransactionOut {
                value: 400,
                recipient: address_alice.clone(),
            },
        ],
    };
    let tx_alice_to_bob = tx_data_alice_to_bob.sign(&signing_key_alice);

    // Validate and apply Alice->Bob transaction.
    validate_transaction(
        &tx_alice_to_bob,
        &utxo_set,
        &verifying_key_alice,
        &address_alice,
    )
        .expect("Alice->Bob tx should be valid");
    apply_transaction(&tx_alice_to_bob, &mut utxo_set);

    // Create a transaction: Bob sends 300 coins back to Alice.
    let bob_utxo_txid = tx_alice_to_bob.data.hash();
    let tx_data_bob_to_alice = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: bob_utxo_txid.clone(),
                vout: 0, // Bob's received output index from previous transaction
            },
            sequence: 0xFFFFFFFF,
        }],
        outputs: vec![TransactionOut {
            value: 300,
            recipient: address_alice.clone(),
        }],
    };
    let tx_bob_to_alice = tx_data_bob_to_alice.sign(&signing_key_bob);

    // Validate and apply Bob->Alice transaction.
    validate_transaction(
        &tx_bob_to_alice,
        &utxo_set,
        &verifying_key_bob,
        &address_bob,
    )
        .expect("Bob->Alice tx should be valid");
    apply_transaction(&tx_bob_to_alice, &mut utxo_set);

    // Verify final balances.
    let alice_final = utxo_set.sum_utxos_for_address(&address_alice);
    let bob_final = utxo_set.sum_utxos_for_address(&address_bob);

    // Alice should have 400 (change) + 300 (from Bob) = 700, Bob should have 0.
    assert_eq!(alice_final, 700, "Alice should have 700 at the end");
    assert_eq!(bob_final, 0, "Bob should have 0 at the end");

    println!("Final UTXO set = {:#?}", utxo_set.map);
    println!("Alice final = {}, Bob final = {}", alice_final, bob_final);
}

/// Tests negative scenarios to ensure validation fails correctly:
/// 1. Referencing a non-existent UTXO.
/// 2. Overspending beyond available funds.
/// 3. Tampering with a transaction after signing.
#[test]
fn test_negative_scenarios() {
    let signing_key_alice = SigningKey::random(&mut OsRng);
    let verifying_key_alice = signing_key_alice.verifying_key();
    let address_alice = AccountAddress::new(&verifying_key_alice.to_sec1_bytes());

    let mut utxo_set = LocalUtxoSet::default();

    // 1) Reference a non-existent UTXO.
    let fake_txid = TransactionHash::try_from([9u8; 32].as_slice()).unwrap();
    let bad_tx_data = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: fake_txid.clone(),
                vout: 0,
            },
            sequence: 0,
        }],
        outputs: vec![TransactionOut {
            value: 100,
            recipient: address_alice.clone(),
        }],
    };
    let bad_tx = bad_tx_data.sign(&signing_key_alice);
    let err = validate_transaction(&bad_tx, &utxo_set, &verifying_key_alice, &address_alice)
        .expect_err("Missing UTXO should fail validation");
    match err {
        StryiCoreError::TxMissingUtxo { txid, vout } => {
            println!("Correctly failed for missing UTXO: ({:?}, {})", txid, vout);
        }
        _ => panic!("Expected TxMissingUtxo, got {:?}", err),
    }

    // 2) Overspending: Attempt to spend more than available.
    let good_txid = TransactionHash::try_from([5u8; 32].as_slice()).unwrap();
    let good_utxo = UTXO {
        txid: good_txid.clone(),
        vout: 0,
        value: 50,
        owner: address_alice.clone(),
    };
    utxo_set.add_utxo(good_utxo);

    let overspend_data = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: good_txid.clone(),
                vout: 0,
            },
            sequence: 0,
        }],
        outputs: vec![TransactionOut {
            value: 100, // more than available
            recipient: address_alice.clone(),
        }],
    };
    let overspend_tx = overspend_data.sign(&signing_key_alice);
    let err = validate_transaction(&overspend_tx, &utxo_set, &verifying_key_alice, &address_alice)
        .expect_err("Overspending should fail");
    match err {
        StryiCoreError::TxInsufficientInputValue { input_sum, output_sum } => {
            println!("Correctly failed overspend: input={}, output={}", input_sum, output_sum);
        }
        _ => panic!("Expected TxInsufficientInputValue, got {:?}", err),
    }

    // 3) Tampering after signing: modify output after the transaction is signed.
    let valid_data = TransactionData {
        version: 1,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: good_txid.clone(),
                vout: 0,
            },
            sequence: 0,
        }],
        outputs: vec![TransactionOut {
            value: 50,
            recipient: address_alice.clone(),
        }],
    };
    let mut valid_tx = valid_data.sign(&signing_key_alice);
    // Tampering: change the output value after signing.
    valid_tx.data.outputs[0].value = 9999;

    // Verify that tampering invalidates the signature.
    let err = valid_tx.verify_signature(&verifying_key_alice)
        .expect_err("Tampering breaks the signature");
    match err {
        StryiCoreError::InvalidSignature { msg: _ } => {
            println!("Correctly failed after tampering the output post-signature");
        }
        _ => panic!("Expected InvalidSignature, got {:?}", err),
    }
}
