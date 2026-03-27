use crate::QueryId;
use crate::api_client::{AddressBalanceResponse, NodeClient, TransactionQueryStatus};
use crate::keys::{
    WalletFile, find_key_by_address, generate_keypair, import_key, load_wallet, require_wallet,
    save_wallet,
};
use crate::output::{print_block_detail, print_transaction_response_detail, shorten_middle};
use crate::tx_builder::{SpendableUtxo, build_payment, serialize_for_submission};
use crate::tx_wait::wait_for_tx_confirmation;
use anyhow::Context;
use colored::Colorize;
use std::path::Path;
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash};
use stryi_core::transactions::{FeePolicy, OutPoint, TransactionHash};
use tokio::time::{Duration, sleep};

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

pub(crate) async fn cmd_balance_many(node_url: &str, addresses: &[String]) -> anyhow::Result<()> {
    let mut client = None;
    let mut errors = Vec::new();

    for (i, raw) in addresses.iter().enumerate() {
        print_batch_separator(i);

        let address = match AccountAddress::from_hash_string(raw).context("invalid address") {
            Ok(address) => address,
            Err(e) => {
                eprintln!("balance {}: {e}", raw.cyan());
                errors.push(format!("{raw}: {e}"));
                continue;
            }
        };

        let client = client.get_or_insert_with(|| NodeClient::new(node_url));
        if let Err(e) = cmd_balance_with_client(client, &address).await {
            eprintln!("balance {}: {e}", raw.cyan());
            errors.push(format!("{raw}: {e}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("one or more balance queries failed: {}", errors.join(" | "));
    }
}

pub(crate) async fn cmd_balance_all(wallet_path: &Path, node_url: &str) -> anyhow::Result<()> {
    let wallet = require_wallet(wallet_path)?;

    if wallet.keys.is_empty() {
        println!("Wallet is empty.");
        return Ok(());
    }

    let client = NodeClient::new(node_url);

    println!();
    println!(
        "  {:<5} {:<10} {:<44} {}",
        "#".bold(),
        "Label".bold(),
        "Address".bold(),
        "Balance".bold(),
    );
    println!("  {}", "-".repeat(76));
    for (i, key) in wallet.keys.iter().enumerate() {
        let addr = key.address.to_string();
        match client.get_balance(&addr).await {
            Ok(resp) => {
                let balance = format!("{:>12}", resp.balance);
                println!(
                    "  {:<5} {:<10} {:<44} {}",
                    format!("[{}]", i + 1).cyan().bold(),
                    key.label,
                    addr.cyan(),
                    balance.green().bold()
                );
            }
            Err(e) => {
                let error = format!("error: {e}");
                println!(
                    "  {:<5} {:<10} {:<44} {}",
                    format!("[{}]", i + 1).cyan().bold(),
                    key.label,
                    addr.cyan(),
                    error.red()
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
    wait: bool,
) -> anyhow::Result<()> {
    let from_str = from.to_string();

    let wallet = require_wallet(wallet_path)?;
    let key_entry = find_key_by_address(&wallet, &from_str)?;

    let client = NodeClient::new(node_url);

    let balance_resp = client.get_balance(&from_str).await?;

    if balance_resp.utxos.is_empty() {
        anyhow::bail!("no UTXOs available for address {}", from_str);
    }

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
    let tx = build_payment(&key_entry.private_key, &spendable, to, amount, &fee_policy)?;

    let num_inputs = tx.data.inputs.len();
    let num_outputs = tx.data.outputs.len();
    let tx_hash = tx.data.hash();
    let tx_hash_str = tx_hash.to_string();

    let encoded = serialize_for_submission(&tx)?;
    println!();
    println!("  {}", "Transaction".green().bold());
    println!("  {}", "_".repeat(44).cyan());
    println!(
        "  {:<12} {}",
        "Hash".bold(),
        shorten_middle(&tx_hash_str).cyan()
    );
    println!("  {:<12} {}", "Full hash".bold(), tx_hash_str.cyan());
    println!(
        "  {:<12} {}",
        "Inputs".bold(),
        num_inputs.to_string().cyan()
    );
    println!(
        "  {:<12} {}",
        "Outputs".bold(),
        num_outputs.to_string().cyan()
    );
    println!(
        "  {:<12} {}",
        "Balance".bold(),
        balance_resp.balance.to_string().green().bold()
    );
    println!(
        "  {:<12} {}",
        "UTXOs".bold(),
        balance_resp.utxos.len().to_string().cyan()
    );
    println!("  {}", "_".repeat(44).cyan());

    let resp = client.send_transaction(&encoded).await?;
    println!(
        "  {:<12} {}",
        "Status".bold(),
        "Accepted by node".green().bold()
    );
    println!("  {:<12} {}", "Node".bold(), resp.to_string().green());

    if wait {
        confirm_transaction_and_print_balances(&client, &tx_hash, from, to).await?;
    }

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

pub(crate) async fn cmd_block_many(node_url: &str, identifiers: &[String]) -> anyhow::Result<()> {
    let mut client = None;
    let mut errors = Vec::new();

    for (i, identifier) in identifiers.iter().enumerate() {
        print_batch_separator(i);

        let query = match block_query_from_identifier(identifier) {
            Ok(query) => query,
            Err(e) => {
                eprintln!("block {}: {e}", identifier.cyan());
                errors.push(format!("{identifier}: {e}"));
                continue;
            }
        };

        let client = client.get_or_insert_with(|| NodeClient::new(node_url));
        if let Err(e) = cmd_block_with_client(client, &query).await {
            eprintln!("block {}: {e}", identifier.cyan());
            errors.push(format!("{identifier}: {e}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("one or more block queries failed: {}", errors.join(" | "));
    }
}

pub(crate) async fn cmd_tx_many(node_url: &str, identifiers: &[String]) -> anyhow::Result<()> {
    let mut client = None;
    let mut errors = Vec::new();

    for (i, identifier) in identifiers.iter().enumerate() {
        print_batch_separator(i);

        let tx_hash = match TransactionHash::from_hash_string(identifier)
            .context("invalid transaction hash")
        {
            Ok(tx_hash) => tx_hash,
            Err(e) => {
                eprintln!("tx {}: {e}", identifier.cyan());
                errors.push(format!("{identifier}: {e}"));
                continue;
            }
        };

        let client = client.get_or_insert_with(|| NodeClient::new(node_url));
        if let Err(e) = cmd_tx_with_client(client, &tx_hash).await {
            eprintln!("tx {}: {e}", identifier.cyan());
            errors.push(format!("{identifier}: {e}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("one or more tx queries failed: {}", errors.join(" | "));
    }
}

async fn cmd_balance_with_client(
    client: &NodeClient,
    address: &AccountAddress,
) -> anyhow::Result<()> {
    let addr_str = address.to_string();
    let resp = client.get_balance(&addr_str).await?;
    print_balance_detail(&resp);
    Ok(())
}

fn block_query_from_identifier(identifier: &str) -> anyhow::Result<String> {
    let query = match crate::classify_identifier(identifier) {
        QueryId::TxHash(h) => {
            anyhow::bail!("'{}' is a transaction hash. Use `tx {}` instead.", h, h);
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

    Ok(query)
}

async fn cmd_block_with_client(client: &NodeClient, query: &str) -> anyhow::Result<()> {
    let resp = client.get_block(query).await?;

    let block: Block =
        serde_json::from_value(resp.block).context("failed to deserialize block data")?;

    print_block_detail(&resp.hash, &block);
    Ok(())
}

async fn cmd_tx_with_client(client: &NodeClient, tx_hash: &TransactionHash) -> anyhow::Result<()> {
    let resp = client.get_transaction(&tx_hash.to_string()).await?;

    let rule = "-".repeat(40);
    println!();
    println!("  {}", "Transaction Known By Node".bold());
    println!("  {}", rule.cyan());
    println!(
        "  {:<20} {}",
        "Known as:".bold(),
        format_tx_status(&resp.status)
    );
    println!(
        "  {:<20} {}",
        "Hash:".bold(),
        resp.tx_hash.to_string().cyan()
    );

    match resp.status {
        TransactionQueryStatus::Pending => {
            println!("  {:<20} {}", "Location:".bold(), "mempool".yellow());
        }
        TransactionQueryStatus::Confirmed => {
            println!("  {:<20} {}", "Location:".bold(), "chain".green());

            if let Some(block_hash) = &resp.block_hash {
                println!("  {:<20} {}", "Block:".bold(), block_hash);
            }
            if let Some(block_height) = resp.block_height {
                println!(
                    "  {:<20} {}",
                    "Block height:".bold(),
                    block_height.to_string().cyan()
                );
            }
            if let Some(tx_index) = resp.tx_index {
                println!("  {:<20} {}", "Tx index:".bold(), tx_index);
            }
        }
    }

    println!("  {}", rule.cyan());
    print_transaction_response_detail(&resp.tx_hash.to_string(), &resp.transaction);
    Ok(())
}

fn print_balance_detail(resp: &AddressBalanceResponse) {
    let rule = "-".repeat(40);

    println!();
    println!("  {}", "Balance Reported By Node".bold());
    println!("  {}", rule.cyan());
    println!("  {:<20} {}", "Address:".bold(), resp.address.cyan());
    println!(
        "  {:<20} {}",
        "Balance:".bold(),
        resp.balance.to_string().green().bold()
    );
    println!(
        "  {:<20} {}",
        "UTXOs:".bold(),
        resp.utxos.len().to_string().cyan()
    );

    if !resp.utxos.is_empty() {
        println!("  {}", rule.cyan());
        for utxo in &resp.utxos {
            println!(
                "  {} {}:{} - {}",
                "utxo:".dimmed(),
                utxo.txid,
                utxo.vout,
                utxo.value.to_string().green()
            );
        }
    }

    println!("  {}", rule.cyan());
}

fn print_balance_summary(resp: &AddressBalanceResponse) {
    println!(
        "  {} {}",
        resp.address.cyan(),
        resp.balance.to_string().green().bold()
    );
}

fn print_batch_separator(index: usize) {
    if index > 0 {
        println!();
        println!("  {}", "=".repeat(68).dimmed());
        println!();
    }
}

async fn confirm_transaction_and_print_balances(
    client: &NodeClient,
    tx_hash: &TransactionHash,
    from: &AccountAddress,
    to: &AccountAddress,
) -> anyhow::Result<()> {
    println!();
    println!("  {}", "Confirmation".yellow().bold());
    println!("  {}", "_".repeat(44).cyan());
    let confirmed = wait_for_tx_confirmation(client, tx_hash).await?;
    println!("  {}", "Full confirmation info from the node:".dimmed());
    println!(
        "  {:<12} {}",
        "Tx hash".bold(),
        confirmed.tx_hash.to_string().bright_cyan().bold()
    );
    if let Some(block_hash) = &confirmed.block_hash {
        println!(
            "  {:<12} {}",
            "Block hash".bold(),
            block_hash.bright_yellow().bold()
        );
    }
    if let Some(block_height) = confirmed.block_height {
        println!(
            "  {:<12} {}",
            "Height".bold(),
            block_height.to_string().cyan().bold()
        );
    }
    sleep(Duration::from_millis(500)).await;
    println!("  {}", "Getting updated balances...".cyan().bold());
    sleep(Duration::from_secs(1)).await;

    println!();
    println!("{}", "Balances currently reported by node:".bold());

    let mut balance_failed = false;
    for address in [from, to] {
        let addr_str = address.to_string();
        match client.get_balance(&addr_str).await {
            Ok(resp) => print_balance_summary(&resp),
            Err(e) => {
                eprintln!("balance {}: {e}", addr_str.cyan());
                balance_failed = true;
            }
        }
    }

    if balance_failed {
        anyhow::bail!(
            "transaction {} was confirmed, but one or more balance queries failed",
            tx_hash
        );
    }

    Ok(())
}

fn format_tx_status(status: &TransactionQueryStatus) -> colored::ColoredString {
    match status {
        TransactionQueryStatus::Pending => "pending".yellow(),
        TransactionQueryStatus::Confirmed => "confirmed".green(),
    }
}
