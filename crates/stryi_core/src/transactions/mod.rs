/// Definitions of basic primitives of transactions such as UTXO, OutPoint, etc.
mod utxo;
pub use crate::transactions::utxo::{
    UTXO, OutPoint, TransactionIn, TransactionOut,
};

/// Definition of custom hash format for transactions based on Blake3.
/// Note: Each transaction hash start with Tx... and contains 32 hex bytes. 
mod hash;
pub use crate::transactions::hash::{TransactionHash, TransactionHasher};

/// Implementation of UtxoProcessor
mod utxo_processor;
pub use crate::transactions::utxo_processor::UtxoProcessor;

/// high-level abstractions for k256-based signatures of transactions
mod signature;
pub use crate::transactions::signature::StryiSignature;

/// Definition of FeePolicy and FeeCalculator for estimating required fee for any transaction.
mod fee_policy;
pub use fee_policy::*;

use serde::{Deserialize, Serialize};

use bincode::{self, config::standard};
use crate::address::AccountAddress;
use crate::error::StryiCoreError;
use crate::hash::HashKind;
use crate::transactions::TransactionKind::{Coinbase, Genesis};
use k256::ecdsa::{signature::hazmat::PrehashVerifier, SigningKey, VerifyingKey};

/// `TransactionKind` enum represents the exact kind of transaction.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Hash)]
#[repr(u8)]
pub enum TransactionKind {
    /// Coinbase is a type of transaction that is used to reward the miner of the last block.
    /// Coinbase transactions are always the very first in each block except for `Genesis`.
    /// This kind also forbids any TxIns in the transaction and must contain exactly one TxOut.
    Coinbase,

    /// Genesis transaction is a unique transaction that happens only in the Genesis Block.
    /// It is used to define initial account balances.
    Genesis,

    /// Payment transaction represents a standard token transfer between parties.
    Payment,
}


/// `TransactionData` holds the *unsigned* transaction fields: version, inputs, outputs.
#[derive(Debug, Serialize, Deserialize, Clone, Eq, Hash, PartialEq)]
pub struct TransactionData {
    /// Transaction version (arbitrary field for potential future upgrades)
    pub version: u16,
    
    /// Transaction kind represents HOW the transaction should be processed.
    pub kind : TransactionKind,
    
    /// Transaction inputs (what UTXOs we're spending)
    pub inputs: Vec<TransactionIn>,

    /// Transaction outputs (where the new coins are going, and how many)
    pub outputs: Vec<TransactionOut>,
}

/// `Transaction` is the fully signed transaction.
/// It wraps `TransactionData` plus a single ECDSA recoverable signature
/// (65 bytes) for the entire transaction.
#[derive(Debug, Serialize, Deserialize, Clone, Eq, Hash, PartialEq)]
pub struct Transaction {
    /// The actual transaction data (version, inputs, outputs)
    pub data: TransactionData,

    /// The single signature over the hash of `TransactionData`.
    pub(crate) signature: StryiSignature,
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
    /// with a **recoverable** ECDSA signature (65 bytes).
    ///
    /// Steps:
    /// 1) Compute the 32-byte message from `self.hash()`.
    /// 2) Sign that message with `sign_ecdsa_recoverable(...)`.
    /// 3) Convert to a [1-byte recId | 64-byte (r,s)] array.
    pub fn sign(self, signing_key: &SigningKey) -> Transaction {
        let msg_bytes: [u8; TransactionHasher::SIZE] = self.hash().data;

        let (signature, recid) = signing_key
            .sign_prehash_recoverable(&msg_bytes)
            .expect("ECDSA signing failed for TransactionData");

        // Assemble the 65-byte representation
        let mut signature_bytes = [0u8; 65];
        signature_bytes[0] = recid.to_byte();
        signature_bytes[1..].copy_from_slice(&signature.to_bytes());

        Transaction {
            data: self,
            signature: StryiSignature(Box::new(signature_bytes)),
        }
    }
}

impl Transaction {
    /// Getter for the `signature` field.
    pub fn signature(&self) -> StryiSignature {
        self.signature.to_owned()
    }
    
    /// Verifies the transaction's signature using the provided `VerifyingKey`.
    /// Returns `Ok(())` if the transaction has [`Genesis`] or [`Coinbase`] kind, because these two do not require such checking
    /// Returns `Ok(())` if the signature is valid, otherwise returns an error.
    pub fn verify_signature(&self, verifying_key: &VerifyingKey) -> Result<(), StryiCoreError> {
        
        // If transaction kind is not payment - early return Ok(())
        if self.data.kind == Genesis || self.data.kind == Coinbase {
            return Ok(());
        }


        // Recompute the message hash from transaction data
        let msg_bytes = self.data.hash().data;

        let (_recovery_id, signature) = self.signature.extract_signature_parts()?;


        // Use the verifying key to check the signature against the message hash
        verifying_key.verify_prehash(&msg_bytes, &signature).map_err(|e| {
            StryiCoreError::InvalidSignature {
                msg: format!("Signature verification failed: {e}"),
            }
        })
    }
    
    /// Recovers the public key from the **recoverable** signature stored in `self.signature`.
    ///
    /// If the signature is invalid or the format is wrong, returns `InvalidSignature`.
    pub fn recover_public_key(&self) -> Result<VerifyingKey, StryiCoreError> {
        let msg_bytes = self.data.hash().data;
        let (recovery_id, signature) = self.signature.extract_signature_parts()?;

        VerifyingKey::recover_from_prehash(&msg_bytes, &signature, recovery_id)
            .map_err(|e| StryiCoreError::InvalidSignature {
                msg: format!("Cannot recover key from signature: {e:?}"),
            })
    }


    /// Verifies that this transaction's recoverable signature recovers to real public key of this account.
    /// Since AccountAddress is hashed public key we will check if recovered public key hash is identical with real AccountAddress.
    pub fn verify_transaction_author(&self, account_address: AccountAddress) -> bool {

        let recovered_key = self.recover_public_key();

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
    use crate::address::AccountAddress; 

    /// Helper function to create a dummy TransactionData with sample inputs/outputs.
    fn create_dummy_transaction_data() -> TransactionData {
        TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![
                TransactionIn {
                    previous_output: OutPoint {
                        txid: TransactionHash::new(&[20u8; 32]),
                        vout: 15,
                    },
                    sequence: 2,
                }
            ],
            outputs: vec![
                TransactionOut {
                    value: 55_555,
                    recipient: AccountAddress::new(&[20u8; 32]),
                }
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

        // Verify with real author address (sender)
        assert!(
            transaction.verify_transaction_author(account_address_sender),
            "Verification with the correct address should be ok"
        );

        // Try verifying against a different address
        assert!(
            !transaction.verify_transaction_author(account_address_other),
            "Verification should fail with a wrong account address"
        );
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
        // (no argument needed now)
        let recovered_key_result = transaction.recover_public_key();
        assert!(recovered_key_result.is_ok(), "Public key recovery should succeed");

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
        // Generate a random signing key and corresponding verifying key
        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        let another_signing_key = SigningKey::random(&mut OsRng);
        let another_verifying_key = another_signing_key.verifying_key();

        // Create dummy transaction data and sign it
        let tx_data = create_dummy_transaction_data();
        let transaction = tx_data.sign(&signing_key);

        // Verify the signature using the matching verifying key
        assert!(
            transaction.verify_signature(verifying_key).is_ok(),
            "Signature should verify with the correct key"
        );

        // Should fail with a different key
        assert!(
            transaction.verify_signature(another_verifying_key).is_err(),
            "Signature should fail to verify with an incorrect key"
        );
    }
}
