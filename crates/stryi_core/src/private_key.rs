//! Simple serialization/deserialization for private keys in StryiChain
//! Why not [WIF](https://en.bitcoin.it/wiki/Wallet_import_format)?
//! WIF is a standard, but it may indeed be excessive for this project.
//! We don't need compatibility with BTC/wallets; we just need to transfer 32 bytes of the key to the program simply and reliably.

use crate::StryiCoreError;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt::{Debug, Display};

/// A private key (wraps [`k256::ecdsa::SigningKey`]).
#[derive(Clone)]
pub struct PrivateKey {
    inner: SigningKey,
}

impl PrivateKey {
    /// Wrap provided SigningKey
    pub fn new(signing_key: SigningKey) -> Self {
        Self { inner: signing_key }
    }

    /// Convert into inner.
    pub fn into_inner(self) -> SigningKey {
        self.inner
    }
}

impl Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateKey")
            .field("inner", &"<redacted>")
            .finish()
    }
}

impl PartialEq for PrivateKey {
    fn eq(&self, other: &Self) -> bool {
        self.inner.to_bytes().as_slice() == other.inner.to_bytes().as_slice()
    }
}

impl Eq for PrivateKey {}

impl Display for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.inner.to_bytes()))
    }
}

impl TryFrom<String> for PrivateKey {
    type Error = StryiCoreError;

    /// Attempts to create a PrivateKey from a hexadecimal string.
    ///
    /// # Arguments
    /// * `value` - A hexadecimal string representing the 32-byte private key
    ///
    /// # Returns
    /// * `Ok(PrivateKey)` if the string is valid hex and represents a valid private key
    /// * `Err` if the string is invalid hex or doesn't represent a valid private key
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let bytes = hex::decode(&value)?;

        let inner = SigningKey::from_slice(&bytes)
            .map_err(|e| Self::Error::InvalidPrivateKey { msg: e.to_string() })?;

        Ok(PrivateKey { inner })
    }
}

impl Serialize for PrivateKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PrivateKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_string = String::deserialize(deserializer)?;
        PrivateKey::try_from(hex_string).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::AccountAddress;
    use rand::rng;

    #[test]
    fn test_random_private_keys() {
        for i in 1..=20 {
            let signing_key = SigningKey::random(&mut rng());
            let private_key = PrivateKey::new(signing_key.clone());
            dbg!(i, private_key.to_string());

            // also generate AccountAddress
            println!(
                "{}:{}",
                i,
                AccountAddress::from_public_key(signing_key.verifying_key())
            );

            // Verify roundtrip conversion
            let recovered = PrivateKey::try_from(private_key.to_string()).unwrap();
            assert_eq!(
                private_key.inner.to_bytes().as_slice(),
                recovered.inner.to_bytes().as_slice()
            );
        }
    }

    #[test]
    fn test_private_key_bad_patterns() {
        // Test cases that should fail
        let too_long = "00".repeat(33);
        let missing_data = "00".repeat(31);

        let error_cases = vec![
            ("Invalid hex (G)", "GGAABB"),
            ("Invalid hex (Z)", "ZZAABB"),
            ("Odd length", "123"),
            ("Empty string", ""),
            ("Too short", "00"),
            ("Too long (33 bytes)", too_long.as_str()),
            ("Special characters", "##$$%%"),
            ("With spaces", "12 34 56"),
            ("Unicode", "❤️"),
            ("Missing data", missing_data.as_str()),
        ];

        for (case_name, invalid_input) in error_cases {
            let result = PrivateKey::try_from(invalid_input.to_string());
            assert!(
                result.is_err(),
                "Case '{}' with input '{}' should have failed but didn't",
                case_name,
                invalid_input
            );
        }
    }
}
