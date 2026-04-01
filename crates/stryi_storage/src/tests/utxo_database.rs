use std::collections::HashMap;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng, random_range};
use tempfile::TempDir;
use tokio::test;

use stryi_core::{
    address::AccountAddress,
    storage::UtxoStorage,
    transactions::{OutPoint, TransactionHash, UTXO},
};

use crate::{GenesisInitConfig, StryiStorage, StryiStorageError};

fn random_txhash(rng: &mut impl Rng) -> TransactionHash {
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes);
    TransactionHash::new(&bytes)
}

fn random_outpoint(rng: &mut impl Rng) -> OutPoint {
    OutPoint {
        txid: random_txhash(rng),
        vout: random_range(0..10_000),
    }
}

fn random_utxo(owner: AccountAddress, op: &OutPoint, rng: &mut impl Rng) -> UTXO {
    UTXO {
        txid: op.txid,
        vout: op.vout,
        value: rng.random_range(1_000..1_000_000),
        owner,
    }
}

fn shuffle<T>(slice: &mut [T], rng: &mut StdRng) {
    for i in (1..slice.len()).rev() {
        let j = rng.random_range(0..=i);
        slice.swap(i, j);
    }
}

#[test]
async fn test_utxo_database_random_integration() -> Result<(), StryiStorageError> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let genesis_config = GenesisInitConfig::new_test();
    let mut storage =
        StryiStorage::initialize_in_path(temp_dir.path().to_owned(), Some(genesis_config)).await?;

    let addresses = vec![
        AccountAddress::new(&[0xAA; 20]),
        AccountAddress::new(&[0xBB; 20]),
        AccountAddress::new(&[0xCC; 20]),
        AccountAddress::new(&[0xDD; 20]),
        AccountAddress::new(&[0xEE; 20]),
    ];

    let total = 100;
    let seed = 1337u64;
    let mut rng = StdRng::seed_from_u64(seed);

    let mut all_pairs = Vec::with_capacity(total);
    for _ in 0..total {
        let op = random_outpoint(&mut rng);
        let addr = addresses[rng.random_range(0..addresses.len())];
        let ut = random_utxo(addr, &op, &mut rng);
        all_pairs.push((op, ut));
    }

    let mut truth_map = HashMap::new(); // Expected storage state after each mutation.

    let half = total / 2;
    let (singles, batch_group) = all_pairs.split_at(half);

    for (op, ut) in singles {
        storage.put_utxo(*op, *ut).await?;
        truth_map.insert(*op, *ut);
    }

    let batch_vec: Vec<_> = batch_group.iter().map(|(op, ut)| (*op, *ut)).collect();
    storage.batch_put_utxos(batch_vec).await?;
    for (op, ut) in batch_group {
        truth_map.insert(*op, *ut);
    }

    for (op, local) in &truth_map {
        let db = storage.get_utxo(*op).await?.unwrap();
        assert_eq!(db.txid.data, local.txid.data);
        assert_eq!(db.vout, local.vout);
        assert_eq!(db.value, local.value);
        assert_eq!(db.owner, local.owner);
    }

    // The address index should agree with the primary UTXO view.
    let mut addr_map: HashMap<AccountAddress, Vec<OutPoint>> = HashMap::new();
    for (op, ut) in &truth_map {
        addr_map.entry(ut.owner).or_default().push(*op);
    }

    for &addr in &addresses {
        let from_db = storage.get_utxos_for_address(addr).await?;
        let local_ops = addr_map.get(&addr).cloned().unwrap_or_default();
        assert_eq!(
            from_db.len(),
            local_ops.len(),
            "address index count drifted for {:?}",
            addr
        );

        let mut db_map = HashMap::new();
        for (op, db_ut) in from_db {
            db_map.insert(op, db_ut);
        }
        for op in local_ops {
            let local_ut = truth_map.get(&op).unwrap();
            let db_ut = db_map.get(&op).unwrap();
            assert_eq!(db_ut.txid.data, local_ut.txid.data);
            assert_eq!(db_ut.vout, local_ut.vout);
            assert_eq!(db_ut.value, local_ut.value);
            assert_eq!(db_ut.owner, local_ut.owner);
        }
    }

    let mut all_ops: Vec<_> = truth_map.keys().cloned().collect();
    shuffle(&mut all_ops, &mut rng);

    let remove_single = 30;
    let remove_batch = 20;
    let mut removed1 = Vec::new();
    let mut removed2 = Vec::new();

    for op in &all_ops[..remove_single] {
        let res = storage.remove_utxo(*op).await;
        if res.is_ok() {
            truth_map.remove(op);
            removed1.push(*op);
        }
    }

    let batch_ops = &all_ops[remove_single..(remove_single + remove_batch)];
    let result_batch = storage.batch_remove_utxos(batch_ops.to_vec()).await;
    if result_batch.is_ok() {
        for op in batch_ops {
            truth_map.remove(op);
            removed2.push(*op);
        }
    }

    for op in removed1.iter().chain(removed2.iter()) {
        let check = storage.get_utxo(*op).await;
        assert!(check.is_err(), "removed outpoint should stay absent");
    }

    let mut final_addrs = HashMap::new();
    for (op, ut) in &truth_map {
        final_addrs
            .entry(ut.owner)
            .or_insert_with(Vec::new)
            .push((*op, *ut));
    }

    for &addr in &addresses {
        let from_db = storage.get_utxos_for_address(addr).await?;
        let local_list = final_addrs.remove(&addr).unwrap_or_default();
        assert_eq!(
            from_db.len(),
            local_list.len(),
            "final address index count drifted for {:?}",
            addr
        );

        let mut db_map = HashMap::new();
        for (op, db_ut) in from_db {
            db_map.insert(op, db_ut);
        }

        for (op, local_ut) in local_list {
            let db_ut = db_map.get(&op).expect("expected op in DB");
            assert_eq!(db_ut.txid.data, local_ut.txid.data);
            assert_eq!(db_ut.vout, local_ut.vout);
            assert_eq!(db_ut.value, local_ut.value);
            assert_eq!(db_ut.owner, local_ut.owner);
        }
    }

    if let Some((some_op, some_ut)) = truth_map.iter().next() {
        let res = storage.put_utxo(*some_op, *some_ut).await;
        assert!(res.is_ok(), "duplicate put should not fail");
        let check = storage.get_utxo(*some_op).await?.unwrap();
        assert_eq!(
            check.value, some_ut.value,
            "duplicate put should keep the stored value"
        );
    }

    let empty_put: Vec<(OutPoint, UTXO)> = Vec::new();
    let ep = storage.batch_put_utxos(empty_put).await;
    assert!(ep.is_ok(), "empty batch put should be a no-op");

    let empty_rem: Vec<OutPoint> = Vec::new();
    let er = storage.batch_remove_utxos(empty_rem).await;
    assert!(er.is_ok(), "empty batch remove should be a no-op");

    let fake = random_outpoint(&mut rng);
    let rem = storage.remove_utxo(fake).await;
    assert!(rem.is_err(), "missing outpoint remove should fail");

    Ok(())
}
