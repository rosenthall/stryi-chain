use crate::{
    address::AccountAddress,
    block::{Block, BlockHash},
    consensus::{ConsensusEngine, ConsensusRules, StryiConsensusEngine},
    error::StryiCoreError,
    storage::in_memory_utxo::InMemoryUtxoStorage,
    transactions::{
        OutPoint, Transaction, TransactionHash,
    },
};
use k256::ecdsa::SigningKey;
use k256::elliptic_curve::rand_core::OsRng;

#[cfg(test)]
mod tests {
    use crate::tests::{apply_block, create_coinbase_tx, make_block, put_genesis_utxo, sign_single_input_tx};
    use super::*;

    /// Test various negative scenarios to ensure that invalid blocks are correctly rejected.
    #[tokio::test]
    async fn test_consensus_negative_scenarios() {
        // 1. Initialize Consensus Rules with difficulty=0 (disables real PoW checks)
        let rules = ConsensusRules::new_test(0);
        let engine = StryiConsensusEngine::<InMemoryUtxoStorage>::new_with_inmemory_storage(rules);
        let mut utxo_db = InMemoryUtxoStorage::default();

        // 2. Generate keypairs for Alice and Bob.
        let sk_alice = SigningKey::random(&mut OsRng);
        let vk_alice = sk_alice.verifying_key();
        let addr_alice = AccountAddress::from_public_key(vk_alice);

        let sk_bob = SigningKey::random(&mut OsRng);
        let vk_bob = sk_bob.verifying_key();
        let addr_bob = AccountAddress::from_public_key(vk_bob);

        // 3. Create a genesis outpoint for Alice.
        let genesis_op = put_genesis_utxo(&mut utxo_db, addr_alice, 1000).await;

        // 4. Create a Coinbase transaction for Alice.
        let coinbase_tx = create_coinbase_tx(&sk_alice, 50, addr_alice);

        // ----- SCENARIO A: Overspend -----
        // Input = 1000 coins, Output = 1200 => Overspending (invalid)
        let overspend_tx = sign_single_input_tx(
            genesis_op,
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
            gen2,
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
                    "Wrong owner test OK: expected={}, actual={}",
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
        let tx_missing_utxo = sign_single_input_tx(missing_op, &sk_alice, vec![(500, addr_alice)]);
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
                    "Missing UTXO test OK: txid={}, vout={}",
                    txid, vout
                );
            }
            _ => panic!("Expected TxMissingUtxo, got {:?}", err),
        }

        // ----- SCENARIO D: Invalid Signature -----
        // Create a valid transaction and then tamper the signature => ConsensusValidationFailed
        let gen4 = put_genesis_utxo(&mut utxo_db, addr_alice, 1000).await;
        let tx_good = sign_single_input_tx(gen4, &sk_alice, vec![(1000, addr_alice)]);

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

        // ----- SCENARIO E: Invalid Transaction Order Due to Dependency Violation -----
        // Transaction B depends on Transaction A but is placed before it in the block.

        // 1. Create a new UTXO for Alice.
        let gen5 = put_genesis_utxo(&mut utxo_db, addr_alice, 1000).await;

        // 2. Create Transaction A: spends gen5 and creates a new UTXO for Bob.
        let tx_a = sign_single_input_tx(
            gen5,
            &sk_alice,
            vec![(600, addr_bob), (400, addr_alice)],
        );

        // 3. Create Transaction B: spends the UTXO created by Transaction A (vout=0)
        let tx_b = sign_single_input_tx(
            OutPoint {
                txid: tx_a.data.hash(),
                vout: 0,
            },
            &sk_bob,
            vec![(300, addr_alice), (300, addr_bob)],
        );

        // 4. Create a block with Transaction B before Transaction A
        let block_invalid_dependency_order = make_block(
            vec![coinbase_tx.clone(), tx_b.clone(), tx_a],
            0,
            5,
            false,
            BlockHash::empty(), // Assuming no previous block
        );

        // 5. Apply the block and expect an error due to invalid transaction order
        let err = apply_block(&engine, &block_invalid_dependency_order, &mut utxo_db)
            .await
            .unwrap_err();
        match err {
            StryiCoreError::TransactionDependencyError { msg, .. } => {
                println!(
                    "Invalid transaction order test OK (TransactionDependencyError): {msg}"
                );
            }
            _ => panic!("Expected ConsensusValidationFailed due to invalid transaction order, got {:?}", err),
        }

        // ----- SCENARIO F: Double Spend Within the Same Block -----
        // Two transactions attempt to spend the same UTXO within the same block.

        // 1. Create a new UTXO for Alice.
        let gen6 = put_genesis_utxo(&mut utxo_db, addr_alice, 1000).await;

        // 2. Create Transaction C: spends gen6 and creates a new UTXO for Bob.
        let tx_c = sign_single_input_tx(
            gen6,
            &sk_alice,
            vec![(600, addr_bob), (400, addr_alice)],
        );

        // 3. Create Transaction D: also spends gen6 and creates a new UTXO for Bob.
        let tx_d = sign_single_input_tx(
            gen6,
            &sk_alice,
            vec![(600, addr_bob), (399, addr_alice)], // note : we can't set second output amount 400 here, it will cause "Block contains duplicated transactions", which is not what we're testing here
        );

        // 4. Create a block with both Transaction C and D spending the same UTXO
        let block_double_spend = make_block(
            vec![coinbase_tx.clone(), tx_c.clone(), tx_d],
            0,
            6,
            false,
            BlockHash::empty(), // Assuming no previous block
        );

        // 5. Apply the block and expect an error due to double spend
        let err = apply_block(&engine, &block_double_spend, &mut utxo_db)
            .await
            .unwrap_err();
        match err {
            StryiCoreError::TransactionDependencyError { msg} => {
                println!(
                    "Double spend test OK (TransactionDependencyError): {msg}"
                );
            }
            _ => panic!("Expected TransactionDependencyError due to double spend, got {:?}", err),
        }

        // ----- SCENARIO G: Duplicate transaction Within the Same Block -----
        // The block includes two absolutely identical transactions

        let gen7 = put_genesis_utxo(&mut utxo_db, addr_alice, 1000).await;


        // Create Transaction C: spends gen7 and creates a new UTXO for Bob.
        let tx_7 = sign_single_input_tx(
            gen7,
            &sk_alice,
            vec![(600, addr_bob), (400, addr_alice)],
        );

        let block_duplicated_txs = make_block(
            vec![coinbase_tx.clone(), tx_7.clone(), tx_7.clone()], // Double tx_7
            0,
            6,
            false,
            BlockHash::empty(), // Assuming no previous block
        );

        // Apply the block and expect a reasonable error
        let err = apply_block(&engine, &block_duplicated_txs, &mut utxo_db)
            .await
            .unwrap_err();

        match err {
            StryiCoreError::ConsensusValidationFailed { details } => {
                if details.as_str() == "Block contains duplicated transactions" {
                    println!("Duplicate transactions test OK");
                } else {
                    panic!("Got (unexpected) ConsensusValidationFailed details when testing duplicated blocks : {}", details);
                }
            },

            _ => panic!("Expected ConsensusValidationFailed because duplicated txs, got {:?}", err),

        }

        println!("All negative scenario tests PASSED");
    }    
    
    /// Test chain selection logic by creating multiple chains and ensuring the engine selects the best one.
    #[tokio::test]
    async fn test_complex_multi_chain_scenario() {
        // Initialize Consensus Rules with difficulty=0 (disables real PoW checks) and adjustment interval=1000
        let rules = ConsensusRules::new_test(0);
        let engine = StryiConsensusEngine::<InMemoryUtxoStorage>::new_with_inmemory_storage(rules);        let mut utxo_db_ok1 = InMemoryUtxoStorage::default();
        let mut utxo_db_ok2 = InMemoryUtxoStorage::default();
        let mut utxo_db_err1 = InMemoryUtxoStorage::default();
        let mut utxo_db_err2 = InMemoryUtxoStorage::default();
        let mut utxo_db_err3 = InMemoryUtxoStorage::default();

        // Generate a single keypair for simplicity
        let sk_user = SigningKey::random(&mut OsRng);
        let vk_user = sk_user.verifying_key();
        let addr_user = AccountAddress::from_public_key(vk_user);

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
        let mut current_op_ok1 = gen_ok1;
        let mut current_op_ok2 = gen_ok2;
        let mut current_op_err1 = gen_err1;
        let mut current_op_err2 = gen_err2;
        let mut current_op_err3 = gen_err3;

        // Helper to create and apply blocks
        async fn create_and_apply_block(
            engine: &StryiConsensusEngine<InMemoryUtxoStorage>,
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
                current_op_ok1,
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
                current_op_ok2,
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
                    current_op_err1,
                    &sk_user,
                    vec![(1200, addr_user)],
                )
            } else {
                sign_single_input_tx(
                    current_op_err1,
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
                    current_op_err2,
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
                                "chain_err_2 correctly failed at block #4: txid={}, vout={}",
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
                current_op_err3,
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
