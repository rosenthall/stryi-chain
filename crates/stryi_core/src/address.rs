use crate::hash::{Hash, HashKind};

use p256::ecdsa::VerifyingKey;

/// Specific hash kind for account addresses.
#[derive(Default, PartialEq, Debug, Clone, Copy)]
pub struct AddressHasher;

impl HashKind for AddressHasher {
    const SIZE: usize = 20;
    const PREFIX: &'static str = "@";

    fn hash(public_key: &[u8]) -> [u8; Self::SIZE] {
    
    
        // We will use blake3 for addresses
        let mut hasher = blake3::Hasher::new();
        hasher.update(public_key);

        let mut data = [0u8; AddressHasher::SIZE];
        hasher.finalize_xof().fill(&mut data);


        data
    }
}

/// Type alias for AccountAddress using the Hash<Address> abstraction.
pub type AccountAddress = Hash<AddressHasher>;

// specific methods for addresses
impl AccountAddress {
    /// Creates an account address from a public key by hashing it.
    pub fn from_public_key(public_key: VerifyingKey) -> Self {
        Self::new(&AddressHasher::hash(&*public_key.to_sec1_bytes()))
    }
    
}


#[cfg(test)]
mod tests {
    use p256::ecdsa::SigningKey;
    use old_rand::thread_rng; // using old version of rand here because ecdsa crate does the same (I HATE it)
    use crate::address::{AccountAddress, AddressHasher};
    use crate::hash::HashKind;

    #[test]
    fn test_create_multiple_account_addresses() {
        // Initialize a deterministic random number generator
        let mut rng = thread_rng();

        for i in 0..15 {
            // Generate a random ECDSA signing key
            let signing_key = SigningKey::random(&mut rng);
            let verifying_key = signing_key.verifying_key();

            // Create AccountAddress from the verifying key
            let account_address = AccountAddress::from_public_key(*verifying_key);

            // Convert AccountAddress to string and verify prefix and length
            let address_string = account_address.to_string();
            assert!(
                address_string.starts_with(AddressHasher::PREFIX),
                "Account Address {} does not start with the expected prefix",
                i + 1
            );
            assert_eq!(
                address_string.len(),
                AddressHasher::PREFIX.len() + 40, // 20 bytes in hex is 40 characters
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
            let converted_account_address= AccountAddress::from_hash_string(address_string.as_str()).unwrap();

            // Those must be the same
            assert_eq!(account_address.data, converted_account_address.data);


            // Optionally, print the address
            println!("Account Address {}: {}", i + 1, address_string);
        }
    }
}