use colored::Colorize;

use crate::api_client::{TransactionKindResponse, TransactionResponse};
use stryi_core::block::Block;
use stryi_core::transactions::TransactionKind;

// Keeps long hashes and addresses readable in compact CLI output.
pub(crate) fn shorten_middle(value: &str) -> String {
    const HEAD: usize = 12;
    const TAIL: usize = 8;
    let chars: Vec<_> = value.chars().collect();

    if chars.len() <= HEAD + TAIL + 1 {
        value.to_string()
    } else {
        let head: String = chars[..HEAD].iter().collect();
        let tail: String = chars[chars.len() - TAIL..].iter().collect();
        format!("{head}...{tail}")
    }
}

pub(crate) fn print_block_detail(hash: &str, block: &Block) {
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
