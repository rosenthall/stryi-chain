use crate::chaingen::ChainGenerator;
use crate::chaingen::state::GenerationState;
use crate::chaingen::strategy::TxGenerationStrategy;
use crate::chaingen::txgen::{
    TransactionGenerationParams, generate_distributing_transaction, generate_transaction,
};
use crate::chaingen::utxo::UtxoInfo;
use indexmap::IndexSet;
use rand::RngExt;
use rand_chacha::ChaCha8Rng;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use std::collections::HashSet;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::BlockStorage;
use stryi_core::difficulty::{MAX_FUTURE_BLOCK_TIME_SECS, median_time_past};
use stryi_core::transactions::{
    OutPoint, Transaction, TransactionData, TransactionKind, TransactionOut,
};
use tracing::{debug, info, warn};

impl ChainGenerator {
    /// Generates a deterministic and valid block at `height` that has only one payment transaction which evenly distributes all the balance of the funding account.
    /// It uses all the existing UTXOs outputs of funder.
    pub async fn build_distributing_block(
        &self,
        state: &mut GenerationState,
    ) -> Result<Block, String> {
        if !state.account_utxos.is_empty() {
            return Err(
                "Distributor block can only be built as the first generated block".to_string(),
            );
        }

        let consts = self.consensus_consts;
        let miner = self.config.blocks.miner_address;
        let distributor_chain_height = self.pre_generation_height + 1;

        // Get the current tip to build on: it provides the parent, timestamp and version
        let (prev_block, hash_threshold, median_time) = {
            let s = self.storage.read().await;
            let prev_block = s
                .get_block_by_height(self.pre_generation_height)
                .await
                .map_err(|e| format!("Failed to read previous block for distributor: {}", e))?
                .ok_or_else(|| {
                    format!(
                        "Previous block at height {} not found",
                        self.pre_generation_height
                    )
                })?;

            // The threshold is derived from the same storage view that provided
            // the parent, so a resumed generation and the validator agree.
            let hash_threshold = self
                .difficulty_calc
                .next_hash_threshold(&*s, prev_block.block_hash())
                .await
                .map_err(|e| format!("Failed to compute next hash threshold: {e}"))?;

            let median_time = median_time_past(&*s, prev_block.block_hash())
                .await
                .map_err(|e| format!("Failed to read distributor timestamp history: {e}"))?;

            (prev_block, hash_threshold, median_time)
        };

        let base_time = self.config.blocks.average_block_time_secs;
        let timestamp = generation_timestamp(
            prev_block.header.timestamp,
            base_time,
            0,
            median_time,
            generation_now_secs()?,
        )?;
        let version = prev_block.header.version;
        let subsidy = consts.block_subsidy(distributor_chain_height);

        let coinbase_tx = Transaction::new_unsigned(TransactionData {
            version: 0,
            kind: TransactionKind::Coinbase {
                height: distributor_chain_height,
                parent: prev_block.block_hash(),
            },
            inputs: vec![],
            outputs: vec![TransactionOut {
                value: subsidy,
                recipient: miner,
            }],
        });

        let dist_tx = generate_distributing_transaction(state)
            .map_err(|e| format!("Failed to generate distributing transaction: {}", e))?;

        info!(
            "Distribution transaction created with {} inputs and {} outputs",
            dist_tx.data.inputs.len(),
            dist_tx.data.outputs.len(),
        );
        info!(
            "Total value distributed: {} ",
            dist_tx.data.outputs.iter().map(|o| o.value).sum::<u64>()
        );

        // Finalize block
        let mut block = Block::new(
            vec![coinbase_tx, dist_tx],
            prev_block.block_hash(),
            distributor_chain_height,
            hash_threshold,
            timestamp,
            version,
        );

        mine_block_parallel_ordered(&mut block, MINING_ATTEMPT_CAP)?;

        Ok(block)
    }

    /// Builds one generated block on top of the current tip.
    ///
    /// `height` must be above genesis and the distributor block.
    /// `rng` drives the deterministic randomness used for timestamps and transaction mix.
    /// `state` holds the generated accounts and the UTXO view chaingen uses while building the block.
    ///
    /// The result is a block ready to persist.
    /// If no viable payment transactions can be created for this height, the block falls back to coinbase-only.
    ///
    /// Returns an error if the requested height is reserved, the previous block cannot be loaded,
    /// the configured miner address is invalid, or block mining fails.
    pub async fn build_block(
        &self,
        height: u64,
        rng: &mut ChaCha8Rng,
        state: &mut GenerationState,
    ) -> Result<Block, String> {
        if height == 0 || height == 1 {
            return Err(
                "build_block called with height=0 (genesis) or 1 (distributor)".to_string(),
            );
        }
        // Hold a shared lock long enough to load the previous block.
        let storage_guard = self.storage.read().await;

        // Load previous block once
        let prev = storage_guard
            .get_block_by_height(height - 1)
            .await
            .map_err(|e| format!("cannot read prev block at height {}: {e}", height - 1))?
            .ok_or_else(|| format!("prev block at height {} not found", height - 1))?;

        // Consensus fields for this height, from the same view as the parent.
        let hash_threshold = self
            .difficulty_calc
            .next_hash_threshold(&*storage_guard, prev.block_hash())
            .await
            .map_err(|e| format!("cannot compute next hash threshold: {e}"))?;

        let median_time = median_time_past(&*storage_guard, prev.block_hash())
            .await
            .map_err(|e| format!("cannot read generation timestamp history: {e}"))?;

        // Add deterministic variance to block times using the RNG
        let base_time = self.config.blocks.average_block_time_secs;
        let variance_range = base_time.min(30); // Max 30s variance or block time, whichever is smaller
        let variance = rng.random_range(0..=variance_range);
        let timestamp = generation_timestamp(
            prev.header.timestamp,
            base_time,
            variance,
            median_time,
            generation_now_secs()?,
        )?;

        let miner = self.config.blocks.miner_address;
        let subsidy = self.consensus_consts.block_subsidy(height);

        // Start with a coinbase transaction
        let coinbase_tx = Transaction::new_unsigned(TransactionData {
            version: 0,
            kind: TransactionKind::Coinbase {
                height,
                parent: prev.block_hash(),
            },
            inputs: vec![],
            outputs: vec![TransactionOut {
                value: subsidy,
                recipient: miner,
            }],
        });
        let mut transactions = vec![coinbase_tx];

        // Choose accounts to use in this block
        let (min_txs, max_txs) = (
            self.config.blocks.min_transactions_per_block,
            self.config.blocks.max_transactions_per_block,
        );

        // calculate how many transactions must be in block
        let total_tx_count = rng.random_range(min_txs..=max_txs) as usize;
        let payment_tx_count = total_tx_count.saturating_sub(1); // Subtract 1 for coinbase

        if payment_tx_count == 0 {
            return Err(format!(
                "block at height {height} must reserve room for at least one payment transaction"
            ));
        }

        let params = TransactionGenerationParams::default();
        let receivers_pool = state
            .get_best_receivers(state.accounts.len())
            .expect("Must be available receivers.");

        // Initialize strategy
        let mut strategy = TxGenerationStrategy::new(payment_tx_count);

        // Track UTXOs spent in this block to avoid double-spends
        let mut spent_in_block: HashSet<OutPoint> = HashSet::new();

        while strategy.should_continue() {
            strategy.record_fail();

            // Get accounts with UTXOs
            let top_accounts =
                state.get_top_accounts_with_utxos(strategy.target_txs - strategy.successful_txs);

            for (_sender_addr, sender_key, sender_utxos) in top_accounts {
                if !strategy.should_continue() {
                    break;
                }

                // Filter out UTXOs that have already been spent in this block
                let available_utxos: IndexSet<UtxoInfo> = sender_utxos
                    .into_iter()
                    .filter(|utxo| !spent_in_block.contains(&utxo.outpoint))
                    .collect();

                // Skip if no UTXOs available
                if available_utxos.is_empty() {
                    continue;
                }

                // Use strategy to choose pattern and UTXO selection
                let pattern = strategy.choose_pattern(available_utxos.len(), &params, rng);
                let (selection_criteria, utxos_to_select) =
                    strategy.choose_utxo_strategy(pattern, available_utxos.len(), &params, rng);

                let selected_utxos = state.select_utxos_by_criteria(
                    &available_utxos,
                    selection_criteria,
                    utxos_to_select,
                );

                if let Some(tx) = generate_transaction(
                    &sender_key,
                    pattern,
                    selected_utxos.clone(),
                    &receivers_pool,
                    rng,
                    &params,
                ) {
                    for utxo in selected_utxos {
                        spent_in_block.insert(utxo.outpoint);
                    }

                    transactions.push(tx);
                    strategy.record_success();
                }
            }
        }

        debug!(
            "transaction count at the end of the block {} is : {}",
            &height,
            transactions.len()
        );

        if transactions.len() == 1 {
            warn!(
                "No viable payment transactions could be built for block {}. Falling back to coinbase-only block.",
                height
            );
        }

        // Assemble block
        let mut block = Block::new(
            transactions,
            prev.block_hash(),
            height,
            hash_threshold,
            timestamp,
            1, // version
        );

        // Mine nonce in parallel using ordered scan for a deterministic result between runs.
        mine_block_parallel_ordered(&mut block, MINING_ATTEMPT_CAP)?;

        Ok(block)
    }
}

fn generation_now_secs() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|e| e.to_string())
}

fn generation_timestamp(
    parent_timestamp: u64,
    base_time_secs: u64,
    variance_secs: u64,
    median_time: u64,
    now_secs: u64,
) -> Result<u64, String> {
    let increment = base_time_secs
        .checked_add(variance_secs)
        .ok_or_else(|| "generation interval overflow".to_string())?;
    let scheduled = parent_timestamp
        .checked_add(increment)
        .ok_or_else(|| "generation timestamp overflow".to_string())?;

    let minimum = median_time
        .checked_add(1)
        .ok_or_else(|| "median time past leaves no valid timestamp".to_string())?;
    let timestamp = scheduled.max(minimum);

    if timestamp > now_secs.saturating_add(MAX_FUTURE_BLOCK_TIME_SECS) {
        return Err("generated timestamp exceeds future time limit".into());
    }

    Ok(timestamp)
}

/// Hard cap on nonces tried for one generated block: keeps local generation
/// bounded for an excessively hard configured threshold.
const MINING_ATTEMPT_CAP: u32 = 50_000_000;

/// Parallel, ordered nonce search using rayon.
/// Scans `0..max_attempts` in increasing order; the first matching nonce is chosen (deterministic).
fn mine_block_parallel_ordered(block: &mut Block, max_attempts: u32) -> Result<(), String> {
    let threshold = block.header.hash_threshold;

    // Take a snapshot of the header without changing the shared `block` during the search.
    let base_header = block.header;

    let found = (0u32..max_attempts).into_par_iter().find_first(|&nonce| {
        let mut hdr = base_header;
        hdr.nonce = nonce;

        let header_bytes = hdr.to_hash_bytes();
        let h = BlockHash::new(&header_bytes);
        h.meets_threshold(&threshold)
    });

    if let Some(nonce) = found {
        block.header.nonce = nonce;
        return Ok(());
    }

    Err(format!(
        "failed to mine block at height {} within {} attempts (hash_threshold: {})",
        block.header.height, max_attempts, threshold
    ))
}
