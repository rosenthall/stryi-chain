/// Integration test for `get_utxos_for_address`-related logic, and the `addresses` partition by itself
/// Checks correctness for read and checks if `addresses` state correctly updates after delete/delete_batch operations
mod addresses_integration;
/// Integration test for common UTXO storage logic,
/// performs some random-values inserts, various batch operations and then checks result
mod utxo_database;

use crate::{GenesisInitConfig, STORAGE_SCHEMA_VERSION, StorageStatus, StryiStorage};
use std::collections::{HashMap, HashSet};
use stryi_core::BlockUndo;
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::ForkDbOverlay;
use stryi_core::difficulty::{ChainWork, HashThreshold};
use stryi_core::storage::{
    BlockIndexData, BlockStorage, ChainMetadata, ChainState, ChainTransition, FullNodeStorage,
    StorageError, StorageReader, StorageStats, StorageWrite, UndoStorage, UtxoStorage, UtxoWrite,
};
use stryi_core::transactions::{
    OutPoint, Transaction, TransactionData, TransactionHash, TransactionIn, TransactionKind,
    TransactionOut, UTXO, UtxoProcessor,
};
use tempfile::TempDir;

fn metadata() -> ChainMetadata {
    ChainMetadata {
        schema_version: STORAGE_SCHEMA_VERSION,
        chain_name: "test".into(),
        protocol_version: 1,
        created_unix_ts: 1_700_000_000,
    }
}

fn next_block(
    parent: &Block,
    recipient: u8,
    target: HashThreshold,
    mut payments: Vec<Transaction>,
) -> Block {
    let height = parent.header.height + 1;
    let coinbase = Transaction::new_unsigned(TransactionData {
        version: 1,
        kind: TransactionKind::Coinbase {
            height,
            parent: parent.block_hash(),
        },
        inputs: vec![],
        outputs: vec![TransactionOut {
            value: 50,
            recipient: AccountAddress::new(&[recipient; 20]),
        }],
    });

    payments.insert(0, coinbase);
    let mut block = Block::new(
        payments,
        parent.block_hash(),
        height,
        target,
        parent.header.timestamp + 10,
        1,
    );

    for nonce in 0..10_000 {
        block.header.nonce = nonce;
        if block.validate_proof_of_work() {
            return block;
        }
    }

    panic!("failed to find a valid nonce for test block");
}

fn payment(input: OutPoint, recipient: u8, value: u64) -> Transaction {
    Transaction::new_unsigned(TransactionData {
        version: 1,
        kind: TransactionKind::Payment,
        inputs: vec![TransactionIn {
            previous_output: input,
            sequence: 0,
        }],
        outputs: vec![TransactionOut {
            value,
            recipient: AccountAddress::new(&[recipient; 20]),
        }],
    })
}

#[test]
fn test_stryi_block_storage_basics() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let storage = StryiStorage::initialize_in_path(
        dir.path().join("nested/db"),
        Some(GenesisInitConfig::new_test()),
    )?;
    let mut parent = storage.read()?.get_block_by_height(0)?.unwrap();
    assert!(parent.header.is_genesis());

    for recipient in 1..=5 {
        let block = next_block(&parent, recipient, HashThreshold::MAX >> 1, vec![]);
        storage.apply_transition(ChainTransition::Append { block: &block })?;
        parent = block;
    }

    let view = storage.read()?;
    let range = view.blocks_range(2..=4)?;
    assert_eq!(range.len(), 3);

    for height in 2..=4 {
        assert_eq!(range[&height].header.height, height);
    }

    assert_eq!(view.tip()?, (5, parent.block_hash()));
    assert_eq!(view.block_count()?, 6);
    assert_eq!(view.chain_work()?, ChainWork::from(10u64));
    assert!(view.get_block_by_height(6)?.is_none());
    assert!(
        view.blocks_range(std::ops::RangeInclusive::new(4, 2))
            .is_err()
    );

    drop(view);
    drop(storage);

    let reopened = StryiStorage::initialize_in_path(dir.path().join("nested/db"), None)?;
    assert_eq!(reopened.read()?.tip()?, (5, parent.block_hash()));
    assert_eq!(reopened.read()?.chain_work()?, ChainWork::from(10u64));

    Ok(())
}

#[test]
fn test_stryi_storage_gaps() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let storage = StryiStorage::initialize_in_path(
        dir.path().to_path_buf(),
        Some(GenesisInitConfig::new_test()),
    )?;
    let genesis = storage.read()?.get_block_by_height(0)?.unwrap();
    let first = next_block(&genesis, 1, HashThreshold::MAX >> 1, vec![]);
    storage.apply_transition(ChainTransition::Append { block: &first })?;

    let mut gap = next_block(&first, 2, HashThreshold::MAX >> 1, vec![]);
    gap.header.height = 3;

    assert!(matches!(
        storage.apply_transition(ChainTransition::Append { block: &gap }),
        Err(StorageError::InvalidTransition(_))
    ));

    let view = storage.read()?;
    assert!(matches!(
        view.blocks_range(1..=3),
        Err(StorageError::HeightMissing(2))
    ));
    assert!(!view.block_exists(gap.block_hash())?);

    Ok(())
}

#[test]
fn test_chain_work_overflow_fails_before_writes() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let storage = StryiStorage::initialize_in_path(
        dir.path().to_path_buf(),
        Some(GenesisInitConfig::new_test()),
    )?;
    let genesis = storage.read()?.get_block_by_height(0)?.unwrap();
    let mut saturated = storage.read()?.chain_state()?.unwrap();

    // Set stored work to MAX so adding next block's work overflows
    saturated.chain_work = ChainWork::MAX;
    let index = BlockIndexData {
        parent_hash: BlockHash::empty(),
        height: 0,
        chain_work: ChainWork::MAX,
    };

    storage.update(|view| {
        view.put_chain_state(&saturated)?;
        view.put("block_indexes", &genesis.block_hash().data, &index)
    })?;

    let block = next_block(&genesis, 1, HashThreshold::MAX >> 1, vec![]);
    assert!(matches!(
        storage.apply_transition(ChainTransition::Append { block: &block }),
        Err(StorageError::InvalidTransition(_))
    ));

    let view = storage.read()?;

    // Make sure nothing was written when work sum failed
    assert_eq!(view.chain_state()?, Some(saturated));
    assert!(!view.block_exists(block.block_hash())?);
    assert!(view.get_block_index(&block.block_hash())?.is_none());
    assert!(view.get_block_by_height(1)?.is_none());
    assert!(
        view.get_utxos_for_address(AccountAddress::new(&[1; 20]))?
            .is_empty()
    );

    Ok(())
}

#[test]
fn test_invalid_pow_rejected_on_init() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("db");
    let mut bad = GenesisInitConfig::new_test();

    bad.genesis_state.consensus_consts.pow.target_spacing_secs = 0;
    assert!(matches!(
        StryiStorage::initialize_in_path(path.clone(), Some(bad.clone())),
        Err(StorageError::InvalidTransition(_))
    ));
    assert!(!path.exists());

    // Valid config should still initialize cleanly afterwards
    let storage =
        StryiStorage::initialize_in_path(path.clone(), Some(GenesisInitConfig::new_test()))?;
    assert_eq!(
        storage
            .read()?
            .get_utxos_for_address(AccountAddress::new(&[0; 20]))?
            .values()
            .map(|utxo| utxo.value)
            .sum::<u64>(),
        1_000_000
    );

    drop(storage);

    assert!(matches!(
        StryiStorage::initialize_in_path(path.clone(), Some(bad)),
        Err(StorageError::InvalidTransition(_))
    ));

    let reopened = StryiStorage::initialize_in_path(path, Some(GenesisInitConfig::new_test()))?;
    assert_eq!(reopened.read()?.block_count()?, 1);
    assert_eq!(
        reopened
            .read()?
            .get_block_by_height(0)?
            .unwrap()
            .header
            .genesis_state
            .unwrap()
            .consensus_consts
            .pow
            .target_spacing_secs,
        10
    );

    Ok(())
}

#[test]
fn test_malformed_genesis_shape_rejected_before_storage() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let storage = StryiStorage::initialize_in_path(dir.path().to_path_buf(), None)?;
    assert_eq!(storage.status()?, StorageStatus::NoGenesis);

    let config = GenesisInitConfig::new_test();
    let mut genesis = Block::new_genesis(
        config.version,
        HashMap::from_iter(config.wanted_balances),
        config.genesis_state,
    );
    genesis.header.hash_threshold = HashThreshold::MAX;
    genesis.header.timestamp = 12_345;
    genesis.header.nonce = 7;

    assert!(crate::validate_genesis(&genesis).is_err());
    assert!(matches!(
        storage.apply_transition(ChainTransition::Genesis {
            block: &genesis,
            metadata: &metadata()
        }),
        Err(StorageError::InvalidTransition(_))
    ));

    let view = storage.read()?;
    assert!(view.chain_state()?.is_none());
    assert!(view.metadata()?.is_none());
    assert!(view.get_block_by_height(0)?.is_none());

    Ok(())
}

#[test]
fn test_genesis_tx_indexed() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let storage = StryiStorage::initialize_in_path(
        dir.path().to_path_buf(),
        Some(GenesisInitConfig::new_test()),
    )?;
    let view = storage.read()?;
    let genesis = view.get_block_by_height(0)?.unwrap();
    let location = view
        .get_transaction(&genesis.data.transactions[0].data.hash())?
        .unwrap()
        .location;

    assert_eq!(location.block_hash, genesis.block_hash());
    assert_eq!(location.block_height, 0);
    assert_eq!(location.tx_index, 0);

    Ok(())
}

#[test]
fn test_storage_state_operations() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let storage = StryiStorage::initialize_in_path(dir.path().to_path_buf(), None)?;
    assert!(storage.read()?.chain_state()?.is_none());

    let state = ChainState {
        latest_block: (1, BlockHash::empty()),
        last_update_time: 12345,
        blocks_count: 2,
        chain_work: ChainWork::from(100u64),
    };

    storage.update(|view| view.put_chain_state(&state))?;
    assert_eq!(storage.read()?.chain_state()?, Some(state));

    Ok(())
}

#[test]
fn test_block_undo_operations() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let storage = StryiStorage::initialize_in_path(dir.path().to_path_buf(), None)?;
    let hash = BlockHash::new(&[1; 32]);
    let outpoint = OutPoint {
        txid: TransactionHash::new(&[1; 32]),
        vout: 0,
    };
    let utxo = UTXO {
        txid: outpoint.txid,
        vout: 0,
        value: 500,
        owner: AccountAddress::new(&[0; 20]),
    };
    let undo = BlockUndo {
        spent_utxos: HashSet::from([(outpoint, utxo)]),
        created_outpoints: HashSet::from([
            OutPoint {
                vout: 1,
                ..outpoint
            },
            OutPoint {
                vout: 2,
                ..outpoint
            },
        ]),
    };

    storage.update(|view| view.put_block_undo(hash, undo))?;
    let saved = storage.read()?.get_block_undo(hash)?.unwrap();
    assert_eq!(saved.spent_utxos, HashSet::from([(outpoint, utxo)]));
    assert_eq!(saved.created_outpoints.len(), 2);

    storage.update(|view| view.remove("undo", &hash.data))?;
    assert!(storage.read()?.get_block_undo(hash)?.is_none());

    Ok(())
}

#[test]
fn test_transaction_snapshot_and_rollback() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let storage = StryiStorage::initialize_in_path(
        dir.path().to_path_buf(),
        Some(GenesisInitConfig::new_test()),
    )?;
    let alice = AccountAddress::new(&[1; 20]);
    let bob = AccountAddress::new(&[2; 20]);
    let outpoint = OutPoint {
        txid: TransactionHash::new(&[5; 32]),
        vout: 3,
    };
    let original = UTXO {
        txid: outpoint.txid,
        vout: outpoint.vout,
        value: 123,
        owner: alice,
    };

    storage.update(|view| view.batch_put_utxos(vec![(outpoint, original)]))?;
    let old = storage.read()?;

    let changed = UTXO {
        owner: bob,
        value: 456,
        ..original
    };

    storage.update(|view| {
        view.batch_remove_utxos(vec![outpoint])?;
        assert!(view.get_utxo(outpoint)?.is_none());

        view.batch_put_utxos(vec![
            (outpoint, changed),
            (
                outpoint,
                UTXO {
                    value: 789,
                    ..changed
                },
            ),
        ])?;
        assert_eq!(view.get_utxo(outpoint)?.unwrap().value, 789);

        Ok(())
    })?;

    assert_eq!(old.get_utxo(outpoint)?, Some(original));
    assert_eq!(
        old.get_utxos_for_address(alice)?,
        HashMap::from([(outpoint, original)])
    );
    assert!(old.get_utxos_for_address(bob)?.is_empty());

    let rejected: Result<(), StorageError> = storage.update(|view| {
        view.batch_put_utxos(vec![(outpoint, original)])?;
        Err(StorageError::InvalidTransition(
            "reject staged owner change".into(),
        ))
    });
    assert!(matches!(rejected, Err(StorageError::InvalidTransition(_))));

    let current = storage.read()?;
    assert!(current.get_utxos_for_address(alice)?.is_empty());
    assert_eq!(
        current.get_utxo(outpoint)?,
        Some(UTXO {
            value: 789,
            ..changed
        })
    );
    assert_eq!(current.get_utxos_for_address(bob)?.len(), 1);

    Ok(())
}

#[test]
fn test_reorg_heavier_fork_survives_reopen() -> Result<(), StorageError> {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let storage =
        StryiStorage::initialize_in_path(path.clone(), Some(GenesisInitConfig::new_test()))?;
    let genesis = storage.read()?.get_block_by_height(0)?.unwrap();
    let allocation = OutPoint {
        txid: genesis.data.transactions[0].data.hash(),
        vout: 0,
    };
    let prefix_payment = payment(allocation, 1, 900);
    let prefix_output = OutPoint {
        txid: prefix_payment.data.hash(),
        vout: 0,
    };
    let prefix = next_block(&genesis, 11, HashThreshold::MAX >> 1, vec![prefix_payment]);
    let shared_payment = payment(prefix_output, 2, 800);
    let shared_output = OutPoint {
        txid: shared_payment.data.hash(),
        vout: 0,
    };
    let old_second = next_block(
        &prefix,
        12,
        HashThreshold::MAX >> 1,
        vec![shared_payment.clone()],
    );
    let stale_payment = payment(shared_output, 3, 700);
    let old_tip = next_block(
        &old_second,
        13,
        HashThreshold::MAX >> 1,
        vec![stale_payment.clone()],
    );

    for block in [&prefix, &old_second, &old_tip] {
        storage.apply_transition(ChainTransition::Append { block })?;
    }

    let old = storage.read()?;
    let fresh_payment = payment(shared_output, 4, 700);
    let replacement = next_block(
        &prefix,
        14,
        HashThreshold::MAX >> 3,
        vec![shared_payment.clone(), fresh_payment.clone()],
    );

    let mut overlay = ForkDbOverlay::new(&old, prefix.block_hash())?;
    let processor = UtxoProcessor::new();
    overlay.rewind_block(&processor)?;
    overlay.rewind_block(&processor)?;
    overlay.replay_block(&replacement)?;
    let delta = overlay.into_delta()?;

    let result = storage.apply_transition(ChainTransition::Reorganize { delta })?;

    assert_eq!(
        result.detached,
        vec![(3, old_tip.block_hash()), (2, old_second.block_hash())]
    );
    assert_eq!(result.new_head, (2, replacement.block_hash()));
    assert_eq!(old.tip()?, (3, old_tip.block_hash()));
    assert_eq!(old.chain_work()?, ChainWork::from(6u64));

    drop(old);
    drop(storage);

    let reopened = StryiStorage::initialize_in_path(path, None)?;
    let view = reopened.read()?;

    assert_eq!(view.tip()?, (2, replacement.block_hash()));
    assert_eq!(view.block_count()?, 3);
    assert_eq!(view.chain_work()?, ChainWork::from(10u64));
    assert_eq!(
        view.get_block_hash_by_height(2)?,
        Some(replacement.block_hash())
    );
    assert!(view.get_block_by_height(3)?.is_none());

    for block in [&prefix, &old_second, &old_tip, &replacement] {
        assert_eq!(
            view.get_block_by_hash(block.block_hash())?,
            Some(block.clone())
        );
        assert!(view.get_block_undo(block.block_hash())?.is_some());
    }

    assert_eq!(
        view.get_block_index(&old_tip.block_hash())?
            .unwrap()
            .chain_work,
        ChainWork::from(6u64)
    );

    assert_eq!(
        view.get_transaction(&shared_payment.data.hash())?
            .unwrap()
            .location
            .block_hash,
        replacement.block_hash()
    );
    assert_eq!(
        view.get_transaction(&fresh_payment.data.hash())?
            .unwrap()
            .location
            .block_hash,
        replacement.block_hash()
    );
    assert!(view.get_transaction(&stale_payment.data.hash())?.is_none());

    assert!(view.get_utxo(allocation)?.is_none());
    assert!(view.get_utxo(prefix_output)?.is_none());
    assert!(view.get_utxo(shared_output)?.is_none());
    assert!(
        view.get_utxos_for_address(AccountAddress::new(&[3; 20]))?
            .is_empty()
    );
    assert!(
        view.get_utxos_for_address(AccountAddress::new(&[12; 20]))?
            .is_empty()
    );
    assert!(
        view.get_utxos_for_address(AccountAddress::new(&[13; 20]))?
            .is_empty()
    );

    let remaining = view.get_utxos_for_address(AccountAddress::new(&[4; 20]))?;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining.values().next().unwrap().value, 700);
    assert_eq!(
        view.get_utxos_for_address(AccountAddress::new(&[11; 20]))?
            .len(),
        1
    );
    assert_eq!(
        view.get_utxos_for_address(AccountAddress::new(&[14; 20]))?
            .len(),
        1
    );

    Ok(())
}
