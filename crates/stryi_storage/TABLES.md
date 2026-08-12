# Storage Layout

Reference for the Fjall partitions used by `stryi_storage`.

## Global overview

- `blocks`, `heights`, `block_indexes`, `transaction_indexes`, and `stats` are updated together when storing a block.
- `addresses` is a secondary index over `utxo` and must match ownership recorded there.
- `stats` is a table with single key, stored under the all-zero 32-byte key.
- `transaction_indexes` stores canonical transaction location metadata.
- `undo` holds rollback data keyed by block hash.

## `blocks`

- Key: `stryi_core::block::BlockHash` as 32 raw bytes.
- Value: `postcard(stryi_core::block::Block)`.
- Notes: canonical block data. Written by `put_block`.

## `heights`

- Key: block height as 8-byte big-endian `u64`.
- Value: 32 raw bytes of `BlockHash`.
- Notes: canonical `height -> block_hash` mapping. Written with `blocks`.

## `utxo`

- Key: 36 bytes `[txid (32) | vout (4-byte big-endian u32)]`.
- Value: `postcard(stryi_core::transactions::UTXO)`.
- Notes: primary UTXO set. Updated on UTXO insert/remove paths.

## `addresses`

- Key: `stryi_core::address::AccountAddress` as 20 raw bytes.
- Value: `postcard(HashSet<OutPoint>)`.
- Notes: secondary index from address to owned outpoints. Updated with `utxo`.

## `stats`

- Key: fixed all-zero 32-byte key.
- Value: `postcard(stryi_storage::stats::StorageStateInformation)`.
- Notes: singleton chain-state record. Replaced on initialization and when storing a block.

## `undo`

- Key: `stryi_core::block::BlockHash` as 32 raw bytes.
- Value: `postcard(stryi_core::BlockUndo)`.
- Notes: rollback data for detach/reorg paths.

## `block_indexes`

- Key: `stryi_core::block::BlockHash` as 32 raw bytes.
- Value: `postcard(stryi_storage::index::BlockIndexData)`.
- Notes: per-block metadata written with `blocks`.

## `transaction_indexes`

- Key: `stryi_core::transactions::TransactionHash` as 32 raw bytes.
- Value: `postcard(stryi_storage::tx_index::TransactionIndexData)`.
- Notes: canonical `tx_hash -> (block_hash, block_height, tx_index)` mapping.
