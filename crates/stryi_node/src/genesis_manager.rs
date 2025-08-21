use std::path::PathBuf;
use bincode::config::standard;
use colored::Colorize;
use stryi_core::block::Block;
use crate::error::StryiNodeError;

const GENESIS_BLOCK_FILE_NAME: &str = "genesis.dat";



#[derive(Debug)]
pub struct GenesisManager {
    /// Path to the blockchain storage directory
    /// We use this path to store genesis block and its hash.
    datadir: PathBuf,

}


impl GenesisManager {
    /// Creates a new GenesisManager with the specified data directory.
    pub fn new(datadir: PathBuf) -> Self {
        Self { datadir }
    }


    /// Prompts the user, describes the provided genesis block file and asks for confirmation to save it.
    pub fn prompt_and_cache_block(&self, block: &Block) -> Result<(), StryiNodeError> {
        let genesis_block_path = self.genesis_block_path();


        println!("Genesis block file will be saved to: {}", genesis_block_path.display());


        // describe the header field by field
        println!("{}", "==============================BLOCK HEADER===============================".blue().bold());

        println!("Version: {}", block.header.version);
        println!("Timestamp: {}", block.header.timestamp);
        println!("Height: {}", block.header.height);
        println!("Difficulty bits: {}", block.header.difficulty_bits);
        println!("Nonce: {}", block.header.nonce);
        println!("Merkle root hash: {}", block.header.merkle_root_hash);
        println!("Previous block hash: {}", block.header.previous_block_hash);
        println!("Is genesis: {}", block.header.is_genesis);

        println!("{}", "===============================BLOCK BODY================================".blue().bold());

        // Print each the genesis transaction's output with corresponding account address and value in a table
        // It will basically be a list of accounts with their initial balances

        let mut table = comfy_table::Table::new();

        table.set_header(vec!["Account Address", "Initial Balance"]);
        table.set_width(73); // that's len of this "===..==" separator string


        assert!(!block.data.transactions.is_empty() && block.data.transactions.len() == 1, "Genesis block must contain exactly one transaction.");


        for tx_out in &block.data.transactions[0].data.outputs {
            table.add_row(vec![tx_out.recipient.to_string(), tx_out.value.to_string()]);
        }

        println!("{table}");

        println!("{}", "=========================================================================".blue().bold());



        // Ask the user for confirmation to save the genesis block
        let mut input = String::new();
        println!("Do you *really* want to use this genesis block? It will be cached locally and used in future. (yes/no)");
        std::io::stdin().read_line(&mut input)?;

        if input.trim().eq_ignore_ascii_case("yes") {
            self.cache_block(block)
        } else {
            Err(StryiNodeError::other("Genesis block caching aborted by user."))
        }
    }


    /// Checks if the genesis block file exists in the data directory.
    pub fn has_cached(&self) -> bool {
        self.genesis_block_path().is_file()
    }


    /// Returns the path to the genesis block file.
    pub fn genesis_block_path(&self) -> PathBuf {
        self.datadir.join(GENESIS_BLOCK_FILE_NAME)
    }


    /// Atomic caching of the genesis block.
    pub fn cache_block(&self, block: &Block) -> Result<(), StryiNodeError> {
        let tmp = self.genesis_block_path().with_extension("tmp");
        std::fs::write(&tmp, bincode::serde::encode_to_vec(block, standard()).map_err(StryiNodeError::other)?)?;
        std::fs::rename(&tmp, self.genesis_block_path())?; // atomic on POSIX
        Ok(())
    }

    /// Load the genesis block from the cached file.
    pub fn load_cached(&self) -> Result<Block, StryiNodeError> {
        let bytes = std::fs::read(self.genesis_block_path())?;
        Ok(bincode::serde::decode_from_slice(&bytes, standard()).map_err(StryiNodeError::other)?.0)
    }


}