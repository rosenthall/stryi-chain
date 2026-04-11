/// Basic primitives of transactions such as UTXO, OutPoint, etc.
mod utxo;
pub use crate::transactions::utxo::{OutPoint, TransactionIn, TransactionOut, UTXO};

/// Custom hash format for transactions
mod hash;
pub use crate::transactions::hash::{TransactionHash, TransactionHasher};

mod utxo_processor;
pub use crate::transactions::utxo_processor::UtxoProcessor;

/// high-level abstractions for k256-based signatures of transactions
mod signature;
pub use crate::transactions::signature::StryiSignature;

/// FeePolicy and FeeCalculator for estimating the required fee for any transaction
mod fee_policy;

/// Provides logic for estimating and computing the actual size of transactions,  
/// including inputs, outputs, and signatures, to ensure accurate fee calculation.
mod size;
pub use crate::transactions::size::*;

pub use fee_policy::*;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::address::AccountAddress;
use crate::error::StryiCoreError;
use crate::hash::HashKind;
use crate::transactions::TransactionKind::{Coinbase, Genesis};
use bincode::{self, config::standard};
use k256::ecdsa::{SigningKey, VerifyingKey, signature::hazmat::PrehashVerifier};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Hash)]
#[repr(u8)]
pub enum TransactionKind {
    /// Coinbase is a type of transaction that is used to reward the miner of the last block.
    /// Coinbase transactions are always the very first in each block except for the Genesis block.
    /// It also forbids any inputs in the transaction and must contain exactly one output.
    Coinbase,

    /// Genesis transaction is a unique transaction that happens only in the Genesis Block.
    /// It is used to define initial account balances.
    Genesis,

    /// Payment transaction represents a standard token transfer
    Payment,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, Hash, PartialEq)]
pub struct TransactionData {
    /// Version byte for future transaction format changes.
    pub version: u16,
    /// Processing rules for this transaction.
    pub kind: TransactionKind,
    /// Inputs spending previous outputs.
    pub inputs: Vec<TransactionIn>,
    /// New outputs created by this transaction.
    pub outputs: Vec<TransactionOut>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, Hash, PartialEq)]
pub struct Transaction {
    /// Unsigned transaction payload.
    pub data: TransactionData,

    /// Signature.
    /// The first byte stores the recovery id so the signature stays recoverable.
    pub(crate) signature: StryiSignature,
}

impl TransactionData {
    /// We hash only unsigned data so signing and tx identity use the same preimage.
    pub fn hash(&self) -> TransactionHash {
        let encoded = bincode::serde::encode_to_vec(self, standard())
            .expect("Failed to serialize TransactionData for hashing");

        TransactionHash::new(&encoded)
    }

    pub fn sign(self, signing_key: &SigningKey) -> Transaction {
        let msg_bytes: [u8; TransactionHasher::SIZE] = self.hash().data;

        let (signature, recid) = signing_key
            .sign_prehash_recoverable(&msg_bytes)
            .expect("ECDSA signing failed for TransactionData");

        // Keep the recovery id beside the signature bytes.
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
    pub fn signature(&self) -> StryiSignature {
        self.signature.to_owned()
    }

    /// Genesis and coinbase are carried unsigned, so signature checks skip them.
    /// For payment-txs it may return (`StryiCoreError::InvalidSignature`)
    pub fn verify_signature(&self, verifying_key: &VerifyingKey) -> Result<(), StryiCoreError> {
        if self.data.kind == Genesis || self.data.kind == Coinbase {
            return Ok(());
        }

        let msg_bytes = self.data.hash().data;

        let (_recovery_id, signature) = self.signature.extract_signature_parts()?;

        verifying_key
            .verify_prehash(&msg_bytes, &signature)
            .map_err(|e| StryiCoreError::InvalidSignature {
                msg: format!("Signature verification failed: {e}"),
            })
    }

    pub fn recover_public_key(&self) -> Result<VerifyingKey, StryiCoreError> {
        let msg_bytes = self.data.hash().data;
        let (recovery_id, signature) = self.signature.extract_signature_parts()?;

        VerifyingKey::recover_from_prehash(&msg_bytes, &signature, recovery_id).map_err(|e| {
            StryiCoreError::InvalidSignature {
                msg: format!("Cannot recover key from signature: {e:?}"),
            }
        })
    }

    /// Creates an unsigned transaction with an empty signature.
    /// Intended for genesis or coinbase transaction kinds.
    pub fn new_unsigned(data: TransactionData) -> Self {
        Transaction {
            data,
            signature: StryiSignature::default(),
        }
    }

    #[cfg(test)]
    fn verify_transaction_author(&self, account_address: AccountAddress) -> bool {
        let recovered_key = self.recover_public_key();

        if recovered_key.is_err() {
            return false;
        }

        let recovered_account_address =
            AccountAddress::new(&recovered_key.unwrap().to_sec1_bytes());

        recovered_account_address == account_address
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::AccountAddress;
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::rand_core::OsRng;

    fn create_dummy_transaction_data() -> TransactionData {
        TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![TransactionIn {
                previous_output: OutPoint {
                    txid: TransactionHash::new(&[20u8; 32]),
                    vout: 15,
                },
                sequence: 2,
            }],
            outputs: vec![TransactionOut {
                value: 55_555,
                recipient: AccountAddress::new(&[20u8; 32]),
            }],
        }
    }

    #[test]
    fn test_sign_and_verify_transaction_author() {
        let signing_key = SigningKey::random(&mut OsRng);
        let verify_key = signing_key.verifying_key();

        let account_address = AccountAddress::new(&verify_key.to_sec1_bytes());

        let tx_data = create_dummy_transaction_data();
        let transaction = tx_data.sign(&signing_key);

        let is_verified = transaction.verify_transaction_author(account_address);
        assert!(
            is_verified,
            "signature should recover to the signer address"
        );
    }

    #[test]
    fn test_verify_transaction_author_wrong_key() {
        let signing_key_sender = SigningKey::random(&mut OsRng);
        let verify_key_sender = signing_key_sender.verifying_key();

        let signing_key_other = SigningKey::random(&mut OsRng);
        let verify_key_other = signing_key_other.verifying_key();

        let account_address_sender = AccountAddress::new(&verify_key_sender.to_sec1_bytes());
        let account_address_other = AccountAddress::new(&verify_key_other.to_sec1_bytes());

        let tx_data = create_dummy_transaction_data();
        let transaction = tx_data.sign(&signing_key_sender);

        assert!(
            transaction.verify_transaction_author(account_address_sender),
            "correct address should match the recovered signer"
        );

        assert!(
            !transaction.verify_transaction_author(account_address_other),
            "wrong address must not match the recovered signer"
        );
    }

    #[test]
    fn test_recover_and_compare_public_key() {
        let signing_key = SigningKey::random(&mut OsRng);
        let verify_key = signing_key.verifying_key();

        let tx_data = create_dummy_transaction_data();
        let transaction = tx_data.sign(&signing_key);

        let recovered_key_result = transaction.recover_public_key();
        assert!(
            recovered_key_result.is_ok(),
            "public key recovery should succeed"
        );

        let recovered_key = recovered_key_result.unwrap();

        assert_eq!(
            recovered_key.to_sec1_bytes(),
            verify_key.to_sec1_bytes(),
            "recovered key should match the signer"
        );
    }

    #[test]
    fn test_verify_signature() {
        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        let another_signing_key = SigningKey::random(&mut OsRng);
        let another_verifying_key = another_signing_key.verifying_key();

        let tx_data = create_dummy_transaction_data();
        let transaction = tx_data.sign(&signing_key);

        assert!(
            transaction.verify_signature(verifying_key).is_ok(),
            "matching key should verify the signature"
        );

        assert!(
            transaction.verify_signature(another_verifying_key).is_err(),
            "different key must fail signature verification"
        );
    }
}
