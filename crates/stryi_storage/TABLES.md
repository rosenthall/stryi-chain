# Storage Layout

Blocks, UTXOs and indexes are stored in RocksDB (`TransactionDB`) using nine fixed column families
(`default` CF is kept empty). Transitions commit all updated records in a single transaction with WAL fsync.
Chain metadata is stored in the `metadata` CF (replacing `metainfo.bin`); old Fjall datadirs require a fresh directory.

## Global overview

- `blocks` and `block_indexes` retain detached branch data.
- `heights` and `transaction_indexes` describe the canonical chain.
- `addresses` indexes ownership in `utxo`; both change in the same transaction.
- `stats` and `metadata` contain one record each.
- `undo` is retained for detached blocks and supports branch replacement.
- Readers use one borrowed snapshot for their complete query.

## `blocks`

- Key: 32 raw bytes of `BlockHash`.
- Value: `postcard(stryi_core::block::Block)`.
- Bodies are retained by hash when a branch is detached.

## `heights`

- Key: 8-byte big-endian `u64` height.
- Value: 32 raw bytes of `BlockHash`.
- Canonical `height -> block_hash` mapping; detached heights are removed.

## `utxo`

- Key: 36 bytes `[txid (32) | vout (4-byte big-endian u32)]`.
- Value: `postcard(stryi_core::transactions::UTXO)`.

## `addresses`

- Key: 20 raw bytes of `AccountAddress`.
- Value: `postcard(HashSet<OutPoint>)`.
- Empty sets are valid. Every member references a UTXO owned by the address.

## `stats`

- Key: 32 all-zero bytes.
- Value: `postcard(stryi_core::storage::ChainState)`.
- Canonical tip, block count, last update timestamp and exact cumulative work.
- Count includes genesis and equals `tip.height + 1`.

## `undo`

- Key: 32 raw bytes of `BlockHash`.
- Value: `postcard(stryi_core::BlockUndo)`.
- Contains the net UTXO changes for reversing a block.

## `block_indexes`

- Key: 32 raw bytes of `BlockHash`.
- Value: `postcard(stryi_core::storage::BlockIndexData)`.
- Immutable parent, height and cumulative work for the block's own branch.

## `transaction_indexes`

- Key: 32 raw bytes of `TransactionHash`.
- Value: `postcard(stryi_core::storage::TransactionIndexData)`.
- Canonical block hash, height and transaction position. Detach removes a
  location only when it still points to the detached block.

## `metadata`

- Key: ASCII bytes `chain`.
- Value: `postcard(stryi_core::storage::ChainMetadata)`.
- Schema version, chain name, protocol version and creation timestamp.
- Genesis and metadata are committed together. Reinitialization compares the
  full genesis content and chain configuration, preserving its creation time.
