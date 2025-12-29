//! Lean bootstrap for saving this datadir to a chosen genesis (SSOT in storage::meta).
//!
//! Responsibilities:
//!   - Probe storage meta to learn whether the datadir is already saved/initialized.
//!   - Validate a candidate genesis block with strict, deterministic invariants.
//!   - Ask for a concise confirmation and atomically save into `storage::meta`.
//!
//! Out of scope (caller must do right after a successful save):
//!   - Commit the genesis block into storage partitions (blocks/heights).
//!   - Start IBD / enable external services.
//!
//!

use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use colored::Colorize;
use comfy_table::Table;
use stryi_core::block::Block;
use stryi_core::consensus::ConsensusConsts;
use stryi_storage::{StorageStatus, validate_genesis, write_status_atomic};

use crate::error::StryiNodeError;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SaveOutcome {
    AlreadySaved,
    SavedNow,
}

#[derive(Clone)]
pub struct GenesisBootstrap {
    /// Root datadir
    datadir: PathBuf,
}

impl GenesisBootstrap {
    /// Create a new bootstrap bound to the given datadir.
    pub fn new(datadir: PathBuf) -> Self {
        Self { datadir }
    }

    /// Probe current meta status from storage.
    pub fn probe_meta(&self) -> Result<StorageStatus, StryiNodeError> {
        StorageStatus::from_path(&self.datadir)
            .map_err(|e| StryiNodeError::other(format!("failed to probe storage meta: {e}")))
    }
    /// Show a concise summary and atomically save into storage::meta.
    ///
    /// Arguments:
    /// - `block`: validated candidate genesis block.
    /// - `chain_name`: human-readable network name for meta (current meta schema).
    /// - `protocol_version`: protocol version for meta (current meta schema).
    /// - `origin`: optional source of this genesis so the user can verify it, e.g. {PeerId} or "local".
    /// - `accept_genesis`: if true, skip interactive confirmation and auto-accept the genesis.
    ///   Intended for non-interactive environments (e.g. E2E tests, CI, Docker).
    ///
    /// Returns:
    /// - `AlreadySaved` if meta already indicates an initialized datadir.
    /// - `SavedNow` after successfully writing meta.
    ///
    /// Note: committing the block into storage partitions is the caller's responsibility
    /// and should happen immediately after this returns `SavedNow`.
    pub fn confirm_and_save(
        &self,
        block: &Block,
        chain_name: &str,
        protocol_version: u64,
        origin: Option<&str>,
        accept_genesis: bool,
    ) -> Result<SaveOutcome, StryiNodeError> {

        if accept_genesis {
            println!(
                "{} Genesis auto-accepted via --accept-genesis",
                "INFO".green().bold()
            );
        }

        // If meta is already initialized, do nothing.
        match self.probe_meta()? {
            StorageStatus::Initialized { .. } => return Ok(SaveOutcome::AlreadySaved),
            StorageStatus::Corrupted { reason } => {
                return Err(StryiNodeError::other(format!(
                    "storage meta is corrupted: {reason}"
                )));
            }
            StorageStatus::NoGenesis => { /* continue */ }
        }

        // Validate candidate invariants before asking the user.
        validate_genesis(block).map_err(|e| {
            StryiNodeError::other(format!("provided genesis candidate is invalid: {e}"))
        })?;

        // Present a concise summary
        self.print_block_summary(block);

        // Confirmation prompt (skipped if --accept-genesis is provided)
        if !accept_genesis {
            let mut input = String::new();

            println!(
                "{} You are about to accept the shown genesis for this node.\n \
            This choice is permanent unless you reinitialize the node.",
                "WARNING".on_yellow().black().bold(),
            );
            println!(
                "Before continuing, verify origin, all the header values (height, state/Merkle root) \n \
            and EVERY allocation (recipient -> amount)."
            );
            println!("Origin: {}", origin.unwrap_or("<unknown>").bold());
            println!("Type 'yes' to confirm; anything else cancels.");
            print!("Accept genesis (yes/no): ");

            io::stdout()
                .flush()
                .map_err(|e| StryiNodeError::other(format!("stdout flush failed: {e}")))?;
            io::stdin()
                .read_line(&mut input)
                .map_err(|e| StryiNodeError::other(format!("stdin read failed: {e}")))?;

            if !input.trim().eq_ignore_ascii_case("yes") {
                return Err(StryiNodeError::other("user rejected the provided genesis"));
            }
        }

        // Atomically save into storage::meta
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let status = StorageStatus::Initialized {
            chain_name: chain_name.to_string(),
            protocol_version,
            created_unix_ts: now,
        };

        write_status_atomic(&self.datadir, status)
            .map_err(|e| StryiNodeError::other(format!("failed to write storage meta: {e}")))?;

        // NOTE: committing the same `block` into storage partitions and initializing stats at height 0
        // must be performed by the caller immediately after this function returns.
        //
        // TODO: introduce quorum of independent sources before the confirmation prompt.

        Ok(SaveOutcome::SavedNow)
    }


    fn print_consensus_consts(cs: &ConsensusConsts) {
        use comfy_table::Table;

        let mut table = Table::new();
        table.set_header(vec!["Consensus Parameter", "Value"]);
        table.set_width(73);

        table.add_row(vec![
            "difficulty_adjustment_interval_blocks".into(),
            cs.difficulty_adjustment_interval_blocks.to_string(),
        ]);
        table.add_row(vec![
            "initial_subsidy".into(),
            cs.initial_subsidy.to_string(),
        ]);
        table.add_row(vec!["decay_interval".into(), cs.decay_interval.to_string()]);
        table.add_row(vec!["decay_step".into(), cs.decay_step.to_string()]);

        println!(
            "{}",
            "========== CONSENSUS CONSTANTS PARAMETERS =========="
                .blue()
                .bold()
        );
        println!("{table}");
    }

    /// Print a concise header summary and a table of initial balances from the
    /// single genesis transaction (no walls of text).
    fn print_block_summary(&self, block: &Block) {
        let header_hash = block.block_hash();

        // ===== Header =====
        println!();
        println!("{}", "======== GENESIS CANDIDATE ========".blue().bold());
        println!("Hash             : {header_hash}");
        println!("Version          : {}", block.header.version);
        println!("Timestamp (unix) : {}", block.header.timestamp);
        println!("Merkle root      : {}", block.header.merkle_root_hash);
        // only print previous hash if not genesis, because it is all zeros
        if !block.is_genesis() {
            println!("Prev block hash  : {}", block.header.previous_block_hash);
        }

        // ===== Consensus Consts =====
        if let Some(gs) = &block.header.genesis_state {
            Self::print_consensus_consts(&gs.consensus_consts);
        }

        // ===== Body: allocation table =====
        println!(
            "{}",
            "==================== BLOCK BODY ===================="
                .blue()
                .bold()
        );

        let mut table = Table::new();
        table.set_header(vec!["Account Address", "OutPoint ID", "Initial Balance"]);

        // Keep the same width you used before so borders align with the header banner.
        table.set_width(73);

        // We validated earlier that there is exactly one transaction of kind Genesis.
        // Still, handle unexpected shapes gracefully.
        if let Some(tx) = block.data.transactions.first() {
            for (id, tx_out) in tx.data.outputs.iter().enumerate() {
                table.add_row(vec![
                    tx_out.recipient.to_string(),
                    id.to_string(),
                    tx_out.value.to_string(),
                ]);
            }
        } else {
            // Fallback: no txs (should not happen if validate_candidate was called).
            table.add_row(vec!["<no outputs>".to_string(), "0".to_string()]);
        }

        println!("{table}");
        println!(
            "{}",
            "==================================================="
                .blue()
                .bold()
        );
    }
}
