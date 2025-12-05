use crate::chaingen::seed;
use crate::chaingen::seed::SeedValue;
use serde::Deserialize;
use std::path::PathBuf;
use stryi_core::PrivateKey;
use stryi_core::address::AccountAddress;
use tracing::trace;

/// Top-level config for chain-generator tool.
/// Includes two tables : `[chain]` and `[blocks]` for chain-wide and block-gen settings correspondingly.
///
/// **NOTE**: Some chain parameters (like `chain_name` and `chain_id`) and Consensus-critical consts
/// are expected to be set in the genesis JSON file, not here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChainGenConfig {
    /// Chain-wide settings.
    pub chain: ChainSettings,

    /// Block-generation settings.
    pub blocks: BlocksSettings,
}

/// How generated blocks are persisted/applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceMode {
    ConsensusEngine,
    DirectInsert,
}

impl std::str::FromStr for PersistenceMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "consensus_engine" => Ok(PersistenceMode::ConsensusEngine),
            "direct_insert" => Ok(PersistenceMode::DirectInsert),
            other => Err(format!(
                "invalid persistence_mode: `{}`. Allowed values: \"consensus_engine\", \"direct_insert\"",
                other
            )),
        }
    }
}

impl TryFrom<String> for PersistenceMode {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.as_str().parse()
    }
}

impl<'de> Deserialize<'de> for PersistenceMode {
    fn deserialize<D>(deserializer: D) -> Result<PersistenceMode, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        PersistenceMode::try_from(s).map_err(serde::de::Error::custom)
    }
}

/// Chain-wide settings: generation scope, seeding, and output/genesis paths.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChainSettings {
    /// Total number of blocks to generate (including genesis).
    pub num_blocks: u64,

    /// Seed strategy: either a single seed (`42`) or multiple ranges.
    pub seed: SeedValue,

    /// Where to initialize the chain’s data.
    pub output_path: PathBuf,

    /// Path to genesis JSON (same format as used by `stryi-node`).
    pub genesis_path: PathBuf,

    /// How generated blocks are persisted/applied.
    /// Allowed values: "consensus_engine", "direct_insert"
    /// Default: "consensus_engine"
    /// - "consensus_engine" — Build and feed blocks into StryiConsensusEngine; validates and applies blocks as a real node would. Recommended for most tests and benchmarks.
    /// - "direct_insert" — Write blocks directly to storage without consensus validation. Faster, useful for low-level tests, but may create chains that real nodes reject. Use only when you know what you are doing.
    pub persistence_mode: PersistenceMode,
}

/// Block-generation rules for timestamps, miner, and synthetic traffic.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BlocksSettings {
    /// Private key of the account that has funds (e.g. from genesis or just chain activity) to distribute them evenly for generation purposes.
    pub funding_key: PrivateKey,

    /// Average time between blocks, in seconds (used for header timestamps).
    pub average_block_time_secs: u64,

    /// Miner address used to mine all generated blocks.
    pub miner_address: String,

    /// Minimum transactions per block (inclusive).
    pub min_transactions_per_block: u32,

    /// Maximum transactions per block (inclusive).
    pub max_transactions_per_block: u32,

    /// Number of unique active addresses participating in transfers.
    pub active_addresses_count: u32,

    /// Whether to insert undo data for each block.
    /// This will increase the size of the generated chain, but allows testing of reorg-related logic
    pub need_undo: bool,
}

impl ChainGenConfig {
    /// Minimum allowed active addresses count.
    /// Generate thousands of blocks with only a few active addresses is not very interesting and probably, quite problematic, + there is no need for that even for testing purposes.
    const MIN_ACTIVE_ADDRESSES: u32 = 10;

    /// Validate the config, returning `Ok(())` if valid, or `Err(String)` with a descriptive message if invalid.
    /// Validates that all the fields are in a reasonable range, and that paths exist + some other cross-field checks.
    pub(crate) fn validate(&self) -> Result<(), String> {
        // validate numeric fields

        if self.chain.num_blocks == 0 {
            return Err("num_blocks must be > 0".to_string());
        }
        if self.blocks.average_block_time_secs == 0 {
            return Err("average_block_time_secs must be > 0".to_string());
        }

        if self.blocks.min_transactions_per_block > self.blocks.max_transactions_per_block {
            return Err(
                "min_transactions_per_block cannot be greater than max_transactions_per_block"
                    .to_string(),
            );
        }
        if self.blocks.active_addresses_count < Self::MIN_ACTIVE_ADDRESSES {
            return Err(format!(
                "active_addresses_count must be >= {}",
                Self::MIN_ACTIVE_ADDRESSES
            ));
        }

        // Transactions per block vs active addresses cross-validation
        // This is equivalent of `min_transactions_per_block / 1.5 > active_addresses_count` but avoids float operations
        if self.blocks.min_transactions_per_block * 2 > self.blocks.active_addresses_count * 3 {
            return Err(
                "active_addresses_count is too low for the requested min_transactions_per_block, try doing at least x1.5 of min_transactions_per_block"
                    .to_string(),
            );
        }

        trace!("Persistence mode set to {:?}", &self.chain.persistence_mode);

        // validate seed ranges via helper
        seed::validate(&self.chain.seed, self.chain.num_blocks)?;

        if !self.chain.genesis_path.is_file() {
            return Err(format!(
                "genesis_path does not exist or is not a file: {}",
                self.chain.genesis_path.display()
            ));
        }

        // check that miner address is valid stryi address
        if let Err(e) = AccountAddress::from_hash_string(&self.blocks.miner_address) {
            return Err(format!("miner_address is not a valid address: {e}"));
        }

        Ok(())
    }
}
