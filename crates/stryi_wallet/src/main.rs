#![allow(incomplete_features)]
#![feature(generic_const_exprs)]

mod api_client;
mod cmd;
mod keys;
mod repl;
mod tx_builder;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;

use crate::api_client::{TransactionKindResponse, TransactionResponse};
use stryi_core::address::AccountAddress;
use stryi_core::block::Block;
use stryi_core::transactions::TransactionKind;

#[derive(Parser)]
#[command(name = "stryi-wallet", version, about = "StryiChain CLI Wallet")]
struct Cli {
    #[arg(long, global = true, default_value_t = default_wallet_path())]
    wallet_path: String,

    #[arg(long, global = true, default_value = "http://localhost:5556")]
    node: String,

    #[arg(short = 'i', long = "interactive")]
    interactive: bool,

    #[command(subcommand)]
    cmd: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new wallet file with one key pair.
    Init,

    /// Generate a new key pair and add it to the wallet.
    Generate {
        #[arg(long, default_value = "default")]
        label: String,
    },

    /// Import a private key from hex.
    Import {
        #[arg(long)]
        key: String,

        #[arg(long, default_value = "imported")]
        label: String,
    },

    /// List all addresses in the wallet.
    List {
        /// Also display private keys.
        #[arg(long)]
        show_keys: bool,
    },

    /// Query the balance for an address from the node.
    Balance {
        #[arg(long)]
        address: String,
    },

    /// Build, sign, and submit a payment transaction.
    Send {
        #[arg(long)]
        from: String,

        #[arg(long)]
        to: String,

        #[arg(long)]
        amount: u64,
    },

    /// Delete a key from the wallet.
    Delete {
        #[arg(long)]
        address: String,
    },

    /// Rename a key in the wallet.
    Rename {
        #[arg(long)]
        address: String,

        #[arg(long)]
        label: String,
    },

    /// Query a block by height or hash.
    Block {
        /// Block height (u64) or block hash (Bx...).
        #[arg(long)]
        id: String,
    },

    /// Query a transaction by hash.
    Tx {
        /// Transaction hash (Tx...).
        #[arg(long)]
        id: String,
    },

    /// Query and display the connected node's state.
    #[command(name = "nodestate")]
    NodeState,
}

fn default_wallet_path() -> String {
    dirs::home_dir()
        .map(|h| h.join(".stryi").join("wallet.json"))
        .unwrap_or_else(|| PathBuf::from("wallet.json"))
        .to_string_lossy()
        .to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let wallet_path = PathBuf::from(&cli.wallet_path);

    // run repl if asked
    if cli.interactive {
        return repl::run_interactive(&wallet_path, &cli.node).await;
    }

    let Some(cmd) = cli.cmd else {
        anyhow::bail!("no command given. Use --help or -i for interactive mode.");
    };

    match cmd {
        Command::Init => cmd::cmd_init(&wallet_path)?,
        Command::Generate { label } => cmd::cmd_generate(&wallet_path, &label)?,
        Command::Import { key, label } => cmd::cmd_import(&wallet_path, &key, &label)?,
        Command::List { show_keys } => cmd::cmd_list(&wallet_path, show_keys)?,
        Command::Balance { address } => {
            let addr = AccountAddress::from_hash_string(&address).context("invalid address")?;
            cmd::cmd_balance(&cli.node, &addr).await?
        }
        Command::Delete { address } => {
            let addr = AccountAddress::from_hash_string(&address).context("invalid address")?;
            cmd::cmd_delete(&wallet_path, &addr)?
        }
        Command::Rename { address, label } => {
            let addr = AccountAddress::from_hash_string(&address).context("invalid address")?;
            cmd::cmd_rename(&wallet_path, &addr, &label)?
        }
        Command::Block { id } => cmd::cmd_block(&cli.node, &id).await?,
        Command::Tx { id } => cmd::cmd_tx(&cli.node, &id).await?,
        Command::Send { from, to, amount } => {
            let from_addr =
                AccountAddress::from_hash_string(&from).context("invalid 'from' address")?;
            let to_addr = AccountAddress::from_hash_string(&to).context("invalid 'to' address")?;
            cmd::cmd_send(&wallet_path, &cli.node, &from_addr, &to_addr, amount).await?
        }
        Command::NodeState => cmd::cmd_nodestate(&cli.node).await?,
    }

    Ok(())
}

enum QueryId<'a> {
    Height(u64),
    BlockHash(&'a str),
    TxHash(&'a str),
    Unknown(&'a str),
}

fn classify_identifier(input: &str) -> QueryId<'_> {
    if let Ok(h) = input.parse::<u64>() {
        return QueryId::Height(h);
    }
    if input.starts_with("Bx") {
        return QueryId::BlockHash(input);
    }
    if input.starts_with("Tx") {
        return QueryId::TxHash(input);
    }
    QueryId::Unknown(input)
}

fn print_block_detail(hash: &str, block: &Block) {
    let header = &block.header;
    let txs = &block.data.transactions;
    let rule = "-".repeat(68);

    println!();
    println!("  {}", format!("Block #{}", header.height).bold());
    println!("  {}", rule.cyan());
    println!("  {:<20} {}", "Hash:".bold(), hash.cyan());
    println!(
        "  {:<20} {}",
        "Prev hash:".bold(),
        header.previous_block_hash
    );
    println!(
        "  {:<20} {}",
        "Merkle root:".bold(),
        header.merkle_root_hash
    );
    println!("  {:<20} {}", "Version:".bold(), header.version);
    println!(
        "  {:<20} {}",
        "Height:".bold(),
        header.height.to_string().cyan()
    );
    println!("  {:<20} {}", "Difficulty:".bold(), header.difficulty_bits);
    println!("  {:<20} {}", "Nonce:".bold(), header.nonce);
    println!("  {:<20} {}", "Timestamp:".bold(), header.timestamp);

    if let Some(ref gs) = header.genesis_state {
        println!("  {:<20} {}", "Genesis:".bold(), "yes".green());
        let c = &gs.consensus_consts;
        println!("    {:<18} {}", "Init subsidy:".dimmed(), c.initial_subsidy);
        println!(
            "    {:<18} {}",
            "Decay interval:".dimmed(),
            c.decay_interval
        );
        println!("    {:<18} {}", "Decay step:".dimmed(), c.decay_step);
        println!(
            "    {:<18} {}",
            "Diff adj blocks:".dimmed(),
            c.difficulty_adjustment_interval_blocks
        );
    }

    println!("  {}", rule.cyan());
    println!(
        "  {} {}",
        "Transactions:".bold(),
        txs.len().to_string().cyan()
    );
    println!("  {}", rule.cyan());

    for (i, tx) in txs.iter().enumerate() {
        let tx_hash = tx.data.hash();
        let kind_label = match tx.data.kind {
            TransactionKind::Coinbase => "Coinbase".yellow(),
            TransactionKind::Genesis => "Genesis".green(),
            TransactionKind::Payment => "Payment".white(),
        };

        println!(
            "  {} {} {}",
            format!("[{}]", i).cyan().bold(),
            kind_label,
            tx_hash.to_string().dimmed()
        );

        if tx.data.inputs.is_empty() {
            println!("    {}", "No inputs (coinbase/genesis)".dimmed());
        } else {
            for inp in &tx.data.inputs {
                println!(
                    "    {} {}:{}",
                    "in:".dimmed(),
                    inp.previous_output.txid.to_string().dimmed(),
                    inp.previous_output.vout
                );
            }
        }

        for out in &tx.data.outputs {
            println!(
                "    {} {} -> {}",
                "out:".dimmed(),
                out.value.to_string().green(),
                out.recipient.to_string().cyan()
            );
        }

        if i < txs.len() - 1 {
            println!();
        }
    }

    println!("  {}", rule.cyan());
    println!();
}

pub(crate) fn print_transaction_response_detail(hash: &str, tx: &TransactionResponse) {
    let rule = "-".repeat(68);
    let kind_label = match tx.data.kind {
        TransactionKindResponse::Coinbase => "Coinbase".yellow(),
        TransactionKindResponse::Genesis => "Genesis".green(),
        TransactionKindResponse::Payment => "Payment".white(),
    };

    println!("  {}", "Transaction".bold());
    println!("  {}", rule.cyan());
    println!("  {:<20} {}", "Hash:".bold(), hash.cyan());
    println!("  {:<20} {}", "Kind:".bold(), kind_label);
    println!("  {:<20} {}", "Version:".bold(), tx.data.version);
    println!("  {:<20} {}", "Inputs:".bold(), tx.data.inputs.len());
    println!("  {:<20} {}", "Outputs:".bold(), tx.data.outputs.len());
    println!("  {}", rule.cyan());

    if tx.data.inputs.is_empty() {
        println!("  {}", "No inputs".dimmed());
    } else {
        for input in &tx.data.inputs {
            println!(
                "  {} {}:{}",
                "in:".dimmed(),
                input.previous_output.txid.to_string().dimmed(),
                input.previous_output.vout
            );
        }
    }

    for output in &tx.data.outputs {
        println!(
            "  {} {} -> {}",
            "out:".dimmed(),
            output.value.to_string().green(),
            output.recipient.to_string().cyan()
        );
    }

    println!("  {}", rule.cyan());
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{cmd_delete, cmd_generate, cmd_init, cmd_rename};
    use crate::keys::load_wallet;
    use tempfile::TempDir;

    #[test]
    fn delete_key() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");

        cmd_init(&wp).unwrap();
        cmd_generate(&wp, "second").unwrap();
        let wallet = load_wallet(&wp).unwrap();
        assert_eq!(wallet.keys.len(), 2);

        let addr = AccountAddress::from_hash_string(&wallet.keys[1].address).unwrap();
        cmd_delete(&wp, &addr).unwrap();

        let wallet = load_wallet(&wp).unwrap();
        assert_eq!(wallet.keys.len(), 1);
        assert_eq!(wallet.keys[0].label, "default");
    }

    #[test]
    fn rename_key() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");

        cmd_init(&wp).unwrap();
        let wallet = load_wallet(&wp).unwrap();
        let addr = AccountAddress::from_hash_string(&wallet.keys[0].address).unwrap();

        cmd_rename(&wp, &addr, "new_name").unwrap();

        let wallet = load_wallet(&wp).unwrap();
        assert_eq!(wallet.keys[0].label, "new_name");
    }

    #[test]
    fn classify_works() {
        assert!(matches!(
            classify_identifier(
                "Bx7e09ff05219c8e14e8ffe148a9b23a824748cfb77bf0d424f4aff4d2b5b30d73"
            ),
            QueryId::BlockHash(_)
        ));

        assert!(matches!(classify_identifier("0"), QueryId::Height(0)));
        assert!(matches!(classify_identifier("42"), QueryId::Height(42)));
        assert!(matches!(
            classify_identifier("999999"),
            QueryId::Height(999999)
        ));

        assert!(matches!(
            classify_identifier(
                "Tx9a3b7c05219c8e14e8ffe148a9b23a824748cfb77bf0d424f4aff4d2b5b30d73"
            ),
            QueryId::TxHash(_)
        ));

        assert!(matches!(classify_identifier("foobar"), QueryId::Unknown(_)));
        assert!(matches!(classify_identifier("hello"), QueryId::Unknown(_)));
    }
}
