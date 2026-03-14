//! Utils to deal with funding accounts.
use indexmap::IndexMap;
use stryi_core::PrivateKey;
use stryi_core::address::AccountAddress;
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::{OutPoint, UTXO};
use stryi_storage::StryiStorage;
use tracing::{debug, trace};

/// FundAccount represents an account that will distribute own balance to other accounts
/// for generating purposes.
#[derive(Clone, PartialEq, Debug)]
pub struct FundAccount {
    /// Funder address
    funder_address: AccountAddress,

    /// private key of this account
    private_key: PrivateKey,

    /// List of all the available UTXOs for this address mapped by their outputs.
    utxos: IndexMap<OutPoint, UTXO>,
}

static MINIMAL_TOTAL_AVAILABLE_BALANCE_FUNDING_ACCOUNT: u64 = 1_000_000;

impl FundAccount {
    /// Builds FundAccount
    /// Queries all the available utxos for this account, returns error if no balance is available
    pub async fn load_from_private_key(
        private_key: PrivateKey,
        storage: &StryiStorage,
    ) -> Result<Self, String> {
        let signing_key = private_key.clone().into_inner();
        let funder_address = AccountAddress::from_public_key(signing_key.verifying_key());

        trace!(
            "Getting all the available UTXOs for address {} for future distribution.",
            funder_address
        );

        let utxos = storage.get_utxos_for_address(funder_address).await.unwrap();
        let utxos = IndexMap::from_iter(utxos);
        debug!("All the available utxos of funder account : {:#?}", utxos);

        // quick check if this account even has balance
        if utxos.is_empty() {
            return Err(
                "Provided funding key, apparently, has no available balance in chain for distribution.".to_string()
            );
        }

        // Check if funding account has enough balance to be funder
        let balance = utxos.values().map(|utxo| utxo.value).sum::<u64>();
        if balance < MINIMAL_TOTAL_AVAILABLE_BALANCE_FUNDING_ACCOUNT {
            return Err(format!(
                "Funder account has not enough balance to be funder. It has {} out of {} minimal",
                balance, MINIMAL_TOTAL_AVAILABLE_BALANCE_FUNDING_ACCOUNT
            ));
        }

        Ok(Self {
            funder_address,
            private_key,
            utxos,
        })
    }

    // -- Getters --

    /// Gets address
    pub fn address(&self) -> AccountAddress {
        self.funder_address
    }

    /// Returns sum of all the output values from owned by this account UTXOs.
    /// NOTE: This value is not 100% spendable.
    pub fn total_balance(&self) -> u64 {
        self.utxos.values().map(|utxo| utxo.value).sum()
    }

    /// Gets the private key
    pub fn private_key(&self) -> &PrivateKey {
        &self.private_key
    }

    /// Gets a list of all the available UTXOs for this address mapped by their outputs.
    pub fn utxos(&self) -> IndexMap<OutPoint, UTXO> {
        self.utxos.clone()
    }
}
