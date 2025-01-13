mod utxo;
mod hash;

use serde::{Deserialize, Serialize};

use p256::ecdsa::{
    signature::{Signer, Verifier},
    Signature, SigningKey, VerifyingKey,
};
use bincode::{self, config::standard};

pub use crate::transactions::hash::{TransactionHash};
pub use crate::transactions::utxo::{TransactionIn, TransactionOut, OutPoint, UTXO};


/// `TransactionData` holds the *unsigned* transaction fields: version, inputs, outputs.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TransactionData {
    /// Transaction version
    pub version: u32,
    /// Transaction inputs
    pub inputs: Vec<TransactionIn>,
    /// Transaction outputs
    pub outputs: Vec<TransactionOut>,
}

/// `Transaction` is the signed transaction: it wraps `TransactionData` plus an ECDSA signature.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Transaction {
    /// The actual transaction data
    pub data: TransactionData,
    /// DER-encoded ECDSA signature over the hash of `TransactionData`
    pub signature: Vec<u8>,
}

impl TransactionData {
    /// Computes the Blake3-based hash of `TransactionData`.
    /// We do *not* include a signature here, since it's unsigned data.
    pub fn hash(&self) -> TransactionHash {
        let encoded = bincode::serde::encode_to_vec(self, standard())
            .expect("Failed to serialize TransactionData for hashing");
        TransactionHash::new(&encoded)
    }

    /// Signs this `TransactionData` with the provided ECDSA `SigningKey`.
    /// Produces a fully signed `Transaction`.
    pub fn sign(self, signing_key: &SigningKey) -> Transaction {
        let message_hash = self.hash();
        // Sign the 32-byte data inside `message_hash`
        let signature: Signature = signing_key.sign(message_hash.data.as_ref());

        Transaction {
            data: self,
            signature: signature.to_der().as_bytes().to_vec(),
        }
    }
}

impl Transaction {
    /// Verifies this transaction's signature using the given `VerifyingKey`.
    /// We:
    ///  - Recompute the hash of the `data`
    ///  - Parse the stored DER-encoded signature
    ///  - Verify the signature with `verifying_key`
    pub fn verify(&self, verifying_key: &VerifyingKey) -> bool {
        let message_hash = self.data.hash();

        let parsed_sig = match Signature::from_der(&self.signature) {
            Ok(s) => s,
            Err(_) => return false,
        };
        verifying_key.verify(message_hash.data.as_ref(), &parsed_sig).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transactions::{OutPoint, TransactionIn, TransactionOut};
    use p256::ecdsa::{SigningKey, VerifyingKey};
    use p256::elliptic_curve::rand_core::OsRng;
    use crate::address::AccountAddress;

    #[test]
    fn test_two_struct_transaction() {
        // Example transaction data
        let tx_data = TransactionData {
            version: 1,
            inputs: vec![
                TransactionIn {
                    previous_output: OutPoint {
                        txid: TransactionHash::try_from([0u8; 32].as_slice()).unwrap(),
                        vout: 0,
                    },
                    signature: vec![],
                    sequence: 0xFFFFFFFF,
                }
            ],
            outputs: vec![
                TransactionOut {
                    value: 1000,
                    recipient: AccountAddress::new(b"Alice"),
                }
            ],
        };

        // Generate random ephemeral signing key
        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = VerifyingKey::from(&signing_key);

        // Sign the data
        let signed_tx = tx_data.sign(&signing_key);

        // Make sure verification works
        assert!(signed_tx.verify(&verifying_key));

        // Mutate the transaction's data to invalidate the signature
        let mut tampered = signed_tx.clone();
        tampered.data.outputs[0].value = 9999; // change amount
        assert!(!tampered.verify(&verifying_key));
    }
}
