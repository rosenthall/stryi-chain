#![allow(incomplete_features)]
#![cfg_attr(test, allow(deprecated))]

#![feature(generic_const_exprs)]

mod api_client;
mod cmd;
mod keys;
mod output;
mod repl;
mod tx_builder;
mod tx_wait;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use stryi_core::address::AccountAddress;

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

    /// Query balances for wallet addresses or explicit addresses from the node.
    Balance {
        /// Zero or more addresses. If omitted, query all addresses in the wallet.
        #[arg(value_name = "ADDRESS")]
        addresses: Vec<String>,
    },

    /// Build, sign, and submit a payment transaction.
    Send {
        #[arg(long)]
        from: String,

        #[arg(long)]
        to: String,

        #[arg(long)]
        amount: u64,

        /// Wait for node confirmation before exiting.
        #[arg(long = "wait-for-confirmation", alias = "wait")]
        wait: bool,
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

    /// Query one or more blocks by height or hash.
    Block {
        /// One or more block heights (u64) or block hashes (Bx...).
        #[arg(value_name = "ID", required = true, num_args = 1..)]
        ids: Vec<String>,
    },

    /// Query one or more transactions currently known by the node.
    Tx {
        /// One or more transaction hashes (Tx...).
        #[arg(value_name = "HASH", required = true, num_args = 1..)]
        ids: Vec<String>,
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
        Command::Balance { addresses } => {
            if addresses.is_empty() {
                cmd::cmd_balance_all(&wallet_path, &cli.node).await?
            } else {
                cmd::cmd_balance_many(&cli.node, &addresses).await?
            }
        }
        Command::Delete { address } => {
            let addr = AccountAddress::from_hash_string(&address).context("invalid address")?;
            cmd::cmd_delete(&wallet_path, &addr)?
        }
        Command::Rename { address, label } => {
            let addr = AccountAddress::from_hash_string(&address).context("invalid address")?;
            cmd::cmd_rename(&wallet_path, &addr, &label)?
        }
        Command::Block { ids } => cmd::cmd_block_many(&cli.node, &ids).await?,
        Command::Tx { ids } => cmd::cmd_tx_many(&cli.node, &ids).await?,
        Command::Send {
            from,
            to,
            amount,
            wait,
        } => {
            let from_addr =
                AccountAddress::from_hash_string(&from).context("invalid 'from' address")?;
            let to_addr = AccountAddress::from_hash_string(&to).context("invalid 'to' address")?;
            cmd::cmd_send(&wallet_path, &cli.node, &from_addr, &to_addr, amount, wait).await?
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
