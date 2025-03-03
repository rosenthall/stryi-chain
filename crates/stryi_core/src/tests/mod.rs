mod transactions;
mod consensus;
mod dependencies;
mod mempool;

use crate::{
    address::AccountAddress,
    block::{Block, BlockData, BlockHeader, BlockHash},
    consensus::{ConsensusEngine},
    storage::in_memory_utxo::InMemoryUtxoStorage,
    transactions::{
        OutPoint, Transaction, TransactionData, TransactionHash, TransactionIn, TransactionKind,
        TransactionOut, UTXO,
    },
};
use k256::ecdsa::SigningKey;
use crate::storage::UtxoStorage;



// Below is some helper methods for testing



/// Helper: Create and sign a Coinbase transaction.
/// Coinbase transactions have no inputs and exactly one output.
pub(crate) fn create_coinbase_tx(
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
pub(crate) fn make_block(
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
            timestamp: 123456,
            nonce: 0,
            is_genesis,
        },
        data: BlockData { transactions: txs },
    };
    block.update_merkle_root();
    block
}

/// Creates a "genesis" outpoint in `utxo_db` for `owner` with the given `value`.
pub(crate) async fn put_genesis_utxo(
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
pub(crate) fn sign_single_input_tx(
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
pub(crate) async fn apply_block<S: ConsensusEngine>(
    engine: &S,
    block: &Block,
    utxo_storage: &mut S::UtxoDatabase,
) -> Result<(), S::Error> {
    engine.validate_and_apply_block(block, utxo_storage).await
}
