use crate::{
    address::AccountAddress,
    block::{Block, BlockData, BlockHeader, BlockHash},
    consensus::{ConsensusEngine, ConsensusRules, StryiConsensusEngine},
    error::StryiCoreError,
    storage::in_memory_utxo::InMemoryUtxoStorage,
    transactions::{
        OutPoint, Transaction, TransactionData, TransactionHash, TransactionIn, TransactionKind,
        TransactionOut, UTXO,
    },
};
use k256::ecdsa::SigningKey;
use k256::elliptic_curve::rand_core::OsRng;
use crate::storage::UtxoStorage;

/// Helper: Create and sign a Coinbase transaction.
/// Coinbase transactions have no inputs and exactly one output.
fn create_coinbase_tx(
    signing_key: &SigningKey,
    reward: u64,
    miner_address: AccountAddress,
) -> Transaction {
    let tx_data = TransactionData {
        version: 1,
        kind: TransactionKind::Coinbase,
        inputs: vec![], // Coinbase has no inputs
        outputs: vec![TransactionOut {
            value: reward,
            recipient: miner_address,
        }],
    };
    tx_data.sign(signing_key)
}

/// Helper: Build a block with the given transactions, set difficulty, set previous_block_hash, and update merkle root.
fn make_block(
    txs: Vec<Transaction>,
    difficulty_bits: u8,
    height: u64,
    is_genesis: bool,
    previous_block_hash: BlockHash,
) -> Block {
    let mut block = Block {
        header: BlockHeader {
            version: 1,
            merkle_root_hash: [0u8; 32],
            previous_block_hash,
            height,
            difficulty_bits,
            timestamp: 123456, // Fixed for testing; adjust if needed
            nonce: 0,          // Fixed for testing; adjust if needed
            is_genesis,
        },
        data: BlockData { transactions: txs },
    };
    block.update_merkle_root();
    block
}

/// Creates a "genesis" outpoint in `utxo_db` for `owner` with the given `value`.
async fn put_genesis_utxo(
    utxo_db: &mut InMemoryUtxoStorage,
    owner: AccountAddress,
    value: u64,
) -> OutPoint {
    let genesis_txid = TransactionHash::new(&[0u8; 32]);
    let genesis_op = OutPoint {
        txid: genesis_txid,
        vout: 0,
    };
    let utxo = UTXO {
        txid: genesis_txid,
        vout: 0,
        value,
        owner,
    };
    utxo_db.put_utxo(&genesis_op, utxo).await.unwrap();
    genesis_op
}

/// Creates and signs a Payment transaction with one input and arbitrary outputs.
fn sign_single_input_tx(
    input: OutPoint,
    signing_key: &SigningKey,
    outputs: Vec<(u64, AccountAddress)>,
) -> Transaction {
    let tx_data = TransactionData {
        version: 1,
        kind: TransactionKind::Payment,
        inputs: vec![TransactionIn {
            previous_output: input,
            sequence: 0xFFFFFFFF,
        }],
        outputs: outputs
            .into_iter()
            .map(|(val, addr)| TransactionOut {
                value: val,
                recipient: addr,
            })
            .collect(),
    };
    tx_data.sign(signing_key)
}

/// Applies a block using the ConsensusEngine's validate_and_apply_block method.
/// This ensures atomic validation and application.
async fn apply_block<S: ConsensusEngine>(
    engine: &S,
    block: &Block,
    utxo_storage: &mut S::UtxoDatabase,
) -> Result<(), S::Error> {
    engine.validate_and_apply_block(block, utxo_storage).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test various negative scenarios to ensure that invalid blocks are correctly rejected.
    #[tokio::test]
    async fn test_consensus_negative_scenarios() {
        // 1. Initialize Consensus Rules with difficulty=0 (disables real PoW checks) and adjustment interval=1000
        let rules = ConsensusRules::new(0, 1000);
        let engine = StryiConsensusEngine::new(rules);
        let mut utxo_db = InMemoryUtxoStorage::default();

        // 2. Generate keypairs for Alice and Bob.
        let sk_alice = SigningKey::random(&mut OsRng);
        let vk_alice = sk_alice.verifying_key();
        let addr_alice = AccountAddress::from_public_key(&vk_alice);

        let sk_bob = SigningKey::random(&mut OsRng);
        let vk_bob = sk_bob.verifying_key();
        let addr_bob = AccountAddress::from_public_key(&vk_bob);

        // 3. Create a genesis outpoint for Alice.
        let genesis_op = put_genesis_utxo(&mut utxo_db, addr_alice, 1000).await;

        // 4. Create a Coinbase transaction for Alice.
        let coinbase_tx = create_coinbase_tx(&sk_alice, 50, addr_alice);

        // ----- SCENARIO A: Overspend -----
        // Input = 1000 coins, Output = 1200 => Overspending (invalid)
        let overspend_tx = sign_single_input_tx(
            genesis_op.clone(),
            &sk_alice,
            vec![(1200, addr_alice)],
        );
        let block_overspend = make_block(
            vec![coinbase_tx.clone(), overspend_tx],
            0,
            1,
            false,
            BlockHash::empty(), // Assuming no previous block
        );
        let err = apply_block(&engine, &block_overspend, &mut utxo_db)
            .await
            .unwrap_err();
        match err {
            StryiCoreError::TxInsufficientInputValue { input_sum, output_sum } => {
                println!(
                    "Overspend test OK: input_sum={} < output_sum={}",
                    input_sum, output_sum
                );
            }
            _ => panic!("Expected TxInsufficientInputValue, got {:?}", err),
        }

        // ----- SCENARIO B: Wrong Owner -----
        // UTXO belongs to Alice, but the TX is signed by Bob => TxWrongOwner
        // Create a new UTXO for Alice.
        let gen2 = put_genesis_utxo(&mut utxo_db, addr_alice, 1000).await;

        let tx_wrong_owner = sign_single_input_tx(
            gen2.clone(),
            &sk_bob, // Signed by Bob instead of Alice
            vec![(500, addr_bob), (500, addr_alice)],
        );
        let block_wrong_owner = make_block(
            vec![coinbase_tx.clone(), tx_wrong_owner],
            0,
            2,
            false,
            BlockHash::empty(), // Assuming no previous block
        );
        let err = apply_block(&engine, &block_wrong_owner, &mut utxo_db)
            .await
            .unwrap_err();
        match err {
            StryiCoreError::TxWrongOwner { expected, actual } => {
                println!(
                    "Wrong owner test OK: expected={:?}, actual={:?}",
                    expected, actual
                );
            }
            _ => panic!("Expected TxWrongOwner, got {:?}", err),
        }

        // ----- SCENARIO C: Missing UTXO -----
        // The TX references an outpoint that doesn't exist in DB => TxMissingUtxo
        let missing_op = OutPoint {
            txid: TransactionHash::new(&[99u8; 32]),
            vout: 9,
        };
        let tx_missing_utxo = sign_single_input_tx(missing_op.clone(), &sk_alice, vec![(500, addr_alice)]);
        let block_missing = make_block(
            vec![coinbase_tx.clone(), tx_missing_utxo],
            0,
            3,
            false,
            BlockHash::empty(), // Assuming no previous block
        );
        let err = apply_block(&engine, &block_missing, &mut utxo_db)
            .await
            .unwrap_err();
        match err {
            StryiCoreError::TxMissingUtxo { txid, vout } => {
                println!(
                    "Missing UTXO test OK: txid={:?}, vout={}",
                    txid, vout
                );
            }
            _ => panic!("Expected TxMissingUtxo, got {:?}", err),
        }

        // ----- SCENARIO D: Invalid Signature -----
        // Create a valid transaction and then tamper the signature => ConsensusValidationFailed
        let gen4 = put_genesis_utxo(&mut utxo_db, addr_alice, 1000).await;
        let tx_good = sign_single_input_tx(gen4.clone(), &sk_alice, vec![(1000, addr_alice)]);

        // Tamper with the signature
        let mut tampered = tx_good.clone();
        for byte in tampered.signature.0.iter_mut() {
            *byte = 0xFF;
        }

        let block_tampered = make_block(
            vec![coinbase_tx.clone(), tampered],
            0,
            4,
            false,
            BlockHash::empty(), // Assuming no previous block
        );
        let err = apply_block(&engine, &block_tampered, &mut utxo_db)
            .await
            .unwrap_err();
        match err {
            StryiCoreError::ConsensusValidationFailed { details } => {
                println!(
                    "Invalid signature test OK (ConsensusValidationFailed): {details}"
                );
            }
            _ => panic!("Expected ConsensusValidationFailed, got {:?}", err),
        }

        println!("All negative scenario tests PASSED");
    }

    /// Test chain selection logic by creating multiple chains and ensuring the engine selects the best one.
    #[tokio::test]
    async fn test_complex_multi_chain_scenario() {
        // Initialize Consensus Rules with difficulty=0 (disables real PoW checks) and adjustment interval=1000
        let rules = ConsensusRules::new(0, 1000);
        let engine = StryiConsensusEngine::new(rules);
        let mut utxo_db_ok1 = InMemoryUtxoStorage::default();
        let mut utxo_db_ok2 = InMemoryUtxoStorage::default();
        let mut utxo_db_err1 = InMemoryUtxoStorage::default();
        let mut utxo_db_err2 = InMemoryUtxoStorage::default();
        let mut utxo_db_err3 = InMemoryUtxoStorage::default();

        // Generate a single keypair for simplicity
        let sk_user = SigningKey::random(&mut OsRng);
        let vk_user = sk_user.verifying_key();
        let addr_user = AccountAddress::from_public_key(&vk_user);

        // Insert genesis UTXOs in each UTXO storage
        let gen_ok1 = put_genesis_utxo(&mut utxo_db_ok1, addr_user, 1000).await;
        let gen_ok2 = put_genesis_utxo(&mut utxo_db_ok2, addr_user, 1000).await;
        let gen_err1 = put_genesis_utxo(&mut utxo_db_err1, addr_user, 1000).await;
        let gen_err2 = put_genesis_utxo(&mut utxo_db_err2, addr_user, 1000).await;
        let gen_err3 = put_genesis_utxo(&mut utxo_db_err3, addr_user, 1000).await;

        // Initialize block lists for each chain
        let mut chain_ok_1 = Vec::new();
        let mut chain_ok_2 = Vec::new();
        let mut chain_err_1 = Vec::new();
        let mut chain_err_2 = Vec::new();
        let mut chain_err_3 = Vec::new();

        // Track the outpoint to spend next in each chain
        let mut current_op_ok1 = gen_ok1.clone();
        let mut current_op_ok2 = gen_ok2.clone();
        let mut current_op_err1 = gen_err1.clone();
        let mut current_op_err2 = gen_err2.clone();
        let mut current_op_err3 = gen_err3.clone();

        // Helper to create and apply blocks
        async fn create_and_apply_block(
            engine: &StryiConsensusEngine,
            utxo_db: &mut InMemoryUtxoStorage,
            previous_hash: BlockHash,
            height: u64,
            difficulty_bits: u8,
            is_genesis: bool,
            txs: Vec<Transaction>,
        ) -> Result<Block, StryiCoreError> {
            let block = make_block(txs, difficulty_bits, height, is_genesis, previous_hash);
            apply_block(engine, &block, utxo_db).await?;
            Ok(block)
        }

        // ----- Building Valid Chains -----

        // 1. Build chain_ok_1 => 5 blocks
        let mut prev_hash = BlockHash::empty();
        for height in 1..=5 {
            // Create Coinbase transaction
            let coinbase_tx = create_coinbase_tx(&sk_user, 50, addr_user);

            // Create Payment transaction
            let tx = sign_single_input_tx(
                current_op_ok1.clone(),
                &sk_user,
                vec![(1000, addr_user)],
            );
            let block = create_and_apply_block(
                &engine,
                &mut utxo_db_ok1,
                prev_hash,
                height,
                0,
                false,
                vec![coinbase_tx, tx.clone()],
            )
                .await
                .expect("chain_ok_1 block should validate and apply successfully");

            chain_ok_1.push(block.clone());
            prev_hash = block.block_hash();

            // Next outpoint
            current_op_ok1 = OutPoint {
                txid: tx.data.hash(),
                vout: 0,
            };
        }

        // 2. Build chain_ok_2 => 8 blocks
        let mut prev_hash = BlockHash::empty();
        for height in 1..=8 {
            // Create Coinbase transaction
            let coinbase_tx = create_coinbase_tx(&sk_user, 50, addr_user);

            // Create Payment transaction
            let tx = sign_single_input_tx(
                current_op_ok2.clone(),
                &sk_user,
                vec![(1000, addr_user)],
            );
            let block = create_and_apply_block(
                &engine,
                &mut utxo_db_ok2,
                prev_hash,
                height,
                0,
                false,
                vec![coinbase_tx, tx.clone()],
            )
                .await
                .expect("chain_ok_2 block should validate and apply successfully");

            chain_ok_2.push(block.clone());
            prev_hash = block.block_hash();

            // Next outpoint
            current_op_ok2 = OutPoint {
                txid: tx.data.hash(),
                vout: 0,
            };
        }

        // ----- Building Invalid Chains -----

        // 3. Build chain_err_1 => Overspend at block #3
        let mut prev_hash = BlockHash::empty();
        for height in 1..=5 {
            // Create Coinbase transaction
            let coinbase_tx = create_coinbase_tx(&sk_user, 50, addr_user);

            // Create Payment transaction
            let tx = if height == 3 {
                // Overspend: input=1000, output=1200
                sign_single_input_tx(
                    current_op_err1.clone(),
                    &sk_user,
                    vec![(1200, addr_user)],
                )
            } else {
                sign_single_input_tx(
                    current_op_err1.clone(),
                    &sk_user,
                    vec![(1000, addr_user)],
                )
            };

            let block = make_block(
                vec![coinbase_tx.clone(), tx.clone()],
                0,
                height,
                false,
                prev_hash,
            );

            if height == 3 {
                // Expect an error due to overspending
                let res = apply_block(&engine, &block, &mut utxo_db_err1).await;
                assert!(
                    res.is_err(),
                    "chain_err_1 should fail at block #3 due to overspending"
                );
                if let Err(e) = res {
                    match e {
                        StryiCoreError::TxInsufficientInputValue { input_sum, output_sum } => {
                            println!(
                                "chain_err_1 correctly failed at block #3: input_sum={} < output_sum={}",
                                input_sum, output_sum
                            );
                        }
                        _ => panic!(
                            "chain_err_1 failed with unexpected error: {:?}",
                            e
                        ),
                    }
                }
                break; // Stop building further blocks as the chain is already invalid
            } else {
                // Should apply successfully
                apply_block(&engine, &block, &mut utxo_db_err1)
                    .await
                    .expect("chain_err_1 block should validate and apply successfully");
                chain_err_1.push(block.clone());
                prev_hash = block.block_hash();

                // Next outpoint
                current_op_err1 = OutPoint {
                    txid: tx.data.hash(),
                    vout: 0,
                };
            }
        }

        // 4. Build chain_err_2 => Missing UTXO at block #4
        let mut prev_hash = BlockHash::empty();
        for height in 1..=5 {
            // Create Coinbase transaction
            let coinbase_tx = create_coinbase_tx(&sk_user, 50, addr_user);

            // Create Payment transaction
            let tx = if height == 4 {
                // Reference a missing UTXO
                let missing_op = OutPoint {
                    txid: TransactionHash::new(&[99u8; 32]),
                    vout: 999,
                };
                sign_single_input_tx(missing_op, &sk_user, vec![(1000, addr_user)])
            } else {
                sign_single_input_tx(
                    current_op_err2.clone(),
                    &sk_user,
                    vec![(1000, addr_user)],
                )
            };

            let block = make_block(
                vec![coinbase_tx.clone(), tx.clone()],
                0,
                height,
                false,
                prev_hash,
            );

            if height == 4 {
                // Expect an error due to missing UTXO
                let res = apply_block(&engine, &block, &mut utxo_db_err2).await;
                assert!(
                    res.is_err(),
                    "chain_err_2 should fail at block #4 due to missing UTXO"
                );
                if let Err(e) = res {
                    match e {
                        StryiCoreError::TxMissingUtxo { txid, vout } => {
                            println!(
                                "chain_err_2 correctly failed at block #4: txid={:?}, vout={}",
                                txid, vout
                            );
                        }
                        _ => panic!(
                            "chain_err_2 failed with unexpected error: {:?}",
                            e
                        ),
                    }
                }
                break; // Stop building further blocks as the chain is already invalid
            } else {
                // Should apply successfully
                apply_block(&engine, &block, &mut utxo_db_err2)
                    .await
                    .expect("chain_err_2 block should validate and apply successfully");
                chain_err_2.push(block.clone());
                prev_hash = block.block_hash();

                // Next outpoint
                current_op_err2 = OutPoint {
                    txid: tx.data.hash(),
                    vout: 0,
                };
            }
        }

        // 5. Build chain_err_3 => Missing Coinbase transaction in one block
        let mut prev_hash = BlockHash::empty();
        for height in 1..=5 {
            // Create Coinbase transaction unless it's block #3
            let mut txs = Vec::new();
            if height != 3 {
                let coinbase_tx = create_coinbase_tx(&sk_user, 50, addr_user);
                txs.push(coinbase_tx);
            }
            // Create Payment transaction
            let tx = sign_single_input_tx(
                current_op_err3.clone(),
                &sk_user,
                vec![(1000, addr_user)],
            );
            txs.push(tx.clone());

            let block = make_block(
                txs.clone(),
                0,
                height,
                false,
                prev_hash,
            );

            if height == 3 {
                // Expect an error due to missing Coinbase transaction
                let res = apply_block(&engine, &block, &mut utxo_db_err3).await;
                assert!(
                    res.is_err(),
                    "chain_err_3 should fail at block #3 due to missing Coinbase transaction"
                );
                if let Err(e) = res {
                    match e {
                        StryiCoreError::ConsensusValidationFailed { details } => {
                            println!(
                                "chain_err_3 correctly failed at block #3: {details}"
                            );
                        }
                        _ => panic!(
                            "chain_err_3 failed with unexpected error: {:?}",
                            e
                        ),
                    }
                }
                break; // Stop building further blocks as the chain is already invalid
            } else {
                // Should apply successfully
                apply_block(&engine, &block, &mut utxo_db_err3)
                    .await
                    .expect("chain_err_3 block should validate and apply successfully");
                chain_err_3.push(block.clone());
                prev_hash = block.block_hash();

                // Next outpoint
                current_op_err3 = OutPoint {
                    txid: tx.data.hash(),
                    vout: 0,
                };
            }
        }

        // ----- CHAIN SELECTION -----
        // Collect all chains, including invalid ones.
        let all_chains = vec![
            chain_ok_1.clone(),
            chain_ok_2.clone(),
            chain_err_1.clone(),
            chain_err_2.clone(),
            chain_err_3.clone(),
        ];

        // Select the best chain
        let best_chain_result = engine.select_chain(all_chains).await;

        match best_chain_result {
            Ok(best_chain) => {
                let diff_ok1 = engine.compute_chain_difficulty(&chain_ok_1);
                let diff_ok2 = engine.compute_chain_difficulty(&chain_ok_2);
                let diff_best = engine.compute_chain_difficulty(&best_chain);

                assert_eq!(diff_best, diff_ok2, "Expected chain_ok_2 to be chosen as the best chain");
                assert!(diff_ok2 > diff_ok1, "chain_ok_2 has higher cumulative difficulty than chain_ok_1");
                println!(
                    "test_complex_multi_chain_scenario: chain_ok_2 is selected as the best chain with cumulative difficulty = {}",
                    diff_best
                );
            }
            Err(e) => panic!("Chain selection failed unexpectedly: {:?}", e),
        }
    }
}
