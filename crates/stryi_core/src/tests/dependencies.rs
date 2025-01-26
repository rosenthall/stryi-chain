use crate::{
    block::Block,
    dependencies::DependencyGraph,
    transactions::{OutPoint, TransactionData, TransactionKind, TransactionOut},
    storage::in_memory_utxo::InMemoryUtxoStorage,
};
use k256::ecdsa::SigningKey;
use k256::elliptic_curve::rand_core::OsRng;
use crate::address::AccountAddress;
use crate::block::{BlockHash, BlockValidator};
use crate::transactions::TransactionIn;

#[tokio::test]
async fn test_complex_dependency_chain() {
    // Initialize test environment
    let sk_alice = SigningKey::random(&mut OsRng);
    let addr_alice = AccountAddress::from_public_key(&sk_alice.verifying_key());
    let mut utxo_storage = InMemoryUtxoStorage::new();
    let block_validator = BlockValidator::new(0); // Zero difficulty for testing

    // Create transaction chain
    let mut txs = Vec::new();
    let mut tx_hashes = Vec::new();

    // Create coinbase transaction
    let coinbase = TransactionData {
        version: 1,
        kind: TransactionKind::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOut {
            value: 1000,
            recipient: addr_alice.clone(),
        }],
    }.sign(&sk_alice);

    tx_hashes.push(coinbase.data.hash());
    txs.push(coinbase);

    // Create chain of dependent transactions
    for i in 0..5 {
        let tx = TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![TransactionIn {
                previous_output: OutPoint {
                    txid: tx_hashes[i].clone(),
                    vout: 0,
                },
                sequence: 0,
            }],
            outputs: vec![TransactionOut {
                value: 100 * (5 - i as u64),
                recipient: addr_alice.clone(),
            }],
        }.sign(&sk_alice);

        tx_hashes.push(tx.data.hash());
        txs.push(tx);
    }

    let block = Block::new(
        txs,
        BlockHash::empty(),
        1,
        0,
        1700000000,
        1,
    );

    // Test proper ordering validation
    let mut dep_graph = DependencyGraph::build(&block.data).expect("Should build dependency graph");
    assert!(dep_graph.validate_order(&block.data).is_ok(), "Transaction order should be valid");

    // Test external dependencies
    let external_deps = dep_graph.get_external_dependencies();
    assert!(external_deps.is_empty(), "Should have no external dependencies in this test");

    // Test full block validation
    assert!(block_validator.validate_dependencies(&block.data, &mut utxo_storage)
                .await
                .is_ok(),
            "Block should pass dependency validation"
    );

    // Test invalid ordering
    let mut invalid_txs = block.data.transactions.clone();
    invalid_txs.swap(1, 2); // Swap two dependent transactions
    let invalid_block = Block::new(
        invalid_txs,
        BlockHash::empty(),
        1,
        0,
        1700000000,
        1,
    );

    let mut invalid_dep_graph = DependencyGraph::build(&invalid_block.data).expect("Should build graph");
    assert!(invalid_dep_graph.validate_order(&invalid_block.data).is_err(),
            "Should reject invalid transaction order"
    );
}

#[tokio::test]
async fn test_double_spend_prevention() {
    let sk_alice = SigningKey::random(&mut OsRng);
    let addr_alice = AccountAddress::from_public_key(&sk_alice.verifying_key());

    // Create a coinbase transaction
    let coinbase = TransactionData {
        version: 1,
        kind: TransactionKind::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOut {
            value: 1000,
            recipient: addr_alice.clone(),
        }],
    }.sign(&sk_alice);

    let coinbase_hash = coinbase.data.hash();

    // Create two transactions spending the same output
    let tx1 = TransactionData {
        version: 1,
        kind: TransactionKind::Payment,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: coinbase_hash.clone(),
                vout: 0,
            },
            sequence: 0,
        }],
        outputs: vec![TransactionOut {
            value: 500,
            recipient: addr_alice.clone(),
        }],
    }.sign(&sk_alice);

    let tx2 = TransactionData {
        version: 1,
        kind: TransactionKind::Payment,
        inputs: vec![TransactionIn {
            previous_output: OutPoint {
                txid: coinbase_hash.clone(),
                vout: 0,
            },
            sequence: 0,
        }],
        outputs: vec![TransactionOut {
            value: 400,
            recipient: addr_alice.clone(),
        }],
    }.sign(&sk_alice);

    let block = Block::new(
        vec![coinbase, tx1, tx2],
        BlockHash::empty(),
        1,
        0,
        1700000000,
        1,
    );

    let mut dep_graph = DependencyGraph::build(&block.data).expect("Should build graph");
    assert!(dep_graph.validate_order(&block.data).is_err(),
            "Should detect double spend attempt"
    );
}