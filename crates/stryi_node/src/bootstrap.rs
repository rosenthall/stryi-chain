//! Bootstrap for saving this datadir to a chosen genesis.
//!
//! Responsibilities:
//!   - Probe storage metadata to learn whether the datadir is initialized.
//!   - Validate a candidate genesis block with strict, deterministic invariants.
//!   - Ask for confirmation and atomically save metadata, genesis and allocations.
//!
//! Starting IBD and enabling external services remain the caller's responsibility.

use std::io;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use colored::Colorize;
use comfy_table::Table;
use stryi_core::block::Block;
use stryi_core::consensus::ConsensusConsts;
use stryi_core::storage::{
    ChainMetadata, ChainTransition, FullNodeStorage, StorageReader, StorageStats,
};
use stryi_storage::StryiStorage;
use stryi_storage::{STORAGE_SCHEMA_VERSION, StorageStatus, validate_genesis};

use crate::error::StryiNodeError;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SaveOutcome {
    AlreadySaved,
    SavedNow,
}

#[derive(Clone)]
pub struct GenesisBootstrap {
    storage: StryiStorage,
}

impl GenesisBootstrap {
    /// Create a new bootstrap bound to the open storage.
    pub fn new(storage: StryiStorage) -> Self {
        Self { storage }
    }

    /// Probe current meta status from storage.
    pub fn probe_meta(&self) -> Result<StorageStatus, StryiNodeError> {
        self.storage
            .status()
            .map_err(|e| StryiNodeError::other(format!("failed to probe storage meta: {e}")))
    }
    /// Show a concise summary and atomically save the chain's initial state.
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
    /// - `AlreadySaved` when the stored genesis content and chain identity match.
    /// - `SavedNow` after successfully committing metadata, genesis and allocations.
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

        // Validate the candidate before any durable effect, so an initialized
        // datadir cannot turn the AlreadySaved shortcut into a validation skip.
        validate_genesis(block).map_err(|e| {
            StryiNodeError::other(format!("provided genesis candidate is invalid: {e}"))
        })?;

        // An initialized datadir must still match the supplied genesis and chain identity.
        if matches!(self.probe_meta()?, StorageStatus::Initialized { .. }) {
            self.verify_saved_genesis(block, chain_name, protocol_version)?;

            return Ok(SaveOutcome::AlreadySaved);
        }

        // Present a concise summary
        self.print_block_summary(block);

        if !accept_genesis {
            Self::confirm_genesis(origin)?;
        }

        self.save_genesis(block, chain_name, protocol_version)?;

        Ok(SaveOutcome::SavedNow)
    }

    fn verify_saved_genesis(
        &self,
        block: &Block,
        chain_name: &str,
        protocol_version: u64,
    ) -> Result<(), StryiNodeError> {
        let saved_metadata = self
            .storage
            .read()?
            .metadata()?
            .ok_or_else(|| StryiNodeError::other("initialized storage has no metadata"))?;

        // Supply the expected identity without changing the stored creation timestamp.
        let expected_metadata = ChainMetadata {
            chain_name: chain_name.to_string(),
            protocol_version,
            ..saved_metadata
        };

        self.storage.apply_transition(ChainTransition::Genesis {
            block,
            metadata: &expected_metadata,
        })?;

        Ok(())
    }

    fn confirm_genesis(origin: Option<&str>) -> Result<(), StryiNodeError> {
        // TODO: introduce quorum of independent sources before the confirmation prompt.
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

        Ok(())
    }

    fn save_genesis(
        &self,
        block: &Block,
        chain_name: &str,
        protocol_version: u64,
    ) -> Result<(), StryiNodeError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| StryiNodeError::other(format!("cannot read local clock: {e}")))?
            .as_secs();

        let metadata = ChainMetadata {
            schema_version: STORAGE_SCHEMA_VERSION,
            chain_name: chain_name.to_string(),
            protocol_version,
            created_unix_ts: i64::try_from(now)
                .map_err(|_| StryiNodeError::other("local clock exceeds metadata range"))?,
        };

        self.storage.apply_transition(ChainTransition::Genesis {
            block,
            metadata: &metadata,
        })?;

        Ok(())
    }

    fn print_consensus_consts(cs: &ConsensusConsts) {
        use comfy_table::Table;

        let mut table = Table::new();
        table.set_header(vec!["Consensus Parameter", "Value"]);
        table.set_width(73);

        table.add_row(vec![
            "pow.target_spacing_secs".into(),
            cs.pow.target_spacing_secs.to_string(),
        ]);
        table.add_row(vec![
            "pow.half_life_secs".into(),
            cs.pow.half_life_secs.to_string(),
        ]);
        table.add_row(vec![
            "pow.initial_hash_threshold".into(),
            cs.pow.initial_hash_threshold.to_string(),
        ]);
        table.add_row(vec![
            "pow.max_hash_threshold".into(),
            cs.pow.max_hash_threshold.to_string(),
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
            "===================== BLOCK BODY ====================="
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
            // Fallback: no txs (should not happen after validate_genesis).
            table.add_row(vec!["<no outputs>".to_string(), "0".into(), "0".into()]);
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
