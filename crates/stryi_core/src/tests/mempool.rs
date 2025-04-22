use crate::mempool::FeeCalculator;
use std::sync::Arc;
use bincode::config::standard;
use k256::{
    ecdsa::{SigningKey, VerifyingKey},
    elliptic_curve::rand_core::OsRng,
};

use crate::{
    address::AccountAddress,
    block::Block,
    block::BlockHash,
    mempool::{FeePolicy, MemPool, MemPoolConfig, RbfPolicy},
    transactions::{
        OutPoint, Transaction, TransactionData, TransactionHash, TransactionIn, TransactionKind,
        TransactionOut, UTXO,
    }
};

fn create_keypair() -> (SigningKey, VerifyingKey, AccountAddress) {
    let signing_key = SigningKey::random(&mut OsRng);
    let binding = signing_key.clone();
    let verifying_key = binding.verifying_key();
    let address = AccountAddress::from_public_key(&verifying_key);
    (signing_key, *verifying_key, address)
}

fn create_simple_transaction(
    signing_key: &SigningKey,
    prev_tx_hash: TransactionHash,
    prev_vout: u32,
    amount: u64,
    recipient: AccountAddress,
) -> Transaction {
    let tx_data = TransactionData {
        version: 1,
        kind: TransactionKind::Payment,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: prev_tx_hash,
                vout: prev_vout,
            },
            sequence: 0xFFFFFFFF,
        }],
        outputs: vec![TransactionOut {
            value: amount,
            recipient,
        }],
    };
    tx_data.sign(signing_key)
}

fn create_utxo_lookup(
    utxos: Vec<(OutPoint, UTXO)>,
) -> Box<dyn Fn(&OutPoint) -> std::pin::Pin<Box<dyn Future<Output = Option<UTXO>> + Send>> + Send + Sync> {


    let utxo_map = Arc::new(utxos.into_iter().collect::<std::collections::HashMap<OutPoint, UTXO>>());

    Box::new(move |outpoint: &OutPoint| {
        let map_ref = Arc::clone(&utxo_map);
        let outpoint_clone = outpoint.clone();
        Box::pin(async move { map_ref.get(&outpoint_clone).cloned() })
    })
}

#[tokio::test]
async fn test_basic_mempool_functionality() {
    let (alice_sk, _, alice_addr) = create_keypair();
    let (_bob_sk, _, bob_addr) = create_keypair();

    let genesis_hash = TransactionHash::new(&[0u8; 32]);
    let alice_outpoint = OutPoint {
        txid: genesis_hash.clone(),
        vout: 0,
    };
    let alice_utxo = UTXO {
        txid: genesis_hash.clone(),
        vout: 0,
        value: 50_000,
        owner: alice_addr.clone(),
    };

    let utxo_lookup = create_utxo_lookup(vec![(alice_outpoint, alice_utxo)]);

    let fee_policy = FeePolicy::new(100, 50, 20, 1);
    let rbf_policy = RbfPolicy::new(50, 0.10);

    let config = MemPoolConfig {
        max_size: 100,
        fee_policy,
        rbf_policy,
        expiry_time: 3600,
    };

    let mut mempool = MemPool::new(config, utxo_lookup);

    let tx = create_simple_transaction(&alice_sk, genesis_hash.clone(), 0, 40_000, bob_addr.clone());
    let tx_hash = tx.data.hash();

    let result = mempool.add_transaction(tx.clone()).await;
    assert!(
        result.is_ok(),
        "Failed to add transaction to mempool: {:?}",
        result
    );

    let best_txs = mempool.get_best_transactions(10).await.unwrap();
    assert_eq!(best_txs.len(), 1, "Expected 1 transaction in mempool");
    assert_eq!(best_txs[0].data.hash(), tx_hash, "Mismatched transaction hash");

    let block = Block::new(
        vec![tx.clone()],
        BlockHash::new(&[1u8; 32]),
        1,
        0,
        123456789,
        1,
    );

    let update_result = mempool.update_on_block(block.data.clone()).await;
    assert!(
        update_result.is_ok(),
        "Failed to update mempool after block confirmation: {:?}",
        update_result
    );

    let best_txs_after = mempool.get_best_transactions(10).await.unwrap();
    assert_eq!(best_txs_after.len(), 0, "Expected mempool to be empty");
}

#[tokio::test]
async fn test_multiple_transaction_dependencies() {
    let (alice_sk, _, alice_addr) = create_keypair();
    let genesis_hash = TransactionHash::new(&[0u8; 32]);

    let alice_outpoint = OutPoint {
        txid: genesis_hash.clone(),
        vout: 0,
    };
    let alice_utxo = UTXO {
        txid: genesis_hash.clone(),
        vout: 0,
        value: 100_000,
        owner: alice_addr.clone(),
    };

    let utxo_lookup = create_utxo_lookup(vec![(alice_outpoint, alice_utxo)]);
    let fee_policy = FeePolicy::new(100, 50, 20, 1);
    let rbf_policy = RbfPolicy::new(50, 0.10);

    let config = MemPoolConfig {
        max_size: 100,
        fee_policy,
        rbf_policy,
        expiry_time: 3600,
    };

    let mut mempool = MemPool::new(config, utxo_lookup);

    let tx1 = create_simple_transaction(&alice_sk, genesis_hash.clone(), 0, 90_000, alice_addr.clone());
    let result = mempool.add_transaction(tx1.clone()).await;
    assert!(result.is_ok(), "Failed to add tx1: {:?}", result);

    let tx2 = create_simple_transaction(
        &alice_sk,
        tx1.data.hash(),
        0,
        80_000,
        alice_addr.clone(),
    );
    let result = mempool.add_transaction(tx2.clone()).await;
    assert!(result.is_ok(), "Failed to add tx2: {:?}", result);

    let tx3 = create_simple_transaction(
        &alice_sk,
        tx2.data.hash(),
        0,
        70_000,
        alice_addr.clone(),
    );
    let result = mempool.add_transaction(tx3.clone()).await;
    assert!(result.is_ok(), "Failed to add tx3: {:?}", result);

    let best_txs = mempool.get_best_transactions(10).await.unwrap();
    assert_eq!(best_txs.len(), 3, "Expected 3 transactions in mempool");

    let tx1_idx = best_txs.iter().position(|t| t.data.hash() == tx1.data.hash()).unwrap();
    let tx2_idx = best_txs.iter().position(|t| t.data.hash() == tx2.data.hash()).unwrap();
    let tx3_idx = best_txs.iter().position(|t| t.data.hash() == tx3.data.hash()).unwrap();
    assert!(tx1_idx < tx2_idx, "tx1 should appear before tx2");
    assert!(tx2_idx < tx3_idx, "tx2 should appear before tx3");
}

#[tokio::test]
async fn test_mempool_state_serialization() {
    let (alice_sk, _, alice_addr) = create_keypair();
    let (_, _, bob_addr) = create_keypair();

    let genesis_hash = TransactionHash::new(&[0u8; 32]);
    let alice_outpoint = OutPoint {
        txid: genesis_hash.clone(),
        vout: 0,
    };
    let alice_utxo = UTXO {
        txid: genesis_hash.clone(),
        vout: 0,
        value: 50_000,
        owner: alice_addr.clone(),
    };

    let utxo_lookup = create_utxo_lookup(vec![(alice_outpoint.clone(), alice_utxo.clone())]);
    let fee_policy = FeePolicy::new(100, 50, 20, 1);
    let rbf_policy = RbfPolicy::new(50, 0.10);
    let config = MemPoolConfig {
        max_size: 100,
        fee_policy,
        rbf_policy,
        expiry_time: 3600,
    };

    let mut mempool_original = MemPool::new(config.clone(), utxo_lookup);

    let tx = create_simple_transaction(&alice_sk, genesis_hash.clone(), 0, 40_000, bob_addr);
    mempool_original.add_transaction(tx.clone()).await.unwrap();

    let state = mempool_original.get_sync_state().await.unwrap();
    let serialized = bincode::serde::encode_to_vec(state, standard()).unwrap();
    
    let restore_utxo_lookup = create_utxo_lookup(vec![(alice_outpoint, alice_utxo)]);
    let mut mempool_restored = MemPool::new(config, restore_utxo_lookup);

    let restore_result = mempool_restored.restore_state(serialized).await;
    assert!(
        restore_result.is_ok(),
        "Failed to restore mempool state: {:?}",
        restore_result
    );

    let best_txs = mempool_restored.get_best_transactions(10).await.unwrap();
    assert_eq!(best_txs.len(), 1, "Expected 1 transaction in restored mempool");
    assert_eq!(
        best_txs[0].data.hash(),
        tx.data.hash(),
        "Mismatch in restored transaction hash"
    );
}

#[tokio::test]
async fn test_ancestor_fee_optimization() {
    let (alice_sk, _, alice_addr) = create_keypair();
    let genesis_hash = TransactionHash::new(&[0u8; 32]);

    let alice_outpoint1 = OutPoint {
        txid: genesis_hash.clone(),
        vout: 0,
    };
    let alice_utxo1 = UTXO {
        txid: genesis_hash.clone(),
        vout: 0,
        value: 50_000,
        owner: alice_addr.clone(),
    };
    let alice_outpoint2 = OutPoint {
        txid: genesis_hash.clone(),
        vout: 1,
    };
    let alice_utxo2 = UTXO {
        txid: genesis_hash.clone(),
        vout: 1,
        value: 50_000,
        owner: alice_addr.clone(),
    };

    let utxo_lookup = create_utxo_lookup(vec![
        (alice_outpoint1, alice_utxo1),
        (alice_outpoint2, alice_utxo2),
    ]);

    let fee_policy = FeePolicy::new(100, 50, 20, 1);
    let rbf_policy = RbfPolicy::new(50, 0.10);
    let config = MemPoolConfig {
        max_size: 100,
        fee_policy,
        rbf_policy,
        expiry_time: 3600,
    };
    let mut mempool = MemPool::new(config, utxo_lookup);

    let chain_tx1 = create_simple_transaction(&alice_sk, genesis_hash.clone(), 0, 49_000, alice_addr.clone());
    let chain_tx2 = create_simple_transaction(&alice_sk, chain_tx1.data.hash(), 0, 48_000, alice_addr.clone());
    let chain_tx3 = create_simple_transaction(&alice_sk, chain_tx2.data.hash(), 0, 47_000, alice_addr.clone());

    let high_fee_tx = create_simple_transaction(&alice_sk, genesis_hash.clone(), 1, 30_000, alice_addr.clone());

    mempool.add_transaction(chain_tx1.clone()).await.unwrap();
    mempool.add_transaction(chain_tx2.clone()).await.unwrap();
    mempool.add_transaction(chain_tx3.clone()).await.unwrap();
    mempool.add_transaction(high_fee_tx.clone()).await.unwrap();

    let best_2 = mempool.get_best_transactions(2).await.unwrap();
    assert_eq!(best_2.len(), 2, "Expected 2 transactions when limit=2");
    let includes_high_fee = best_2.iter().any(|t| t.data.hash() == high_fee_tx.data.hash());
    assert!(
        includes_high_fee,
        "High-fee tx should appear in the top 2 by fee rate"
    );

    let all_txs = mempool.get_best_transactions(10).await.unwrap();
    assert_eq!(all_txs.len(), 4, "Expected all 4 transactions in mempool");
}

#[tokio::test]
async fn test_mempool_invalid_transaction_rejection() {
    let (alice_sk, _, alice_addr) = create_keypair();
    let (bob_sk, _, bob_addr) = create_keypair();
    let genesis_hash = TransactionHash::new(&[0u8; 32]);

    let alice_outpoint = OutPoint {
        txid: genesis_hash.clone(),
        vout: 0,
    };
    let alice_utxo = UTXO {
        txid: genesis_hash.clone(),
        vout: 0,
        value: 50_000,
        owner: alice_addr.clone(),
    };

    let utxo_lookup = create_utxo_lookup(vec![(alice_outpoint.clone(), alice_utxo)]);
    let fee_policy = FeePolicy::new(100, 50, 20, 1);
    let rbf_policy = RbfPolicy::new(50, 0.10);
    let config = MemPoolConfig {
        max_size: 100,
        fee_policy,
        rbf_policy,
        expiry_time: 3600,
    };
    let mut mempool = MemPool::new(config, utxo_lookup);

    let invalid_tx1 = create_simple_transaction(&bob_sk, genesis_hash.clone(), 0, 40_000, bob_addr.clone());
    let result = mempool.add_transaction(invalid_tx1).await;
    assert!(
        result.is_err(),
        "Should reject transaction with incorrect signature ownership"
    );

    let invalid_tx2 = create_simple_transaction(
        &alice_sk,
        TransactionHash::new(&[0xFFu8; 32]),
        0,
        40_000,
        alice_addr.clone(),
    );
    let result = mempool.add_transaction(invalid_tx2).await;
    assert!(
        result.is_err(),
        "Should reject transaction referencing a missing UTXO"
    );

    let valid_tx = create_simple_transaction(&alice_sk, genesis_hash.clone(), 0, 40_000, alice_addr.clone());
    let result = mempool.add_transaction(valid_tx.clone()).await;
    assert!(result.is_ok(), "Expected valid transaction to be accepted");

    let double_spend_tx = create_simple_transaction(&alice_sk, genesis_hash.clone(), 0, 39_500, alice_addr.clone());
    let result = mempool.add_transaction(double_spend_tx).await;
    assert!(
        result.is_err(),
        "Should reject double-spend with insufficient fee increase"
    );
}

#[tokio::test]
async fn test_rbf_multiple_conflicts() {
    let (alice_sk, _, alice_addr) = create_keypair();
    let (_, _, bob_addr) = create_keypair();
    let (_, _, charlie_addr) = create_keypair();

    let genesis_hash = TransactionHash::new(&[0u8; 32]);

    // Create two UTXOs for Alice
    let alice_outpoint1 = OutPoint {
        txid: genesis_hash.clone(),
        vout: 0,
    };
    let alice_utxo1 = UTXO {
        txid: genesis_hash.clone(),
        vout: 0,
        value: 50_000,
        owner: alice_addr.clone(),
    };

    let alice_outpoint2 = OutPoint {
        txid: genesis_hash.clone(),
        vout: 1,
    };
    let alice_utxo2 = UTXO {
        txid: genesis_hash.clone(),
        vout: 1,
        value: 50_000,
        owner: alice_addr.clone(),
    };

    let utxo_lookup = create_utxo_lookup(vec![
        (alice_outpoint1.clone(), alice_utxo1),
        (alice_outpoint2.clone(), alice_utxo2),
    ]);

    // Use a simpler fee policy to make testing more manageable
    let fee_policy = FeePolicy::new(100, 10, 10, 0);  // Minimal fee policy
    let rbf_policy = RbfPolicy::new(50, 0.05);        // 5% increase requirement

    let config = MemPoolConfig {
        max_size: 100,
        fee_policy,
        rbf_policy,
        expiry_time: 3600,
    };

    let mut mempool = MemPool::new(config.clone(), utxo_lookup);

    // Create two separate transactions spending the two UTXOs
    // Each spends an entire 50,000 input with a small output to create fees
    let tx1 = create_simple_transaction(&alice_sk, genesis_hash.clone(), 0, 49_000, bob_addr.clone());
    let tx2 = create_simple_transaction(&alice_sk, genesis_hash.clone(), 1, 49_000, bob_addr.clone());

    // Calculate fees before adding to mempool for verification
    let calculator = FeeCalculator::new(config.fee_policy.clone());
    let tx1_fee = calculator.calculate_fee(&tx1);
    let tx2_fee = calculator.calculate_fee(&tx2);
    println!("TX1 fee: {}, TX2 fee: {}", tx1_fee, tx2_fee);

    // Add both transactions to mempool
    mempool.add_transaction(tx1.clone()).await.unwrap();
    mempool.add_transaction(tx2.clone()).await.unwrap();

    // Create combined transaction with very low output to ensure high fee
    let combined_tx_data = TransactionData {
        version: 1,
        kind: TransactionKind::Payment,
        inputs: vec![
            TransactionIn {
                previous_output: alice_outpoint1.clone(),
                sequence: 0xFFFFFFFF,
            },
            TransactionIn {
                previous_output: alice_outpoint2.clone(),
                sequence: 0xFFFFFFFF,
            },
        ],
        outputs: vec![TransactionOut {
            // Create a significantly smaller output than the sum of inputs
            // to ensure a high fee: 100,000 input - 50,000 output = ~50,000 fee
            value: 50_000,
            recipient: charlie_addr,
        }],
    };

    let combined_tx = combined_tx_data.sign(&alice_sk);

    // Calculate the actual fees based on our fee calculator
    let combined_fee = calculator.calculate_fee(&combined_tx);
    let input_sum = 50_000 + 50_000; // Sum of two input UTXOs
    let output_sum = 50_000;         // Output value
    let implicit_fee = input_sum - output_sum;

    // Calculate the minimum required fee for RBF
    let min_required_fee = (tx1_fee + tx2_fee) * 105 / 100; // 5% increase

    println!("Combined explicit fee (calculator): {}", combined_fee);
    println!("Combined implicit fee (input-output): {}", implicit_fee);
    println!("Minimum required: {}", min_required_fee);

    // Make sure our test setup is valid before proceeding
    assert!(
        implicit_fee > min_required_fee,
        "Test setup issue: Combined fee ({}) must be higher than minimum required ({})",
        implicit_fee,
        min_required_fee
    );

    // Now add the combined transaction
    let result = mempool.add_transaction(combined_tx.clone()).await;
    assert!(
        result.is_ok(),
        "Should allow replacement of multiple transactions with sufficient fee: {:?}",
        result
    );

    // Check that tx1 and tx2 are no longer in the mempool
    let best_txs = mempool.get_best_transactions(10).await.unwrap();
    assert_eq!(best_txs.len(), 1, "Expected only 1 transaction in mempool after RBF");
    assert_eq!(
        best_txs[0].data.hash(),
        combined_tx.data.hash(),
        "Combined transaction should be in mempool"
    );
}