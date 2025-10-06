use crate::chaingen::txgen::FundAccount;
use crate::chaingen::utxo::{UtxoInfo, UtxoSelectionCriteria};
use k256::ecdsa::SigningKey;
use rand::SeedableRng;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use stryi_core::PrivateKey;
use stryi_core::address::AccountAddress;
use stryi_core::transactions::OutPoint;
use tracing::{debug, info};

/// Current state of chain's generation.
#[derive(Clone)]
pub struct GenerationState {
    /// Collection of all the accounts.
    /// @addr mapped by SigningKey of corresponding account.
    pub(crate) accounts: HashMap<AccountAddress, SigningKey>,

    /// Account that has balance before generation.
    /// This balance will be distributed between `Self::accounts`,
    /// This account is used once, in very first generated block, the "distributor block" and
    /// generator never uses this one again for simplicity.
    pub(crate) fund_account: FundAccount,

    /// Track available UTXOs for each account for transaction generation
    pub(crate) account_utxos: HashMap<AccountAddress, HashSet<UtxoInfo>>,
}

/// Accounts list with corresponding private keys and its UTXOs.
pub type AccountsWithUtxos = Vec<(AccountAddress, SigningKey, HashSet<UtxoInfo>)>;

impl GenerationState {
    /// Create new instance, with `n` of random accounts, that generated with `base_seed` to generate accounts.
    /// `fund_account` is AccountAddress and its corresponding PrivateKey that has funds in this chain.
    /// See `Self::generate_accounts` for more details.
    pub fn new_with_random_accounts(
        n: usize,
        fund_account: (AccountAddress, PrivateKey, u64, OutPoint),
        base_seed: u64,
    ) -> Self {
        Self {
            accounts: Self::generate_accounts(n, base_seed),
            fund_account,
            account_utxos: Default::default(),
        }
    }

    /// Saves accounts in provided dir, in file `.txt`
    /// Saving format is @AccountAddress:CorrespondingPrivateKeyInHex line for each address
    pub fn save_accounts(&self, path: PathBuf) -> Result<(), String> {
        // Check that path is dir and exists
        if !path.is_dir() {
            return Err(format!(
                "Provided path ({}) is either not a path, or not a valid dir",
                &path.to_str().unwrap_or("UNPRINTABLE")
            ));
        }

        let file_path = path.join("CHAINGEN_PRIVATE_KEY.txt");
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

    /// Get all the available UTXOs for provided address in current state.
    pub(crate) fn available_utxos(&self, addr: &AccountAddress) -> Option<HashSet<UtxoInfo>> {
        self.account_utxos.get(addr).cloned()
    }

    /// Removes a set of UTXOs from an account's available UTXO pool.
    pub(crate) fn spend_utxos(&mut self, owner: &AccountAddress, utxos_to_spend: &[UtxoInfo]) {
        if let Some(available_utxos) = self.account_utxos.get_mut(owner) {
            for utxo in utxos_to_spend {
                available_utxos.remove(utxo);
            }
        }
    }

    /// Adds a new UTXO to an account's available UTXO pool.
    pub(crate) fn add_utxo(&mut self, owner: AccountAddress, utxo: UtxoInfo) {
        self.account_utxos.entry(owner).or_default().insert(utxo);
    }

    /// Gets a list of the best recipients for tx outputs.
    /// The best are ones with lower amount of inputs they own.
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
                (addr, count)
            })
            .collect();

        // Sort by UTXO count ascending (least UTXOs first)
        addr_counts.sort_by_key(|&(_, count)| count);

        // Take first n addresses
        Some(
            addr_counts
                .into_iter()
                .take(n)
                .map(|(addr, _)| *addr)
                .collect(),
        )
    }

    /// Get the top `n` accounts with the most available UTXOs for generating transactions in a new block.
    /// Returns accounts sorted by their UTXO count in descending order.
    /// Only includes accounts that have at least one available UTXO.
    pub(crate) fn get_top_accounts_with_utxos(&self, n: usize) -> AccountsWithUtxos {
        let mut accounts_with_utxos: Vec<_> = self
            .account_utxos
            .iter()
            // Filter out accounts with no UTXOs
            .filter(|(_addr, utxos)| !utxos.is_empty())
            // Map to include signing key and UTXO count for sorting
            .filter_map(|(addr, utxos)| {
                // Get the signing key for this account
                self.accounts.get(addr).map(|signing_key| {
                    let utxo_count = utxos.len();
                    (*addr, signing_key.clone(), utxos.clone(), utxo_count)
                })
            })
            .collect();

        // Sort by UTXO count in descending order (most UTXOs first)
        accounts_with_utxos.sort_by(|a, b| b.3.cmp(&a.3));

        // Take first `n` accounts and remove the utxo_count field so we can return proper AccountsWithUtxos
        accounts_with_utxos
            .into_iter()
            .take(n)
            .map(|(addr, key, utxos, _count)| (addr, key, utxos))
            .collect()
    }

    /// Helper to generate N deterministic accounts from a base seed.
    /// Uses deterministic key generation for reproducibility.
    /// All the accounts must be generated before even the first block is generated.
    pub(crate) fn generate_accounts(
        amount: usize,
        base_seed: u64,
    ) -> HashMap<AccountAddress, SigningKey> {
        // Create a wrapper that bridges rand::StdRng to k256's rand_core
        struct StdRngWrapper(rand::rngs::StdRng);

        impl k256::elliptic_curve::rand_core::RngCore for StdRngWrapper {
            fn next_u32(&mut self) -> u32 {
                rand::RngCore::next_u32(&mut self.0)
            }
            fn next_u64(&mut self) -> u64 {
                rand::RngCore::next_u64(&mut self.0)
            }
            fn fill_bytes(&mut self, dest: &mut [u8]) {
                rand::RngCore::fill_bytes(&mut self.0, dest)
            }
            fn try_fill_bytes(
                &mut self,
                dest: &mut [u8],
            ) -> Result<(), k256::elliptic_curve::rand_core::Error> {
                rand::RngCore::fill_bytes(&mut self.0, dest);
                Ok(())
            }
        }

        impl k256::elliptic_curve::rand_core::CryptoRng for StdRngWrapper {}

        let mut accounts = HashMap::new();

        for i in 0..amount {
            // Create deterministic seed for each account by combining base seed with index
            let account_seed = base_seed.wrapping_add(i as u64);

            let mut rng = StdRngWrapper(rand::rngs::StdRng::seed_from_u64(account_seed));

            // Generate a random signing key
            let signing_key = SigningKey::random(&mut rng);

            let verifying_key = signing_key.verifying_key();
            let account_address = AccountAddress::from_public_key(verifying_key);
            accounts.insert(account_address, signing_key);
        }
        accounts
    }

    /// Select multiple UTXOs based on the given criteria
    /// Returns up to `count` distinct UTXOs sorted according to the criteria
    /// Each UTXO will appear at most once in the result
    pub(crate) fn select_utxos_by_criteria(
        &self,
        utxos: &HashSet<UtxoInfo>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_accounts_deterministic() {
        // Generate accounts with same seed twice
        let accounts1 = GenerationState::generate_accounts(10, 42);
        let accounts2 = GenerationState::generate_accounts(10, 42);

        // Check we get exactly 10 accounts
        assert_eq!(accounts1.len(), 10, "Should generate exactly 10 accounts");

        // Check determinism by comparing against second generation
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
