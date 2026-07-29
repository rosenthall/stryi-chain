use crate::chaingen::funding_account::FundAccount;
use crate::chaingen::utxo::{UtxoInfo, UtxoSelectionCriteria};
use indexmap::{IndexMap, IndexSet};
use k256::ecdsa::SigningKey;
use rand::SeedableRng;
use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use stryi_core::PrivateKey;
use stryi_core::address::AccountAddress;
use stryi_core::block::Block;
use stryi_core::transactions::{OutPoint, TransactionKind};
use stryi_storage::chaingen::StryiStorageChaingenExt;
use stryi_storage::{StryiStorage, StryiStorageError};
use tracing::{debug, info};

#[derive(Clone)]
pub struct GenerationState {
    /// Collection of all the accounts that are used during a generation process.
    /// @addr mapped by SigningKey of a corresponding account.
    pub(crate) accounts: IndexMap<AccountAddress, SigningKey>,

    /// Account that has balance at the moment of the latest "natural" block (one that was naturally generated in network, but not via generator tool)
    /// It may be an account from genesis allocation, or just any account with enough balances.
    /// This balance will be distributed between `Self::accounts`;
    /// This account is used once, in the very first generated block, the "distributor block" and
    /// generator never uses this one again for simplicity.
    /// So basically, this is going to be drained.
    pub(crate) fund_account: FundAccount,

    /// Track available UTXOs for each account for transaction generation
    pub(crate) account_utxos: IndexMap<AccountAddress, IndexSet<UtxoInfo>>,
}

/// Accounts list with corresponding private keys and its UTXOs.
pub type AccountsWithUtxos = Vec<(AccountAddress, SigningKey, IndexSet<UtxoInfo>)>;

impl GenerationState {
    /// Create new instance, with `n` of random accounts, that generated with `base_seed` to generate accounts.
    /// `fund_account` is AccountAddress and its corresponding PrivateKey that has funds in this chain.
    /// `start_height` is the height from which the accounts are generated, this is used for accounts generation.
    /// See `Self::generate_accounts` for more details
    // TODO: Maybe add GenerationState::new_with_accounts(accounts: Vec<SigningKey>, base_seed: u64)
    pub fn new_with_random_accounts(
        n: usize,
        fund_account: FundAccount,
        base_seed: u64,
        start_height: u64,
    ) -> Self {
        Self {
            accounts: Self::generate_accounts(n, base_seed, start_height),
            fund_account,
            account_utxos: Default::default(),
        }
    }

    /// Print some stats about currently existing UTXOs
    pub(crate) async fn log_utxos_state(
        &mut self,
        storage: &StryiStorage,
        // true if log about generation start, false if about the generation end
        start: bool,
    ) -> Result<(), StryiStorageError> {
        // Lock storage and retrieve all UTXOs
        let account_to_utxo_map = storage.get_all_utxos().await?;

        // Count total UTXOs
        let total_utxos: usize = account_to_utxo_map.values().map(|m| m.len()).sum();
        info!(
            "There are {} UTXOs available in storage at the moment of the generation {}",
            total_utxos,
            if start { "start" } else { "finish" },
        );

        // Calculate balances per account
        let mut balances: Vec<(AccountAddress, usize, u64)> = account_to_utxo_map
            .iter()
            .map(|(address, utxos)| {
                let utxo_count = utxos.len();
                let total_value: u64 = utxos.values().map(|utxo| utxo.value).sum();
                (*address, utxo_count, total_value)
            })
            .collect();

        // Sort by balance (descending)
        balances.sort_by(|a, b| b.2.cmp(&a.2));

        // Print top balances
        let balances_to_print = std::cmp::min(balances.len(), 20);
        info!("Top {} account balances:", balances_to_print);
        for (i, (address, utxo_count, balance)) in
            balances.iter().take(balances_to_print).enumerate()
        {
            info!(
                "  #{}: {} - {} UTXOs, {} coins",
                i + 1,
                address,
                utxo_count,
                balance
            );
        }

        Ok(())
    }

    /// Gets a list of the best recipients for tx outputs.
    /// The best are ones with lower amount of inputs they own.
    /// Get the top `n` accounts with the most available UTXOs for generating transactions in a new block.
    pub(crate) fn get_best_receivers(&self, n: usize) -> Option<Vec<AccountAddress>> {
        if self.accounts.is_empty() || n == 0 {
            return Some(Vec::new());
        }

        if self.accounts.len() < n {
            return None;
        }

        // Create a vector of (address, utxo_count) pairs
        let mut addr_counts: Vec<_> = self
            .accounts
            .keys()
            .map(|addr| {
                let count = self.account_utxos.get(addr).map_or(0, |utxos| utxos.len());
                (*addr, count)
            })
            .collect();

        // Sort by least UTXOs available. Use account address as a tie-breaker.
        addr_counts.sort_by(|(a_addr, a_cnt), (b_addr, b_cnt)| {
            a_cnt.cmp(b_cnt).then_with(|| a_addr.data.cmp(&b_addr.data))
        });

        Some(
            addr_counts
                .into_iter()
                .take(n)
                .map(|(addr, _)| addr)
                .collect(),
        )
    }

    /// Returns accounts sorted by their UTXO count in descending order.
    /// Only includes accounts that have at least one available UTXO.
    pub(crate) fn get_top_accounts_with_utxos(&self, n: usize) -> AccountsWithUtxos {
        let mut accounts_with_utxos: Vec<_> = self
            .account_utxos
            .iter()
            // Filter out accounts with no UTXOs
            .filter(|(_addr, utxos)| !utxos.is_empty())
            .filter_map(|(addr, utxos)| {
                self.accounts.get(addr).map(|signing_key| {
                    let mut utxos_vec: Vec<_> = utxos.iter().cloned().collect();

                    // Canonical, deterministic order (txid, then vout)
                    utxos_vec.sort_by(|a, b| {
                        a.outpoint
                            .txid
                            .data
                            .cmp(&b.outpoint.txid.data)
                            .then(a.outpoint.vout.cmp(&b.outpoint.vout))
                    });

                    let utxos_sorted: IndexSet<_> = utxos_vec.into_iter().collect();

                    let utxo_count = utxos_sorted.len();

                    (*addr, signing_key.clone(), utxos_sorted, utxo_count)
                })
            })
            .collect();

        // Sort by UTXO count in descending order (most UTXOs first) with deterministic tie-break
        accounts_with_utxos.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.data.cmp(&b.0.data)));

        // Take first `n` accounts and remove the utxo_count field so we can return proper AccountsWithUtxos
        accounts_with_utxos
            .into_iter()
            .take(n)
            .map(|(addr, key, utxos, _count)| (addr, key, utxos))
            .collect()
    }

    /// Helper to generate N deterministic accounts from a base seed and from a start height.
    /// Determinism in generation includes start height. If we generate accounts from the same seed and start height,
    /// we will get the same accounts, and if we run chaingen tool multiple times, it may actually break our logic.
    /// Uses deterministic key generation for reproducibility.
    /// All the accounts must be generated before even the first block is generated.
    fn generate_accounts(
        amount: usize,
        base_seed: u64,
        start_height: u64,
    ) -> IndexMap<AccountAddress, SigningKey> {
        // Add start height to the base seed
        let base_seed = base_seed.wrapping_pow(if start_height == 0 {
            1
        } else {
            start_height as u32
        });

        let mut accounts = IndexMap::new();

        for i in 0..amount {
            // Create a deterministic seed for each account by combining base seed with index
            let account_seed = base_seed.wrapping_add(i as u64);

            let mut rng = rand::rngs::StdRng::seed_from_u64(account_seed);

            #[allow(deprecated)]
            let signing_key = SigningKey::random(&mut rng);

            let verifying_key = signing_key.verifying_key();
            let account_address = AccountAddress::from_public_key(verifying_key);
            accounts.insert(account_address, signing_key);
        }
        accounts
    }

    /// Saves accounts in a provided dir, in file `CHAINGEN_ACCOUNTS_{start_height}_{end_height}.txt`
    /// In this file, each line contains an account address and corresponding private key in hex format separated by colon (:)
    pub fn save_accounts(
        &self,
        path: PathBuf,
        start_height: u64,
        end_height: u64,
    ) -> Result<(), String> {
        // Check that path is dir and exists
        if !path.is_dir() {
            return Err(format!(
                "Provided path ({}) is either not a path, or not a valid dir",
                &path.to_str().unwrap_or("UNPRINTABLE PATH")
            ));
        }
        let file_path = path.join(format!(
            "CHAINGEN_ACCOUNTS_{}_{}.txt",
            start_height, end_height
        ));
        debug!(
            "Path to generate private key backup : {}",
            file_path.to_str().unwrap()
        );
        let mut file = File::create_new(&file_path).map_err(|e| {
            format!(
                "Unable to create file for creating private keys backup, error: `{}`.",
                e
            )
        })?;

        info!("Backing up generated accounts in {:?}", path);

        let pairs: Vec<(AccountAddress, PrivateKey)> = self
            .accounts
            .clone()
            .iter()
            .map(|(addr, signing_key)| (addr.to_owned(), PrivateKey::new(signing_key.clone())))
            .collect();

        // Create buffer before writing
        let mut buffer = String::with_capacity(pairs.len() * 100);

        // construct lines in our format
        for (account_address, private_key) in pairs {
            let line = format!("{}:{}\n", account_address, private_key);
            buffer.push_str(&line);
        }

        // write in file
        file.write_all(buffer.as_bytes())
            .map_err(|e| format!("Unable to write backup in file, error : {}", e))
    }

    /// Select multiple UTXOs based on the given criteria
    /// Returns up to `count` distinct UTXOs sorted according to the criteria
    /// Each UTXO will appear at most once in the result
    pub(crate) fn select_utxos_by_criteria(
        &self,
        utxos: &IndexSet<UtxoInfo>,
        criteria: UtxoSelectionCriteria,
        count: usize,
    ) -> Vec<UtxoInfo> {
        if utxos.is_empty() || count == 0 {
            return Vec::new();
        }

        let mut utxo_vec: Vec<UtxoInfo> = utxos.iter().cloned().collect();

        match criteria {
            UtxoSelectionCriteria::Oldest => {
                // Sort by height, then by outpoint for stable ordering
                utxo_vec.sort_by(|a, b| a.height_created.cmp(&b.height_created));
            }
            UtxoSelectionCriteria::Newest => {
                utxo_vec.sort_by(|a, b| b.height_created.cmp(&a.height_created));
            }
            UtxoSelectionCriteria::Largest => {
                utxo_vec.sort_by(|a, b| b.value.cmp(&a.value));
            }
            UtxoSelectionCriteria::Smallest => {
                utxo_vec.sort_by(|a, b| a.value.cmp(&b.value));
            }
        }

        utxo_vec.into_iter().take(count).collect()
    }

    /// Applies block to GenerationState.
    pub fn apply_block(&mut self, block: &Block) {
        let height = block.header.height;

        // Transactions in the block
        let txs = &block.data.transactions;

        // Collect all spent OutPoints from inputs
        let mut spent_outpoints: Vec<OutPoint> = Vec::new();
        for tx in txs.iter() {
            for input in tx.data.inputs.iter() {
                spent_outpoints.push(input.previous_output);
            }
        }

        // Remove all spent UTXOs from account_utxos
        if !spent_outpoints.is_empty() {
            let spent_set: HashSet<OutPoint> = spent_outpoints.into_iter().collect();

            // For each account, remove any utxo whose outpoint is in spent_set
            for (_addr, utxos) in self.account_utxos.iter_mut() {
                utxos.retain(|u| !spent_set.contains(&u.outpoint))
            }
        }

        // Add outputs as new UTXOs
        for tx in txs.iter() {
            let tx_id = tx.data.hash();
            let is_coinbase = tx.data.kind == TransactionKind::Coinbase;

            for (idx, output) in tx.data.outputs.iter().enumerate() {
                let owner: AccountAddress = output.recipient;
                let value: u64 = output.value;

                // Construct OutPoint for the new UTXO
                let outpoint = OutPoint {
                    txid: tx_id,
                    vout: idx as u32,
                };

                // Construct new UtxoInfo
                let new_utxo = UtxoInfo {
                    outpoint,
                    value,
                    height_created: height,
                    is_coinbase,
                };

                // Insert into account_utxos for the owner
                self.account_utxos
                    .entry(owner)
                    .or_default()
                    .insert(new_utxo);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_accounts_deterministic() {
        // Generate accounts with the same seed and start_height twice
        let accounts1 = GenerationState::generate_accounts(10, 42, 0);
        let accounts2 = GenerationState::generate_accounts(10, 42, 0);

        // Check we get exactly 10 accounts
        assert_eq!(accounts1.len(), 10, "Should generate exactly 10 accounts");

        // Check determinism by comparing against the second generation
        assert_eq!(
            accounts1.len(),
            accounts2.len(),
            "Should generate same number of accounts"
        );

        // Compare each account address
        for (addr1, _) in accounts1 {
            assert!(
                accounts2.contains_key(&addr1),
                "Same address should be present in both generations"
            );
        }
    }
}
