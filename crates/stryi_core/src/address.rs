use crate::hash::{Hash, HashKind};
use k256::ecdsa::VerifyingKey;

/// Specific hash kind for account addresses (20 bytes).
#[derive(Default, PartialEq, Debug, Clone, Copy, Eq, Hash)]
pub struct AddressHasher;

impl HashKind for AddressHasher {
    const SIZE: usize = 20;
    const PREFIX: &'static str = "@";

    // NOTE: using default hash() implementation, based on blake3
}

/// Type alias for AccountAddress using the Hash<AddressHasher> abstraction.
pub type AccountAddress = Hash<AddressHasher>;

impl AccountAddress {
    /// Creates an account address from a secp256k1 public key by hashing it (Blake3, truncated to 20 bytes).
    pub fn from_public_key(verifying_key: &VerifyingKey) -> Self {
        let pubkey_bytes = verifying_key.to_sec1_bytes();
        Self::new(&AddressHasher::hash(&pubkey_bytes))
    }
}

#[cfg(test)]
mod tests {
    use crate::PrivateKey;
    use crate::address::{AccountAddress, AddressHasher};
    use crate::hash::HashKind;
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::rand_core::OsRng;

    #[test]
    fn test_create_multiple_account_addresses() {
        for i in 0..15 {
            // Generate a random ECDSA keypair
            let signing_key = SigningKey::random(&mut OsRng);
            let verifying_key = signing_key.verifying_key();

            // Create AccountAddress from the secp256k1 public key
            let account_address = AccountAddress::from_public_key(verifying_key);

            // Convert AccountAddress to string and verify prefix and length
            let address_string = account_address.to_string();
            assert!(
                address_string.starts_with(AddressHasher::PREFIX),
                "Account Address {} does not start with the expected prefix",
                i + 1
            );
            assert_eq!(
                address_string.len(),
                AddressHasher::PREFIX.len() + 40, // 20 bytes in hex => 40 hex chars
                "Account Address {} does not have the expected length",
                i + 1
            );

            // Ensure the hash data is of the correct size
            assert_eq!(
                account_address.data.len(),
                AddressHasher::SIZE,
                "Account Address {} data length mismatch",
                i + 1
            );

            // Test converting string back to address
            let converted_account_address = AccountAddress::from_hash_string(&address_string)
                .expect("Failed to parse address string back");

            // Must be the same
            assert_eq!(
                account_address.data, converted_account_address.data,
                "Round-trip from string to address mismatch"
            );

            println!("Account Address {}: {}", i + 1, address_string);
        }
    }

    /// Just a helper to generate key and address, for manual testing or debugging.
    #[ignore]
    #[test]
    fn generate_key_and_address() {
        let signing_key = SigningKey::random(&mut OsRng);
        let s_pk = PrivateKey::new(signing_key.clone());

        let verifying_key = signing_key.verifying_key();

        let account_address = AccountAddress::from_public_key(verifying_key);
        println!("Account private key: {}", s_pk);
        println!("Account Address: {}", account_address);
    }
}
