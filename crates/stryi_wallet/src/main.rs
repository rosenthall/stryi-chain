#![allow(incomplete_features)]
#![feature(generic_const_exprs)]

/// Node HTTP API client.
mod api_client;

/// Wallet key management.
mod keys;

/// Transaction building and serialization.
mod tx_builder;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use crate::api_client::NodeClient;
use crate::keys::{
    WalletFile, find_key_by_address, generate_keypair, import_key, load_wallet, save_wallet,
};
use crate::tx_builder::{SpendableUtxo, build_payment, serialize_for_submission};
use stryi_core::transactions::{FeePolicy, OutPoint};

#[derive(Parser)]
#[command(name = "stryi-wallet", version, about = "StryiChain CLI Wallet")]
struct Cli {
    #[arg(long, global = true, default_value_t = default_wallet_path())]
    wallet_path: String,

    #[arg(long, global = true, default_value = "http://localhost:7001")]
    node: String,

    #[command(subcommand)]
    cmd: Command,
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
    List,

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

    match cli.cmd {
        Command::Init => cmd_init(&wallet_path)?,
        Command::Generate { label } => cmd_generate(&wallet_path, &label)?,
        Command::Import { key, label } => cmd_import(&wallet_path, &key, &label)?,
        Command::List => cmd_list(&wallet_path)?,
        Command::Balance { address } => cmd_balance(&cli.node, &address).await?,
        Command::Send { from, to, amount } => {
            cmd_send(&wallet_path, &cli.node, &from, &to, amount).await?
        }
        Command::NodeState => cmd_nodestate(&cli.node).await?,
    }

    Ok(())
}

fn cmd_init(wallet_path: &Path) -> Result<()> {
    if wallet_path.exists() {
        anyhow::bail!(
            "wallet file already exists at {}. Use `generate` to add more keys.",
            wallet_path.display()
        );
    }

    let entry = generate_keypair("default");
    let wallet = WalletFile {
        keys: vec![entry.clone()],
    };
    save_wallet(wallet_path, &wallet)?;

    println!("Wallet created at {}", wallet_path.display());
    println!("  Address: {}", entry.address);
    println!("  Label:   {}", entry.label);
    Ok(())
}

fn cmd_generate(wallet_path: &Path, label: &str) -> Result<()> {
    let mut wallet = load_wallet(wallet_path)
        .with_context(|| "no wallet found - run `stryi-wallet init` first")?;

    let entry = generate_keypair(label);
    println!("Generated new key:");
    println!("  Address: {}", entry.address);
    println!("  Label:   {}", entry.label);

    wallet.keys.push(entry);
    save_wallet(wallet_path, &wallet)?;
    Ok(())
}

fn cmd_import(wallet_path: &Path, hex_key: &str, label: &str) -> Result<()> {
    let mut wallet = if wallet_path.exists() {
        load_wallet(wallet_path)?
    } else {
        WalletFile { keys: vec![] }
    };

    let entry = import_key(hex_key, label)?;
    println!("Imported key:");
    println!("  Address: {}", entry.address);
    println!("  Label:   {}", entry.label);

    wallet.keys.push(entry);
    save_wallet(wallet_path, &wallet)?;
    Ok(())
}

fn cmd_list(wallet_path: &Path) -> Result<()> {
    let wallet = load_wallet(wallet_path)
        .with_context(|| "no wallet found - run `stryi-wallet init` first")?;

    if wallet.keys.is_empty() {
        println!("Wallet is empty.");
        return Ok(());
    }

    println!("{:<8} {:<44} Private Key", "Label", "Address");
    println!("{}", "-".repeat(120));
    for entry in &wallet.keys {
        println!(
            "{:<8} {:<44} {}",
            entry.label, entry.address, entry.private_key
        );
    }
    Ok(())
}

async fn cmd_balance(node_url: &str, address: &str) -> Result<()> {
    let client = NodeClient::new(node_url);
    let resp = client.get_balance(address).await?;

    println!("Address: {}", resp.address);
    println!("Balance: {}", resp.balance);

    if !resp.utxos.is_empty() {
        println!("\nUTXOs ({}):", resp.utxos.len());
        for utxo in &resp.utxos {
            println!("  {}:{} - value {}", utxo.txid, utxo.vout, utxo.value);
        }
    }
    Ok(())
}

async fn cmd_send(
    wallet_path: &Path,
    node_url: &str,
    from: &str,
    to: &str,
    amount: u64,
) -> Result<()> {
    let wallet = load_wallet(wallet_path).with_context(
        || "no wallet found, run `stryi-wallet init` or `stryi-wallet import` first",
    )?;
    let key_entry = find_key_by_address(&wallet, from)?;

    let client = NodeClient::new(node_url);

    println!("Fetching UTXOs for {}...", from);
    let balance_resp = client.get_balance(from).await?;

    if balance_resp.utxos.is_empty() {
        anyhow::bail!("no UTXOs available for address {}", from);
    }

    println!(
        "Available balance: {} ({} UTXOs)",
        balance_resp.balance,
        balance_resp.utxos.len()
    );

    let spendable: Vec<SpendableUtxo> = balance_resp
        .utxos
        .iter()
        .map(|u| SpendableUtxo {
            outpoint: OutPoint {
                txid: u.txid,
                vout: u.vout,
            },
            value: u.value,
        })
        .collect();

    let fee_policy = FeePolicy::default();

    println!(
        "Building transaction: {} -> {} (amount: {})...",
        from, to, amount
    );
    let tx = build_payment(&key_entry.private_key, &spendable, to, amount, &fee_policy)?;

    let num_inputs = tx.data.inputs.len();
    let num_outputs = tx.data.outputs.len();
    let tx_hash = tx.data.hash();

    let encoded = serialize_for_submission(&tx)?;
    println!(
        "Transaction built: {} ({} inputs, {} outputs)",
        tx_hash, num_inputs, num_outputs
    );

    println!("Submitting to node...");
    let resp = client.send_transaction(&encoded).await?;
    println!("Node response: {}", resp);
    Ok(())
}

async fn cmd_nodestate(node_url: &str) -> Result<()> {
    let client = NodeClient::new(node_url);
    let state = client.get_nodestate().await?;

    println!("Chain:            {}", state.chain_name);
    println!("API version:      {}", state.api_version);
    println!("Height:           {}", state.height);
    println!("Latest block:     {}", state.latest_block_hash);
    println!("Total difficulty: {}", state.total_difficulty);
    println!("Last updated:     {}", state.last_update_time);
    Ok(())
}
