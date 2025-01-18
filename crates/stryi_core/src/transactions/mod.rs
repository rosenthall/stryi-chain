mod utxo;
mod hash;
mod utxo_processor;
mod signature;

use serde::{Deserialize, Serialize};

use bincode::{self, config::standard};
use k256::{
    ecdsa::SigningKey
};
use k256::ecdsa::VerifyingKey;
use k256::ecdsa::signature::hazmat::PrehashVerifier;
use crate::address::AccountAddress;
use crate::error::StryiCoreError;
use crate::hash::HashKind;

use crate::transactions::hash::TransactionHasher;

// Exports
pub use crate::transactions::hash::{TransactionHash};
pub use crate::transactions::utxo::{TransactionIn, TransactionOut, OutPoint, UTXO};
pub use crate::transactions::signature::StryiSignature;

/// `TransactionData` holds the *unsigned* transaction fields: version, inputs, outputs.
/// It does NOT contain any cryptographic signature by itself.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TransactionData {
    /// Transaction version (arbitrary field for potential future upgrades)
    pub version: u32,

    /// Transaction inputs (what UTXOs we're spending)
    pub inputs: Vec<TransactionIn>,

    /// Transaction outputs (where the new coins are going, and how many)
    pub outputs: Vec<TransactionOut>,
}

/// `Transaction` is the fully signed transaction.
/// It wraps `TransactionData` plus a signature (which in this code is 65 bytes of
/// recoverable ECDSA format).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Transaction {
    /// The actual transaction data (version, inputs, outputs)
    pub data: TransactionData,

    /// The signature over the hash of `TransactionData`.
    pub signature: StryiSignature,
}

impl TransactionData {
    /// Computes a 32-byte Blake3-based hash of `TransactionData`.
    /// (We do NOT include any signature here, since it's the unsigned data.)
    ///
    /// This hash is used as the message to sign/verify in ECDSA.
    pub fn hash(&self) -> TransactionHash {
        let encoded = bincode::serde::encode_to_vec(self, standard())
            .expect("Failed to serialize TransactionData for hashing");
        TransactionHash::new(&encoded)
    }


    /// Signs this `TransactionData` using secp256k1, producing a `Transaction`
    /// containing the data plus a **recoverable** ECDSA signature.
    ///
    /// Steps:
    /// 1) Compute a 32-byte message from `self.hash()`.
    /// 2) Sign that message with `sign_ecdsa_recoverable(...)`.
    /// 3) Convert to a 65-byte array: [ recovery_id_byte | 64 bytes of (r, s) ].
    ///
    /// The resulting `Transaction` stores that 65-byte array in `signature`.
    pub fn sign(self, signing_key : &SigningKey) -> Transaction {
        // 1) Compute the 32-byte message hash from TransactionData
        let msg_bytes: [u8; TransactionHasher::SIZE] = self.hash().data;


        let (signature, recid) = signing_key.sign_prehash_recoverable(&msg_bytes).expect("Idk how did you got this");


        // First byte is quired to define RecoveryId value for the signature
        // Actually it takes just two bits

        // Convert that to a 65-byte representation:
        // - 1 byte for recovery ID
        // - 64 bytes for (r, s)
        let mut signature_bytes = [0u8; 65];
        signature_bytes[0] = recid.to_byte();  // recovery ID
        signature_bytes[1..].copy_from_slice(&*signature.to_bytes()); // r, s

        Transaction {
            data: self,
            signature: StryiSignature(Box::new(signature_bytes)),  // store the 65 bytes
        }
    }
}

impl Transaction {
    
    /// Verifies the transaction's signature using the provided `VerifyingKey`.
    /// Returns `Ok(())` if the signature is valid, otherwise returns an error.
    pub fn verify_signature(&self, verifying_key: &VerifyingKey) -> Result<(), StryiCoreError> {
        // Recompute the message hash from transaction data
        let msg_bytes = self.data.hash().data;
        
        // Try extract signature value from compat signature bytes
        // (verifying does not actually require key restoration) 
        let (_recovery_id, signature) = self.signature.extract_signature_parts()?;

        
        // Use the verifying key to check the signature against the message hash
        verifying_key.verify_prehash(&msg_bytes, &signature)
            .map_err(|e| StryiCoreError::InvalidSignature {
                msg: format!("Signature verification failed: {e}"),
            })
    }

    /// Recovers the public key from the **recoverable** signature stored in `self.signature`.
    ///
    /// This is possible because we're storing the 65-byte format:
    ///   [recovery ID (1 byte) | r, s (64 bytes)].
    /// If the signature is invalid or the format is wrong, returns `StriyCoreError::InvalidSignature`
    pub fn recover_public_key(&self, msg: Vec<u8>) -> Result<VerifyingKey, StryiCoreError> {
        // Try extract values from signature bytes
        let (recovery_id, signature) = self.signature.extract_signature_parts()?;

        // Try recover key, return error if cannot
        let recovered_key = VerifyingKey::recover_from_prehash(
            &msg,
            &signature,
            recovery_id
        ).map_err(|e| StryiCoreError::InvalidSignature {
            msg : format!("Cannot recover key from signature: {e:?}")
        })?;
        
        
        Ok(recovered_key)
        
    }

    /// Verifies that this transaction's recoverable signature recovers to real public key of this account.
    /// Since AccountAddress is hashed public key we will check if recovered public key hash is identical with real AccountAddress.
    pub fn verify_transaction_author(&self, account_address: AccountAddress) -> bool {

        let recovered_key = self.recover_public_key(self.data.hash().data.to_vec());
        
        // If we cant recover key consider returning false.
        if recovered_key.is_err() {
            return false;
        }
        
        let recovered_account_address = AccountAddress::new(&recovered_key.unwrap().to_sec1_bytes());


        recovered_account_address == account_address

    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::rand_core::OsRng;

    /// Helper function to create a dummy TransactionData with sample inputs and outputs.
    /// Adjust this function as necessary to suit your actual `TransactionIn` and `TransactionOut` types.
    fn create_dummy_transaction_data() -> TransactionData {
        // Using empty vectors for inputs and outputs for simplicity.
        TransactionData {
            version: 1,
            inputs: vec![TransactionIn {
                previous_output: OutPoint { 
                    txid: TransactionHash::new(&[20u8;32]),
                    vout: 15 },
                signature: StryiSignature(Box::new([1u8; 65])),
                sequence: 2,
            }],
            outputs: vec![TransactionOut {
                value: 55555,
                recipient: AccountAddress::new(&[20u8;32]) }
            ],
        }
    }

    #[test]
    fn test_sign_and_verify_transaction_author() {
        // Generate a random signing key
        let signing_key = SigningKey::random(&mut OsRng);
        let verify_key = signing_key.verifying_key();

        // Create AccountAddress based on the verifying key
        let account_address = AccountAddress::new(&verify_key.to_sec1_bytes());

        // Create dummy transaction data and sign it
        let tx_data = create_dummy_transaction_data();
        let transaction = tx_data.sign(&signing_key);

        // Verify that the transaction's author matches the account address
        let is_verified = transaction.verify_transaction_author(account_address);
        assert!(is_verified, "The transaction should be verified successfully.");
    }
    
    #[test]
    fn test_verify_transaction_author_wrong_key() {
        // Generate two different key pairs
        let signing_key_sender = SigningKey::random(&mut OsRng);
        let verify_key_sender = signing_key_sender.verifying_key();
        let signing_key_other = SigningKey::random(&mut OsRng);
        let verify_key_other = signing_key_other.verifying_key();

        let account_address_sender = AccountAddress::new(&verify_key_sender.to_sec1_bytes());
        let account_address_other = AccountAddress::new(&verify_key_other.to_sec1_bytes());

        // Create and sign transaction with sender's key
        let tx_data = create_dummy_transaction_data();
        let transaction = tx_data.sign(&signing_key_sender);

        
        // Verify with real author address
        assert!(transaction.verify_transaction_author(account_address_sender), "Verification should be ok");


        // Try verifying the transaction against a different account address (other)
        assert!(!transaction.verify_transaction_author(account_address_other),
                "Verification should fail when using a wrong account address.");
    }

    #[test]
    fn test_recover_and_compare_public_key() {
        // Generate a key pair
        let signing_key = SigningKey::random(&mut OsRng);
        let verify_key = signing_key.verifying_key();

        // Create dummy transaction data and sign it
        let tx_data = create_dummy_transaction_data();
        let transaction = tx_data.sign(&signing_key);

        // Attempt to recover the public key from the signature
        let recovered_key_result = transaction.recover_public_key(transaction.data.hash().data.to_vec());
        assert!(recovered_key_result.is_ok(), "Public key recovery should succeed.");

        let recovered_key = recovered_key_result.unwrap();

        // Compare recovered key with the original verifying key
        assert_eq!(
            recovered_key.to_sec1_bytes(),
            verify_key.to_sec1_bytes(),
            "Recovered public key should match the original verifying key."
        );
    }
    
    #[test]
    fn test_verify_signature() {
        // Generate a random signing key and its corresponding verifying key
        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = signing_key.verifying_key();



        let another_signing_key = SigningKey::random(&mut OsRng);
        let another_verifying_key = another_signing_key.verifying_key();

        // Create dummy transaction data and sign it
        let tx_data = create_dummy_transaction_data();
        let transaction = tx_data.sign(&signing_key);

        // Verify the signature using the verifying key
        assert!(transaction.verify_signature(&verifying_key).is_ok(), "Signature should verify successfully.");
        
        // Check that .verify will throw en error if incorrect verifying key
        assert!(transaction.verify_signature(&another_verifying_key).is_err(), "Signature should not be verified successfully.");

    }

}
