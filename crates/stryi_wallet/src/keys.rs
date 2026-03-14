//! Wallet key management.

use anyhow::{Context, Result};
use k256::ecdsa::SigningKey;
use k256::elliptic_curve::rand_core::OsRng;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use stryi_core::PrivateKey;
use stryi_core::address::AccountAddress;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEntry {
    pub label: String,
    pub private_key: PrivateKey,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletFile {
    pub keys: Vec<KeyEntry>,
}

pub fn generate_keypair(label: &str) -> KeyEntry {
    let signing_key = SigningKey::random(&mut OsRng);
    let address = AccountAddress::from_public_key(signing_key.verifying_key());

    KeyEntry {
        label: label.to_string(),
        private_key: PrivateKey::new(signing_key),
        address: address.to_string(),
    }
}

pub fn import_key(hex_key: &str, label: &str) -> Result<KeyEntry> {
    let private_key =
        PrivateKey::try_from(hex_key.to_string()).context("invalid private key hex")?;
    let signing_key = private_key.clone().into_inner();
    let address = AccountAddress::from_public_key(signing_key.verifying_key());

    Ok(KeyEntry {
        label: label.to_string(),
        private_key,
        address: address.to_string(),
    })
}

pub fn load_wallet(path: &Path) -> Result<WalletFile> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("cannot read wallet file at {}", path.display()))?;
    let wallet: WalletFile = serde_json::from_str(&data).context("failed to parse wallet JSON")?;
    Ok(wallet)
}

/// Loads a wallet file, returning a clear error if no wallet exists yet.
pub fn require_wallet(path: &Path) -> Result<WalletFile> {
    load_wallet(path).context("no wallet found — run `stryi-wallet init` first")
}

/// Creates parent dirs if needed.
pub fn save_wallet(path: &Path, wallet: &WalletFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create directory {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(wallet).context("failed to serialize wallet")?;
    fs::write(path, json)
        .with_context(|| format!("cannot write wallet file at {}", path.display()))?;
    Ok(())
}

pub fn find_key_by_address<'a>(wallet: &'a WalletFile, address: &str) -> Result<&'a KeyEntry> {
    wallet
        .keys
        .iter()
        .find(|k| k.address == address)
        .ok_or_else(|| {
            let available: Vec<&str> = wallet.keys.iter().map(|k| k.address.as_str()).collect();
            if available.is_empty() {
                anyhow::anyhow!("wallet is empty - run `stryi-wallet generate` first")
            } else {
                anyhow::anyhow!(
                    "address {} not found in wallet. Available: {}",
                    address,
                    available.join(", ")
                )
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn generate_and_roundtrip() {
        let entry = generate_keypair("test");
        assert!(entry.address.starts_with('@'));
        assert_eq!(entry.label, "test");

        let hex = entry.private_key.to_string();
        let recovered = PrivateKey::try_from(hex).unwrap();
        let addr = AccountAddress::from_public_key(recovered.into_inner().verifying_key());
        assert_eq!(addr.to_string(), entry.address);
    }

    #[test]
    fn import_known_key() {
        let original = generate_keypair("original");
        let hex = original.private_key.to_string();

        let imported = import_key(&hex, "imported").unwrap();
        assert_eq!(imported.address, original.address);
    }

    #[test]
    fn import_invalid_hex() {
        assert!(import_key("not_valid_hex", "bad").is_err());
        assert!(import_key("", "empty").is_err());
        assert!(import_key("00", "short").is_err());
    }

    #[test]
    fn save_and_load_wallet() {
        let wallet = WalletFile {
            keys: vec![generate_keypair("a"), generate_keypair("b")],
        };

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        save_wallet(&path, &wallet).unwrap();
        let loaded = load_wallet(&path).unwrap();

        assert_eq!(loaded.keys.len(), 2);
        assert_eq!(loaded.keys[0].address, wallet.keys[0].address);
        assert_eq!(loaded.keys[1].address, wallet.keys[1].address);
    }

    #[test]
    fn find_key_by_address_works() {
        let wallet = WalletFile {
            keys: vec![generate_keypair("first"), generate_keypair("second")],
        };
        let addr = &wallet.keys[1].address;
        let found = find_key_by_address(&wallet, addr).unwrap();
        assert_eq!(found.label, "second");
    }

    #[test]
    fn find_key_missing_address() {
        let wallet = WalletFile {
            keys: vec![generate_keypair("only")],
        };
        assert!(find_key_by_address(&wallet, "@0000000000000000000000000000000000000000").is_err());
    }
}
