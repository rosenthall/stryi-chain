//! tests/transactions.rs
//!
//! Integration tests for transaction logic using secp256k1 ECDSA.

use std::collections::HashMap;

use secp256k1::{Secp256k1, rand::thread_rng, PublicKey};

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

fn validate_transaction(
    tx: &Transaction,
    utxo_set: &LocalUtxoSet,
    secp: &Secp256k1<secp256k1::All>,
    public_key: &PublicKey,
    signer_address: &AccountAddress,
) -> Result<(), StryiCoreError> {
    if !tx.verify_recoverable(secp, public_key) {
        return Err(StryiCoreError::TxInvalidSignature);
    }

    let mut total_input_value = 0u64;
    for input in &tx.data.inputs {
        let outpoint = &input.previous_output;

        let utxo = utxo_set.get_utxo(&outpoint.txid, outpoint.vout)
            .ok_or(StryiCoreError::TxMissingUtxo { txid: outpoint.txid, vout: outpoint.vout })?;

        if utxo.owner != *signer_address {
            return Err(StryiCoreError::TxWrongOwner {
                expected: utxo.owner.clone(),
                actual: signer_address.clone(),
            });
        }

        total_input_value = total_input_value.saturating_add(utxo.value);
    }

    let total_output_value: u64 = tx.data.outputs.iter().map(|out| out.value).sum();
    if total_input_value < total_output_value {
        return Err(StryiCoreError::TxInsufficientInputValue {
            input_sum: total_input_value,
            output_sum: total_output_value,
        });
    }

    Ok(())
}

fn apply_transaction(tx: &Transaction, utxo_set: &mut LocalUtxoSet) {
    let tx_hash = tx.data.hash();

    for input in &tx.data.inputs {
        let outpoint = &input.previous_output;
        utxo_set.remove_utxo(&outpoint.txid, outpoint.vout);
    }

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

#[test]
fn test_valid_transactions_flow() {
    let secp = Secp256k1::new();
    let mut rng = thread_rng();

    // Generate key pairs (Alice, Bob)
    let (sk_alice, pk_alice) = secp.generate_keypair(&mut rng);
    let address_alice = AccountAddress::from_public_key(&pk_alice);

    let (sk_bob, pk_bob) = secp.generate_keypair(&mut rng);
    let address_bob = AccountAddress::from_public_key(&pk_bob);

    // Create a local UTXO set with a genesis UTXO for Alice (1000 coins)
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

    // Alice -> Bob transaction (600 coins) with change back to Alice (400 coins)
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
    let tx_alice_to_bob = tx_data_alice_to_bob.sign(&secp, &sk_alice);

    validate_transaction(
        &tx_alice_to_bob,
        &utxo_set,
        &secp,
        &pk_alice,
        &address_alice,
    ).expect("Alice->Bob tx should be valid");
    apply_transaction(&tx_alice_to_bob, &mut utxo_set);

    // Bob -> Alice transaction (300 coins)
    let bob_utxo_txid = tx_alice_to_bob.data.hash();
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
            recipient: address_alice.clone(),
        }],
    };
    let tx_bob_to_alice = tx_data_bob_to_alice.sign(&secp, &sk_bob);

    validate_transaction(
        &tx_bob_to_alice,
        &utxo_set,
        &secp,
        &pk_bob,
        &address_bob,
    ).expect("Bob->Alice tx should be valid");
    apply_transaction(&tx_bob_to_alice, &mut utxo_set);

    // Check final balances for each address
    let alice_final = utxo_set.sum_utxos_for_address(&address_alice);
    let bob_final = utxo_set.sum_utxos_for_address(&address_bob);

    // Alice: 400 change from first tx + 300 from Bob = 700
    // Bob spent his 600 to send 300 to Alice, so Bob should have 0 left.
    assert_eq!(alice_final, 700, "Alice should have 700 at the end");
    assert_eq!(bob_final, 0, "Bob should have 0 at the end");
    println!("Final UTXO set = {:#?}", utxo_set.map);
    println!("Alice final = {}, Bob final = {}", alice_final, bob_final);
}

#[test]
fn test_negative_scenarios() {
    let secp = Secp256k1::new();
    let mut rng = thread_rng();

    let (sk_alice, pk_alice) = secp.generate_keypair(&mut rng);
    let address_alice = AccountAddress::from_public_key(&pk_alice);

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
            recipient: address_alice.clone(),
        }],
    };
    let bad_tx = bad_tx_data.sign(&secp, &sk_alice);
    let err = validate_transaction(&bad_tx, &utxo_set, &secp, &pk_alice, &address_alice)
        .expect_err("Missing UTXO should fail validation");
    match err {
        StryiCoreError::TxMissingUtxo { txid, vout } => {
            println!("Correctly failed for missing UTXO: ({:?}, {})", txid, vout);
        }
        _ => panic!("Expected TxMissingUtxo, got {:?}", err),
    }

    // 2) Overspending
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
        outputs: vec![TransactionOut {
            value: 100,
            recipient: address_alice.clone(),
        }],
    };
    let overspend_tx = overspend_data.sign(&secp, &sk_alice);
    let err = validate_transaction(&overspend_tx, &utxo_set, &secp, &pk_alice, &address_alice)
        .expect_err("Overspending should fail");
    match err {
        StryiCoreError::TxInsufficientInputValue { input_sum, output_sum } => {
            println!("Correctly failed overspend: input={}, output={}", input_sum, output_sum);
        }
        _ => panic!("Expected TxInsufficientInputValue, got {:?}", err),
    }

    // 3) Tampering after signing
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
            recipient: address_alice.clone(),
        }],
    };
    let mut valid_tx = valid_data.sign(&secp, &sk_alice);
    valid_tx.data.outputs[0].value = 9999;
    let err = validate_transaction(&valid_tx, &utxo_set, &secp, &pk_alice, &address_alice)
        .expect_err("Tampering breaks the signature");
    match err {
        StryiCoreError::TxInvalidSignature => {
            println!("Correctly failed after tampering the output post-signature");
        }
        _ => panic!("Expected TxInvalidSignature, got {:?}", err),
    }
}
