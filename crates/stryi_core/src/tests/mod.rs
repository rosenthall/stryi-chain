mod consensus_engine;
mod mempool;
mod utxo_processor;

use crate::address::AccountAddress;
use crate::block::mining::mine_block_memcpy;
use crate::block::{Block, BlockHash, GenesisState};
use crate::consensus::{BlockValidator, ConsensusConsts, StryiConsensusEngine};
use crate::difficulty::difficulty_calculator_from_consts;
use crate::mempool::UtxoLookup;
use crate::storage::StryiInMemoryStorage;
use crate::transactions::{
    OutPoint, Transaction, TransactionData, TransactionIn, TransactionKind, TransactionOut, UTXO,
    UtxoProcessor,
};
use k256::ecdsa::SigningKey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Deterministic keypair from a single seed byte
pub(crate) fn keypair_from_seed(seed: u8) -> (SigningKey, AccountAddress) {
    let hash = blake3::hash(&[seed]);
    let key = SigningKey::from_bytes(hash.as_bytes().into())
        .expect("Blake3 output is always valid as secp256k1 secret key");
    let addr = AccountAddress::from_public_key(key.verifying_key());
    (key, addr)
}

/// Creates a signed Coinbase transaction with a single output.
pub(crate) fn make_coinbase_tx(
    signing_key: &SigningKey,
    reward: u64,
    recipient: AccountAddress,
) -> Transaction {
    let data = TransactionData {
        version: 1,
        kind: TransactionKind::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOut {
            value: reward,
            recipient,
        }],
    };
    data.sign(signing_key)
}

/// Creates a signed Payment transaction with arbitrary inputs and outputs.
pub(crate) fn make_payment_tx(
    signing_key: &SigningKey,
    inputs: Vec<OutPoint>,
    outputs: Vec<(u64, AccountAddress)>,
) -> Transaction {
    let data = TransactionData {
        version: 1,
        kind: TransactionKind::Payment,
        inputs: inputs
            .into_iter()
            .map(|op| TransactionIn {
                previous_output: op,
                sequence: 0xFFFFFFFF,
            })
            .collect(),
        outputs: outputs
            .into_iter()
            .map(|(value, recipient)| TransactionOut { value, recipient })
            .collect(),
    };
    data.sign(signing_key)
}

/// Creates an unsigned Genesis transaction with the specified outputs.
pub(crate) fn make_genesis_tx(outputs: Vec<(u64, AccountAddress)>) -> Transaction {
    Transaction::new_unsigned(TransactionData {
        version: 1,
        kind: TransactionKind::Genesis,
        inputs: vec![],
        outputs: outputs
            .into_iter()
            .map(|(value, recipient)| TransactionOut { value, recipient })
            .collect(),
    })
}

/// Creates a genesis block with specified balances and returns both the block
/// and an initialized `StryiInMemoryStorage`.
pub(crate) fn make_test_genesis(
    balances: &[(AccountAddress, u64)],
    consts: ConsensusConsts,
) -> (Block, StryiInMemoryStorage) {
    let wanted: HashMap<AccountAddress, u64> = balances.iter().cloned().collect();
    let genesis = Block::new_genesis(
        1,
        wanted,
        GenesisState {
            consensus_consts: consts,
        },
    );
    let db = StryiInMemoryStorage::new(genesis.clone());
    (genesis, db)
}

/// Creates a block and mines it. Panics if mining fails within 2_000_000 attempts
/// (should never happen for bits <= 2).
pub(crate) fn make_block_mined(
    txs: Vec<Transaction>,
    height: u64,
    difficulty_bits: u8,
    previous_block_hash: BlockHash,
) -> Block {
    let timestamp = 1_700_000_000 + height * 10;
    let mut block = Block::new(
        txs,
        previous_block_hash,
        height,
        difficulty_bits,
        timestamp,
        1,
    );
    assert!(
        mine_block_memcpy(&mut block, || false),
        "Mining failed - this should not happen for difficulty_bits <= 2"
    );
    block
}

/// Creates a fully wired `StryiConsensusEngine` from an already-initialized storage.
pub(crate) async fn make_engine(
    db: StryiInMemoryStorage,
    consts: ConsensusConsts,
) -> StryiConsensusEngine<StryiInMemoryStorage> {
    let diff_calc = difficulty_calculator_from_consts::<StryiInMemoryStorage>(consts);
    let validator = BlockValidator::new(consts, diff_calc.clone());
    let utxo_proc = UtxoProcessor::new();
    let db = Arc::new(RwLock::new(db));

    StryiConsensusEngine::new(consts, validator, utxo_proc, db)
        .await
        .expect("Engine initialization must succeed with valid genesis")
}

/// Builds a `UtxoLookup` closure from a flat list of UTXOs.
/// The closure captures an `Arc<HashMap>` and resolves OutPoint to Option<UTXO>.
pub(crate) fn make_utxo_lookup(utxos: Vec<(OutPoint, UTXO)>) -> UtxoLookup {
    let map: HashMap<OutPoint, UTXO> = utxos.into_iter().collect();
    let shared = Arc::new(map);
    Box::new(move |op: &OutPoint| {
        let shared = Arc::clone(&shared);
        let op = *op;
        Box::pin(async move { shared.get(&op).cloned() })
    })
}
