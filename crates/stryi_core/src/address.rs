use crate::hash::{Hash, HashKind};
use blake3;
use secp256k1::PublicKey;

/// Specific hash kind for account addresses (20 bytes).
#[derive(Default, PartialEq, Debug, Clone, Copy)]
pub struct AddressHasher;

impl HashKind for AddressHasher {
    const SIZE: usize = 20;
    const PREFIX: &'static str = "@";

    fn hash(public_key: &[u8]) -> [u8; Self::SIZE] {
        // We use Blake3 to hash the serialized public key, then take 20 bytes of output.
        let mut hasher = blake3::Hasher::new();
        hasher.update(public_key);

        let mut data = [0u8; Self::SIZE];
        hasher.finalize_xof().fill(&mut data);

        data
    }
}

/// Type alias for AccountAddress using the Hash<AddressHasher> abstraction.
pub type AccountAddress = Hash<AddressHasher>;

impl AccountAddress {
    /// Creates an account address from a secp256k1 public key by hashing it (Blake3, truncated to 20 bytes).
    /// By default, we use the compressed public key serialization (33 bytes).
    pub fn from_public_key(pubkey: &PublicKey) -> Self {
        let pubkey_bytes = pubkey.serialize(); // 33 bytes in compressed form
        Self::new(&AddressHasher::hash(&pubkey_bytes))
    }
}


#[cfg(test)]
mod tests {
    use secp256k1::{Secp256k1, rand::thread_rng};
    use crate::address::{AccountAddress, AddressHasher};
    use crate::hash::HashKind;

    #[test]
    fn test_create_multiple_account_addresses() {
        let secp = Secp256k1::new();
        let mut rng = thread_rng();

        for i in 0..15 {
            // Generate a random ECDSA keypair (secp256k1)
            let (secret_key, public_key) = secp.generate_keypair(&mut rng);

            // Create AccountAddress from the secp256k1 public key
            let account_address = AccountAddress::from_public_key(&public_key);

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

            // Ensure the hash data is of correct size
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
                account_address.data,
                converted_account_address.data,
                "Round-trip from string to address mismatch"
            );

            // Optionally, print the address
            println!("Account Address {}: {}", i + 1, address_string);
        }
    }

}