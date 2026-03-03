use crate::QueryId;
use crate::api_client::NodeClient;
use crate::keys::{
    WalletFile, find_key_by_address, generate_keypair, import_key, load_wallet, require_wallet,
    save_wallet,
};
use crate::tx_builder::{SpendableUtxo, build_payment, serialize_for_submission};
use anyhow::Context;
use colored::Colorize;
use std::path::Path;
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash};
use stryi_core::transactions::{FeePolicy, OutPoint};

pub(crate) fn cmd_init(wallet_path: &Path) -> anyhow::Result<()> {
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

    println!("{} {}", "Wallet created at".green(), wallet_path.display());
    println!("  Address: {}", entry.address.to_string().cyan());
    println!("  Label:   {}", entry.label);
    Ok(())
}

pub(crate) fn cmd_generate(wallet_path: &Path, label: &str) -> anyhow::Result<()> {
    let mut wallet = require_wallet(wallet_path)?;

    let entry = generate_keypair(label);
    println!("{}", "Generated new key:".green());
    println!("  Address: {}", entry.address.to_string().cyan());
    println!("  Label:   {}", entry.label);

    wallet.keys.push(entry);
    save_wallet(wallet_path, &wallet)?;
    Ok(())
}

pub(crate) fn cmd_import(wallet_path: &Path, hex_key: &str, label: &str) -> anyhow::Result<()> {
    let mut wallet = if wallet_path.exists() {
        load_wallet(wallet_path)?
    } else {
        WalletFile { keys: vec![] }
    };

    let entry = import_key(hex_key, label)?;
    println!("{}", "Imported key:".green());
    println!("  Address: {}", entry.address.to_string().cyan());
    println!("  Label:   {}", entry.label);

    wallet.keys.push(entry);
    save_wallet(wallet_path, &wallet)?;
    Ok(())
}

pub(crate) fn cmd_list(wallet_path: &Path, show_keys: bool) -> anyhow::Result<()> {
    let wallet = require_wallet(wallet_path)?;

    if wallet.keys.is_empty() {
        println!("Wallet is empty.");
        return Ok(());
    }

    if show_keys {
        println!(
            "  {:<5} {:<10} {:<44} {}",
            "#".bold(),
            "Label".bold(),
            "Address".bold(),
            "Private Key".bold()
        );
        println!("  {}", "-".repeat(116));
        for (i, entry) in wallet.keys.iter().enumerate() {
            println!(
                "  {:<5} {:<10} {:<44} {}",
                format!("[{}]", i + 1).cyan().bold(),
                entry.label,
                entry.address.to_string().cyan(),
                entry.private_key
            );
        }
    } else {
        println!(
            "  {:<5} {:<10} {}",
            "#".bold(),
            "Label".bold(),
            "Address".bold(),
        );
        println!("  {}", "-".repeat(60));
        for (i, entry) in wallet.keys.iter().enumerate() {
            println!(
                "  {:<5} {:<10} {}",
                format!("[{}]", i + 1).cyan().bold(),
                entry.label,
                entry.address.to_string().cyan(),
            );
        }
    }
    Ok(())
}

pub(crate) fn cmd_delete(wallet_path: &Path, address: &AccountAddress) -> anyhow::Result<()> {
    let addr_str = address.to_string();
    let mut wallet = require_wallet(wallet_path)?;

    let idx = wallet
        .keys
        .iter()
        .position(|k| k.address == addr_str)
        .ok_or_else(|| anyhow::anyhow!("address {} not found in wallet", addr_str))?;

    let removed = wallet.keys.remove(idx);
    save_wallet(wallet_path, &wallet)?;

    println!(
        "{} removed {} ({})",
        "Deleted:".green(),
        removed.address.cyan(),
        removed.label
    );
    Ok(())
}

pub(crate) fn cmd_rename(
    wallet_path: &Path,
    address: &AccountAddress,
    new_label: &str,
) -> anyhow::Result<()> {
    let addr_str = address.to_string();
    let mut wallet = require_wallet(wallet_path)?;

    let entry = wallet
        .keys
        .iter_mut()
        .find(|k| k.address == addr_str)
        .ok_or_else(|| anyhow::anyhow!("address {} not found in wallet", addr_str))?;

    let old_label = entry.label.clone();
    entry.label = new_label.to_string();
    save_wallet(wallet_path, &wallet)?;

    println!(
        "{} {} renamed from '{}' to '{}'",
        "Renamed:".green(),
        addr_str.cyan(),
        old_label,
        new_label
    );
    Ok(())
}

pub(crate) async fn cmd_balance(node_url: &str, address: &AccountAddress) -> anyhow::Result<()> {
    let client = NodeClient::new(node_url);
    let addr_str = address.to_string();
    let resp = client.get_balance(&addr_str).await?;

    println!("Address: {}", resp.address.cyan());
    println!("Balance: {}", resp.balance.to_string().green().bold());

    if !resp.utxos.is_empty() {
        println!("\n{}:", format!("UTXOs ({})", resp.utxos.len()).bold());
        for utxo in &resp.utxos {
            println!("  {}:{} - value {}", utxo.txid, utxo.vout, utxo.value);
        }
    }
    Ok(())
}

pub(crate) async fn cmd_balance_all(wallet_path: &Path, node_url: &str) -> anyhow::Result<()> {
    let wallet = require_wallet(wallet_path)?;

    if wallet.keys.is_empty() {
        println!("Wallet is empty.");
        return Ok(());
    }

    let client = NodeClient::new(node_url);

    println!();
    for (i, key) in wallet.keys.iter().enumerate() {
        let addr = key.address.to_string();
        match client.get_balance(&addr).await {
            Ok(resp) => {
                println!(
                    "  {} {} ({}) -- {}",
                    format!("[{}]", i + 1).cyan().bold(),
                    addr.cyan(),
                    key.label,
                    resp.balance.to_string().green().bold()
                );
            }
            Err(e) => {
                println!(
                    "  {} {} ({}) -- {}",
                    format!("[{}]", i + 1).cyan().bold(),
                    addr.cyan(),
                    key.label,
                    format!("error: {e}").red()
                );
            }
        }
    }
    println!();
    Ok(())
}

pub(crate) async fn cmd_send(
    wallet_path: &Path,
    node_url: &str,
    from: &AccountAddress,
    to: &AccountAddress,
    amount: u64,
) -> anyhow::Result<()> {
    let from_str = from.to_string();
    let to_str = to.to_string();

    let wallet = require_wallet(wallet_path)?;
    let key_entry = find_key_by_address(&wallet, &from_str)?;

    let client = NodeClient::new(node_url);

    println!("Fetching UTXOs for {}...", from_str);
    let balance_resp = client.get_balance(&from_str).await?;

    if balance_resp.utxos.is_empty() {
        anyhow::bail!("no UTXOs available for address {}", from_str);
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
        from_str, to_str, amount
    );
    let tx = build_payment(&key_entry.private_key, &spendable, to, amount, &fee_policy)?;

    let num_inputs = tx.data.inputs.len();
    let num_outputs = tx.data.outputs.len();
    let tx_hash = tx.data.hash();

    let encoded = serialize_for_submission(&tx)?;
    println!(
        "{} {} ({} inputs, {} outputs)",
        "Transaction built:".green(),
        tx_hash,
        num_inputs,
        num_outputs
    );

    println!("Submitting to node...");
    let resp = client.send_transaction(&encoded).await?;
    println!(
        "  {} {}",
        "OK".green().bold(),
        format!("Transaction submitted (node: {})", resp).green()
    );
    Ok(())
}

pub(crate) async fn cmd_nodestate(node_url: &str) -> anyhow::Result<()> {
    let client = NodeClient::new(node_url);
    let state = client.get_nodestate().await?;

    let rule = "-".repeat(40);
    println!();
    println!("  {}", "Node State".bold());
    println!("  {}", rule.cyan());
    println!("  {:<20} {}", "Chain:".bold(), state.chain_name);
    println!("  {:<20} {}", "API version:".bold(), state.api_version);
    println!(
        "  {:<20} {}",
        "Height:".bold(),
        state.height.to_string().cyan()
    );
    println!(
        "  {:<20} {}",
        "Latest block:".bold(),
        state.latest_block_hash
    );
    println!(
        "  {:<20} {}",
        "Total difficulty:".bold(),
        state.total_difficulty
    );
    println!(
        "  {:<20} {}",
        "Last updated:".bold(),
        state.last_update_time
    );
    println!("  {}", rule.cyan());
    Ok(())
}

pub(crate) async fn cmd_block(node_url: &str, identifier: &str) -> anyhow::Result<()> {
    let query = match crate::classify_identifier(identifier) {
        QueryId::TxHash(h) => {
            println!(
                "  {} '{}' looks like a transaction hash.",
                "Note:".yellow().bold(),
                h
            );
            println!(
                "  Transaction query is not yet implemented. Use a block height or hash (Bx...)."
            );
            return Ok(());
        }
        QueryId::Unknown(s) => {
            anyhow::bail!(
                "unrecognized identifier '{}'. Expected a block height (e.g. 42) or block hash (Bx...)",
                s
            );
        }
        QueryId::Height(h) => h.to_string(),
        QueryId::BlockHash(h) => {
            BlockHash::from_hash_string(h).context("invalid block hash")?;
            h.to_string()
        }
    };

    let client = NodeClient::new(node_url);
    let resp = client.get_block(&query).await?;

    let block: Block =
        serde_json::from_value(resp.block).context("failed to deserialize block data")?;

    crate::print_block_detail(&resp.hash, &block);
    Ok(())
}
