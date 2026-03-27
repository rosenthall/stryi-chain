//! Interactive REPL for the wallet.

use anyhow::{Context, Result};
use colored::Colorize;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Editor, Helper};
use std::borrow::Cow;
use std::io::{Write, stdin, stdout};
use std::path::Path;

use crate::api_client::NodeClient;
use crate::cmd::{
    cmd_balance_all, cmd_balance_many, cmd_block_many, cmd_delete, cmd_generate, cmd_import,
    cmd_init, cmd_list, cmd_nodestate, cmd_rename, cmd_send, cmd_tx_many,
};
use crate::keys::load_wallet;
use crate::output::shorten_middle;
use stryi_core::address::AccountAddress;
use stryi_core::transactions::FeePolicy;

/// List of all the REPL commands used for fuzzy completion.
const REPL_COMMANDS: &[&str] = &[
    "init",
    "generate",
    "import",
    "list",
    "keys",
    "delete",
    "rename",
    "balance",
    "send",
    "block",
    "tx",
    "nodestate",
    "help",
    "clear",
    "exit",
];

/// Implements rustyline's `Helper` trait to provide hints and tab-completion.
struct ReplHelper;

impl Helper for ReplHelper {}
impl Validator for ReplHelper {}

impl Hinter for ReplHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &rustyline::Context<'_>) -> Option<String> {
        if pos == 0 || line[..pos].contains(' ') {
            return None;
        }
        let prefix = &line[..pos];
        REPL_COMMANDS
            .iter()
            .find(|c| c.starts_with(prefix) && **c != prefix)
            .map(|c| c[pos..].to_string())
    }
}

impl Highlighter for ReplHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[2m{}\x1b[0m", hint))
    }
}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if line[..pos].contains(' ') {
            return Ok((pos, vec![]));
        }
        let prefix = &line[..pos];
        if prefix.is_empty() {
            return Ok((0, vec![]));
        }

        let mut matches: Vec<Pair> = REPL_COMMANDS
            .iter()
            .filter(|c| c.starts_with(prefix))
            .map(|c| Pair {
                display: c.to_string(),
                replacement: c.to_string(),
            })
            .collect();

        if matches.is_empty() {
            matches = REPL_COMMANDS
                .iter()
                .filter(|c| c.contains(prefix))
                .map(|c| Pair {
                    display: c.to_string(),
                    replacement: c.to_string(),
                })
                .collect();
        }

        Ok((0, matches))
    }
}

/// Uses strsim's implementation of Levenshtein distance to suggest similar commands.
fn fuzzy_suggest(input: &str) -> Option<&'static str> {
    let mut best: Option<(&str, usize)> = None;
    for &cmd in REPL_COMMANDS {
        let dist = strsim::levenshtein(input, cmd);
        if dist <= 2 && (best.is_none() || dist < best.unwrap().1) {
            best = Some((cmd, dist));
        }
    }
    best.map(|(cmd, _)| cmd)
}

/// Prompts the user for a string value, returning the default if input is empty.
fn prompt_line(label: &str, default: Option<&str>) -> Result<String> {
    if let Some(def) = default {
        print!(
            "  {} {} {}: ",
            ">".cyan().bold(),
            label.bold(),
            format!("[{}]", def).white()
        );
    } else {
        print!("  {} {}: ", ">".cyan().bold(), label.bold());
    }
    stdout().flush()?;
    let mut buf = String::new();
    stdin().read_line(&mut buf)?;
    let val = buf.trim().to_string();
    if val.is_empty() {
        if let Some(def) = default {
            return Ok(def.to_string());
        }
        anyhow::bail!("{} is required", label);
    }
    Ok(val)
}

/// Prompts with a pretty confirmation message that accepts "y" or "yes".
fn prompt_confirm(message: &str) -> Result<bool> {
    print!(
        "  {} {} {}: ",
        "?".cyan().bold(),
        message.bold(),
        "[y/N]".white()
    );
    stdout().flush()?;
    let mut buf = String::new();
    stdin().read_line(&mut buf)?;
    Ok(matches!(buf.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Shows a numbered address picker from the wallet, then prompts for selection by index or raw address.
fn prompt_select_address(wallet_path: &Path, label: &str) -> Result<String> {
    if let Ok(wallet) = load_wallet(wallet_path)
        && !wallet.keys.is_empty()
    {
        println!();
        for (i, key) in wallet.keys.iter().enumerate() {
            println!(
                "    {}  {} ({})",
                format!("[{}]", i + 1).cyan().bold(),
                key.address.to_string().white(),
                key.label
            );
        }
        println!();
        let input = prompt_line(&format!("{} (# or address)", label), None)?;
        if let Some(addr) = wallet_address_from_selection(wallet_path, &input) {
            println!("    -> {}", addr.cyan());
            return Ok(addr);
        }
        return Ok(input);
    }
    prompt_line(label, None)
}

fn wallet_address_from_selection(wallet_path: &Path, input: &str) -> Option<String> {
    let wallet = load_wallet(wallet_path).ok()?;
    let idx = input.parse::<usize>().ok()?;
    let offset = idx.checked_sub(1)?;

    wallet
        .keys
        .get(offset)
        .map(|entry| entry.address.to_string())
}

/// Prints a bordered summary box before the user confirms a send.
fn print_tx_summary(
    wallet_path: &Path,
    from: &str,
    to: &str,
    amount: u64,
    fee_est: u64,
    wait_for_confirmation: bool,
) {
    let rule = "_".repeat(44);
    let wallet = load_wallet(wallet_path).ok();
    let label_for = |address: &str| {
        wallet
            .as_ref()
            .into_iter()
            .flat_map(|wallet| wallet.keys.iter())
            .find(|entry| entry.address == address)
            .map(|entry| entry.label.as_str())
    };
    let from_label = label_for(from);
    let to_label = label_for(to);

    println!();
    println!("  {}", "Transfer Preview".cyan().bold());
    println!("  {}", rule.cyan());
    println!("  {}", "From".bold());
    if let Some(label) = from_label {
        println!("    {:<8} {}", "Label".dimmed(), label.white().bold());
    }
    println!(
        "    {:<8} {}",
        "Address".dimmed(),
        shorten_middle(from).cyan()
    );
    println!("  {}", "To".bold());
    if let Some(label) = to_label {
        println!("    {:<8} {}", "Label".dimmed(), label.white().bold());
    }
    println!(
        "    {:<8} {}",
        "Address".dimmed(),
        shorten_middle(to).cyan()
    );
    println!(
        "    {:<8} {}",
        "Amount".dimmed(),
        amount.to_string().green().bold()
    );
    println!(
        "    {:<8} {}",
        "Fee".dimmed(),
        format!("~{}", fee_est).yellow()
    );
    println!(
        "    {:<8} {}",
        "Total".dimmed(),
        format!("~{}", amount + fee_est).yellow().bold()
    );
    println!(
        "    {:<8} {}",
        "After".dimmed(),
        if wait_for_confirmation {
            "Wait for confirmation".yellow().bold()
        } else {
            "Return after submission".cyan().bold()
        }
    );
    println!("  {}", rule.cyan());
    println!();
}

struct CommandInput<'a> {
    values: &'a [&'a str],
}

impl<'a> CommandInput<'a> {
    fn new(values: &'a [&'a str]) -> Self {
        Self { values }
    }

    fn value(&self, index: usize, prompt: &str, default: Option<&str>) -> Result<String> {
        match self.values.get(index) {
            Some(value) => Ok((*value).to_string()),
            None => prompt_line(prompt, default),
        }
    }

    fn parse<T>(
        &self,
        index: usize,
        prompt: &str,
        default: Option<&str>,
        invalid_msg: &'static str,
    ) -> Result<T>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        let raw = self.value(index, prompt, default)?;
        raw.parse::<T>()
            .map_err(|e| anyhow::anyhow!("{invalid_msg}: {e}"))
    }

    fn wallet_address(
        &self,
        index: usize,
        wallet_path: &Path,
        prompt: &str,
        invalid_msg: &'static str,
    ) -> Result<(String, AccountAddress)> {
        let raw = match self.values.get(index) {
            Some(value) => wallet_address_from_selection(wallet_path, value)
                .unwrap_or_else(|| (*value).to_string()),
            None => prompt_select_address(wallet_path, prompt)?,
        };
        let address = AccountAddress::from_hash_string(&raw).context(invalid_msg)?;
        Ok((raw, address))
    }
}

/// Spawns the main REPL loop.
pub(crate) async fn run_interactive(wallet_path: &Path, node_url: &str) -> Result<()> {
    let mut rl = Editor::new().context("failed to init readline")?;
    rl.set_helper(Some(ReplHelper));

    let history_path = dirs::home_dir().map(|h| h.join(".stryi").join("wallet_history.txt"));
    if let Some(ref p) = history_path {
        let _ = rl.load_history(p);
    }

    let title = "stryi-wallet interactive mode";
    let wallet_path_label = if let Some(home) = dirs::home_dir() {
        if let Ok(relative) = wallet_path.strip_prefix(&home) {
            format!("~/{}", relative.display())
        } else {
            wallet_path.display().to_string()
        }
    } else {
        wallet_path.display().to_string()
    };
    let wallet_info = if let Ok(w) = load_wallet(wallet_path) {
        format!("{} ({} keys)", wallet_path_label, w.keys.len())
    } else {
        format!("{} (no wallet)", wallet_path_label)
    };

    println!();
    println!("  {}", title.bold());
    println!("  {}", "-".repeat(title.len()).cyan());
    println!("  {:<8} {}", "Wallet".cyan().bold(), wallet_info.white());
    println!("  {:<8} {}", "Node".cyan().bold(), node_url.white());
    println!(
        "\n  Type {} for commands, {} to quit.\n",
        "help".cyan(),
        "exit".cyan()
    );

    loop {
        // build prompt on each new iteration
        let prompt = build_repl_prompt(wallet_path);

        match rl.readline(&prompt) {
            // -- read successfully --
            Ok(line) => {
                let line = line.trim();

                // fast skip if empty command
                if line.is_empty() {
                    continue;
                }

                // add this line to history
                let _ = rl.add_history_entry(line);

                // process clear/cls
                if line == "clear" || line == "cls" {
                    let _ = rl.clear_screen();
                    continue;
                }

                // dispatch real commands
                match dispatch_repl(wallet_path, node_url, line).await {
                    Ok(should_exit) => {
                        if should_exit {
                            break;
                        }
                    }
                    Err(e) => eprintln!("{} {e:#}", "error:".red().bold()),
                }

                println!();
            }

            // -- continue on interruption --
            Err(ReadlineError::Interrupted) => {
                continue;
            }

            // -- break on other error --
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
    }

    if let Some(ref p) = history_path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = rl.save_history(p);
    }

    Ok(())
}

/// build string prompt
fn build_repl_prompt(wallet_path: &Path) -> String {
    let key_count = load_wallet(wallet_path).map(|w| w.keys.len()).unwrap_or(0);

    if key_count == 0 {
        format!("{}{} ", "wallet".cyan(), ">".white())
    } else {
        format!(
            "{}({}){} ",
            "wallet".cyan(),
            key_count.to_string().white(),
            ">".white()
        )
    }
}

/// Returns `Ok(true)` when the REPL should exit.
async fn dispatch_repl(wallet_path: &Path, node_url: &str, line: &str) -> Result<bool> {
    let mut it = line.split_whitespace();
    let Some(cmd) = it.next() else {
        return Ok(false);
    };

    // Collect only args
    let args: Vec<&str> = it.collect();
    let input = CommandInput::new(&args);

    match cmd {
        // -- meta / exit --
        "exit" | "quit" | "q" => return Ok(true),
        "help" => print_repl_help(),

        // -- wallet file --
        "init" => cmd_init(wallet_path)?,

        // -- key management --
        "generate" => handle_generate(wallet_path, &input)?,
        "import" => handle_import(wallet_path, &input)?,
        "list" => cmd_list(wallet_path, false)?,
        "keys" => cmd_list(wallet_path, true)?,
        "rename" => handle_rename(wallet_path, &input)?,
        "delete" => handle_delete(wallet_path, &input)?,

        // -- balances and sending --
        "balance" => handle_balance(wallet_path, node_url, &input).await?,
        "send" => handle_send(wallet_path, node_url, &input).await?,

        // -- node and chain queries --
        "block" => handle_block(node_url, &input).await?,
        "tx" => handle_tx(node_url, &input).await?,
        "nodestate" => cmd_nodestate(node_url).await?,

        // fallback to our suggester
        _ => handle_unknown_command(cmd),
    }

    Ok(false)
}

fn handle_generate(wallet_path: &Path, input: &CommandInput<'_>) -> Result<()> {
    let label = input.value(0, "Label", Some("default"))?;
    cmd_generate(wallet_path, &label)
}

fn handle_import(wallet_path: &Path, input: &CommandInput<'_>) -> Result<()> {
    let key = input.value(0, "Private key (hex)", None)?;
    let label = input.value(1, "Label", Some("imported"))?;
    cmd_import(wallet_path, &key, &label)
}

fn handle_delete(wallet_path: &Path, input: &CommandInput<'_>) -> Result<()> {
    let (address_str, address) =
        input.wallet_address(0, wallet_path, "Address to delete", "invalid address")?;

    if !prompt_confirm(&format!("Delete key {}?", &address_str))? {
        println!("  {}", "Cancelled.".yellow());
        return Ok(());
    }

    cmd_delete(wallet_path, &address)?;
    Ok(())
}

fn handle_rename(wallet_path: &Path, input: &CommandInput<'_>) -> Result<()> {
    let (_, address) =
        input.wallet_address(0, wallet_path, "Address to rename", "invalid address")?;
    let new_label = input.value(1, "New label", None)?;
    cmd_rename(wallet_path, &address, &new_label)
}

async fn handle_balance(
    wallet_path: &Path,
    node_url: &str,
    input: &CommandInput<'_>,
) -> Result<()> {
    if input.values.is_empty() {
        cmd_balance_all(wallet_path, node_url).await
    } else {
        let addresses = input
            .values
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        cmd_balance_many(node_url, &addresses).await
    }
}

fn parse_send_args<'a>(values: &'a [&'a str]) -> Result<(Vec<&'a str>, bool)> {
    let mut positionals = Vec::new();
    let mut wait_override = None;

    for value in values {
        match *value {
            "wait" | "--wait" | "--wait-for-confirmation" => {
                if wait_override == Some(false) {
                    anyhow::bail!(
                        "conflicting confirmation flags. Use only one of `--wait` or `--no-wait`."
                    );
                }
                wait_override = Some(true);
            }
            "no-wait" | "--no-wait" | "--no-wait-for-confirmation" => {
                if wait_override == Some(true) {
                    anyhow::bail!(
                        "conflicting confirmation flags. Use only one of `--wait` or `--no-wait`."
                    );
                }
                wait_override = Some(false);
            }
            other => positionals.push(other),
        }
    }

    if positionals.len() > 3 {
        anyhow::bail!(
            "unexpected extra argument '{}'. Usage: send [from] [to] [amount]",
            positionals[3]
        );
    }

    Ok((positionals, wait_override.unwrap_or(true)))
}

async fn handle_send(wallet_path: &Path, node_url: &str, input: &CommandInput<'_>) -> Result<()> {
    let (positionals, wait_for_confirmation) = parse_send_args(input.values)?;
    let send_input = CommandInput::new(&positionals);

    let (from_str, from) =
        send_input.wallet_address(0, wallet_path, "From", "invalid 'from' address")?;

    let (to_str, to) = send_input.wallet_address(1, wallet_path, "To", "invalid 'to' address")?;

    let amount: u64 = send_input.parse(2, "Amount", None, "invalid amount")?;

    let client = NodeClient::new(node_url);
    let utxo_count = client
        .get_balance(&from_str)
        .await
        .map(|r| r.utxos.len())
        .unwrap_or(1)
        .max(1);

    let fee_est = FeePolicy::default().estimate_fee(utxo_count, 2);
    print_tx_summary(
        wallet_path,
        &from_str,
        &to_str,
        amount,
        fee_est,
        wait_for_confirmation,
    );

    if !prompt_confirm("Confirm send?")? {
        println!("  {}", "Cancelled.".yellow());
        return Ok(());
    }

    cmd_send(
        wallet_path,
        node_url,
        &from,
        &to,
        amount,
        wait_for_confirmation,
    )
    .await?;
    Ok(())
}

async fn handle_block(node_url: &str, input: &CommandInput<'_>) -> Result<()> {
    let ids = if input.values.is_empty() {
        vec![input.value(0, "Block height or hash", None)?]
    } else {
        input
            .values
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    };

    cmd_block_many(node_url, &ids).await
}

async fn handle_tx(node_url: &str, input: &CommandInput<'_>) -> Result<()> {
    let ids = if input.values.is_empty() {
        vec![input.value(0, "Transaction hash", None)?]
    } else {
        input
            .values
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    };

    cmd_tx_many(node_url, &ids).await
}

fn handle_unknown_command(cmd: &str) {
    if let Some(suggestion) = fuzzy_suggest(cmd) {
        eprintln!(
            "unknown command '{}'. Did you mean {}?",
            cmd,
            suggestion.cyan()
        );
    } else {
        eprintln!(
            "unknown command '{}'. Type {} for available commands.",
            cmd,
            "help".cyan()
        );
    }
}

/// Prints the REPL help table listing every available command.
fn print_repl_help() {
    println!("\n  {}", "Commands:".bold());
    println!("    {:<12} Create a new wallet file", "init".cyan());
    println!("    {:<12} Generate a new key pair", "generate".cyan());
    println!("    {:<12} Import a private key", "import".cyan());
    println!("    {:<12} List wallet addresses", "list".cyan());
    println!("    {:<12} List addresses with private keys", "keys".cyan());
    println!("    {:<12} Delete a key from the wallet", "delete".cyan());
    println!("    {:<12} Rename a key in the wallet", "rename".cyan());
    println!(
        "    {:<12} Query balances reported by the node",
        "balance".cyan()
    );
    println!("    {:<12} Send a payment", "send".cyan());
    println!("    {:<12} Query a block by height or hash", "block".cyan());
    println!(
        "    {:<12} Query a transaction currently known by the node",
        "tx".cyan()
    );
    println!("    {:<12} Show node state", "nodestate".cyan());
    println!("    {:<12} Show this help", "help".cyan());
    println!("    {:<12} Clear screen", "clear".cyan());
    println!("    {:<12} Quit", "exit".cyan());
    println!("\n  {}", "Examples:".bold());
    println!("    {}", "balance".white());
    println!("    {}", "balance @addr1 @addr2".white());
    println!("    {}", "send @from @to 1000".white());
    println!("    {}", "send @from @to 1000 --no-wait".white());
    println!("    {}", "send".white());
    println!("    {}", "block 42 Bx...".white());
    println!("    {}", "tx Tx...".white());
    println!(
        "\n  {}",
        "  `send` waits for confirmation by default. Add `--no-wait` to return after submission."
            .white()
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::load_wallet;
    use tempfile::TempDir;

    #[tokio::test]
    async fn init_and_list() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");

        let exit = dispatch_repl(&wp, "http://localhost:0", "init")
            .await
            .unwrap();
        assert!(!exit);
        assert!(wp.exists());

        let exit = dispatch_repl(&wp, "http://localhost:0", "list")
            .await
            .unwrap();
        assert!(!exit);
    }

    #[tokio::test]
    async fn generate_with_label() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");

        dispatch_repl(&wp, "http://localhost:0", "init")
            .await
            .unwrap();
        dispatch_repl(&wp, "http://localhost:0", "generate mylabel")
            .await
            .unwrap();

        let wallet = load_wallet(&wp).unwrap();
        assert_eq!(wallet.keys.len(), 2);
        assert_eq!(wallet.keys[1].label, "mylabel");
    }

    #[test]
    fn fuzzy_suggest_close_match() {
        assert_eq!(fuzzy_suggest("balence"), Some("balance"));
        assert_eq!(fuzzy_suggest("sendd"), Some("send"));
        assert_eq!(fuzzy_suggest("lst"), Some("list"));
    }

    #[test]
    fn fuzzy_suggest_no_match() {
        assert_eq!(fuzzy_suggest("xyzxyzxyz"), None);
    }

    #[test]
    fn wallet_address_from_selection_maps_indexes() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");
        let wallet = crate::keys::WalletFile {
            keys: vec![
                crate::keys::generate_keypair("first"),
                crate::keys::generate_keypair("second"),
            ],
        };
        crate::keys::save_wallet(&wp, &wallet).unwrap();

        assert_eq!(
            wallet_address_from_selection(&wp, "1"),
            Some(wallet.keys[0].address.clone())
        );
        assert_eq!(
            wallet_address_from_selection(&wp, "2"),
            Some(wallet.keys[1].address.clone())
        );
    }

    #[test]
    fn wallet_address_from_selection_ignores_non_indexes() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");
        let wallet = crate::keys::WalletFile {
            keys: vec![crate::keys::generate_keypair("first")],
        };
        crate::keys::save_wallet(&wp, &wallet).unwrap();

        let raw = wallet.keys[0].address.clone();
        assert_eq!(wallet_address_from_selection(&wp, &raw), None);
        assert_eq!(wallet_address_from_selection(&wp, "@deadbeef"), None);
        assert_eq!(wallet_address_from_selection(&wp, "0"), None);
    }

    #[test]
    fn parse_send_args_cases() {
        for (args, expected_positionals, expected_wait) in [
            (&["1"][..], vec!["1"], true),
            (&["wait", "1"][..], vec!["1"], true),
            (&["--wait-for-confirmation", "1"][..], vec!["1"], true),
            (&["--no-wait", "1"][..], vec!["1"], false),
        ] {
            let (positionals, wait_for_confirmation) = parse_send_args(args).unwrap();
            assert_eq!(positionals, expected_positionals);
            assert_eq!(wait_for_confirmation, expected_wait);
        }

        for args in [&["--wait", "--no-wait", "1"][..], &["1", "2", "3", "4"][..]] {
            assert!(parse_send_args(args).is_err());
        }
    }

    #[tokio::test]
    async fn block_with_tx_hash_returns_redirect_error() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");
        let result = dispatch_repl(
            &wp,
            "http://localhost:0",
            "block Tx9a3b7c05219c8e14e8ffe148a9b23a824748cfb77bf0d424f4aff4d2b5b30d73",
        )
        .await;
        assert!(result.is_err());
        let message = result.err().unwrap().to_string();
        assert!(message.contains("Use `tx "));
    }

    #[tokio::test]
    async fn send_rejects_invalid_from() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");
        let result = dispatch_repl(
            &wp,
            "http://localhost:0",
            "send notanaddr @0000000000000000000000000000000000000000 100",
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn block_rejects_invalid_block_hash() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");
        let result = dispatch_repl(&wp, "http://localhost:0", "block Bxzzzz").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn tx_rejects_invalid_hash() {
        let dir = TempDir::new().unwrap();
        let wp = dir.path().join("wallet.json");
        let result = dispatch_repl(&wp, "http://localhost:0", "tx nope").await;
        assert!(result.is_err());
    }
}
