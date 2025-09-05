use std::io::Write;
use std::path::PathBuf;
use bincode::config::standard;
use bincode::serde::{decode_from_slice, encode_to_vec};
use colored::Colorize;
use tracing::{error, warn};
use stryi_core::block::Block;
use crate::error::StryiNodeError;

const GENESIS_BLOCK_FILE_NAME: &str = "genesis.dat";



#[derive(Debug)]
pub struct GenesisManager {
    /// Path to the blockchain storage directory
    /// We use this path to store genesis block and its hash.
    datadir: PathBuf,

}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GenesisChoice {
    Local,
    Network,
}

impl GenesisManager {
    /// Creates a new GenesisManager with the specified data directory.
    pub fn new(datadir: PathBuf) -> Self {
        Self { datadir }
    }

    /// Pretty print a brief summary of a block (header + coinbase outputs table).
    fn print_block_summary(&self, block: &Block, title: &str) {
        println!("\n{}", format!("================ {} ================", title).blue().bold());
        println!("Version: {}", block.header.version);
        println!("Timestamp: {}", block.header.timestamp);
        println!("Height: {}", block.header.height);
        println!("Difficulty bits: {}", block.header.difficulty_bits);
        println!("Nonce: {}", block.header.nonce);
        println!("Merkle root hash: {}", block.header.merkle_root_hash);
        println!("Previous block hash: {}", block.header.previous_block_hash);
        println!("Is genesis: {}", block.header.is_genesis);

        println!("{}", "==================== BLOCK BODY ====================".blue().bold());

        let mut table = comfy_table::Table::new();
        table.set_header(vec!["Account Address", "Initial Balance"]);
        table.set_width(73);

        assert!(
            !block.data.transactions.is_empty() && block.data.transactions.len() == 1,
            "Genesis block must contain exactly one transaction."
        );

        for tx_out in &block.data.transactions[0].data.outputs {
            table.add_row(vec![tx_out.recipient.to_string(), tx_out.value.to_string()]);
        }

        println!("{table}");
        println!("{}", "====================================================".blue().bold());
    }

    /// Ask user to confirm caching the given genesis block; save if confirmed.
    pub fn prompt_and_cache_block(&self, block: &Block) -> Result<(), StryiNodeError> {
        let genesis_block_path = self.genesis_block_path();
        println!("Genesis block file will be saved to: {}", genesis_block_path.display());

        self.print_block_summary(block, "CANDIDATE GENESIS");

        // Ask the user for confirmation to save the genesis block
        let mut input = String::new();
        println!("======================{}=======================", "WARNING".on_yellow().white().bold());
        println!("MAKE SURE THAT YOU TRUST THIS PEER AND ITS GENESIS BLOCK!");
        println!("Bad genesis may lead to incompatible chain or malicious behaviour from the peer which gave it.");

        println!("Changing later requires wiping entire data directory.");
        println!("Proceed only if you trust this peer and verified the header.");
        println!("Type 'yes' to accept; anything else aborts.");

        print!("I DO TRUST THIS PEER (yes/no): ");
        std::io::stdout().flush()?;

        std::io::stdout().flush()?;
        std::io::stdin().read_line(&mut input)?;
        if input.trim().eq_ignore_ascii_case("yes") {
            self.save_block(block)
        } else {
            Err(StryiNodeError::other(
                "User refused network genesis. Change the trusted peer and reconnect.",
            ))
        }
    }

    /// Ask the user to choose between local and network genesis.
    pub fn prompt_choose_between(
        &self,
        local: &Block,
        network: &Block,
    ) -> Result<GenesisChoice, StryiNodeError> {
        println!("\nGenesis mismatch detected. Please choose which one to use:");
        self.print_block_summary(local, "LOCAL GENESIS");
        self.print_block_summary(network, "NETWORK GENESIS");

        loop {
            let mut input = String::new();
            print!("Type 'local' to keep local genesis or 'network' to replace with the network one: ");
            std::io::stdout().flush()?;
            std::io::stdin().read_line(&mut input)?;

            let trimmed = input.trim().to_ascii_lowercase();
            match trimmed.as_str() {
                "local" => return Ok(GenesisChoice::Local),
                "network" => return Ok(GenesisChoice::Network),
                _ => {
                    println!("Invalid choice: '{}'. Please type 'local' or 'network'.", trimmed);
                }
            }
        }
    }

    /// Returns the path to the genesis block file.
    fn genesis_block_path(&self) -> PathBuf {
        self.datadir.join(GENESIS_BLOCK_FILE_NAME)
    }

    /// Atomic caching of the genesis block.
    pub fn save_block(&self, block: &Block) -> Result<(), StryiNodeError> {
        let tmp = self.genesis_block_path().with_extension("tmp");
        let encoded = encode_to_vec(block, standard())
            .map_err(|e| StryiNodeError::other(format!("Failed to encode genesis: {e}")))?;
        std::fs::write(&tmp, encoded)?;

        // POSIX-atomic rename
        std::fs::rename(&tmp, self.genesis_block_path())?;
        Ok(())
    }

    /// Load the genesis block from the cached file.
    /// Returns None and logs if it can't read or deserialize.
    pub fn load_saved(&self) -> Option<Block> {
        let path = self.genesis_block_path();

        let bytes = std::fs::read(&path)
            .map_err(|e| warn!("Can't load genesis from {}: {:?} First run?", path.display(), e))
            .ok()?; // Result<Vec<u8>, _> -> Option<Vec<u8>>

        decode_from_slice::<Block, _>(&bytes, standard())
            .map(|(b, _)| b)
            .map_err(|e| error!("Failed to deserialize genesis from {}: {:?}", path.display(), e))
            .ok()
    }
}