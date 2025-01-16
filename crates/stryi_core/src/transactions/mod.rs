mod utxo;
mod hash;
mod utxo_processor;

use serde::{Deserialize, Serialize};

use bincode::{self, config::standard};
use secp256k1::{All, Secp256k1, SecretKey, PublicKey};
use secp256k1::ecdsa::Signature as SecpSignature;
use crate::hash::HashKind;
pub use crate::transactions::hash::{TransactionHash};
use crate::transactions::hash::TransactionHasher;
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
    /// DER-encoded secp256k1 ECDSA signature over the hash of `TransactionData`
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

    /// Signs this `TransactionData` with secp256k1, producing a fully signed `Transaction`.
    /// This is already updated, but shown for completeness:
    pub fn sign(self, secp: &Secp256k1<All>, secret_key: &SecretKey) -> Transaction {
        // 1) Compute the 32-byte message hash from `TransactionData`
        let msg_bytes: [u8; TransactionHasher::SIZE] = self.hash().data;
        let message = secp256k1::Message::from_digest(msg_bytes);

        // 2) ECDSA sign with secp256k1
        let signature = secp.sign_ecdsa(&message, secret_key);

        // 3) Serialize the signature in DER format
        let signature_bytes = signature.serialize_der().to_vec();

        Transaction {
            data: self,
            signature: signature_bytes,
        }
    }
}

impl Transaction {
    /// Verifies this transaction's signature using secp256k1 ECDSA.
    /// Steps:
    ///  - Recompute the hash of the `data`
    ///  - Parse the stored DER-encoded signature
    ///  - Verify the signature with the provided secp256k1 public key
    pub fn verify(&self, secp: &Secp256k1<All>, public_key: &PublicKey) -> bool {
        let message_hash = self.data.hash();
        let msg_bytes = message_hash.data;

        // Parse the message from the 32-byte hash
        let message = match secp256k1::Message::from_slice(&msg_bytes) {
            Ok(m) => m,
            Err(_) => return false,
        };

        // Parse the DER-encoded signature
        let parsed_sig = match SecpSignature::from_der(&self.signature) {
            Ok(s) => s,
            Err(_) => return false,
        };

        // Verify ECDSA signature
        secp.verify_ecdsa(&message, &parsed_sig, public_key).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::{Secp256k1, rand::thread_rng};
    use crate::address::AccountAddress;
    use crate::transactions::{OutPoint, TransactionIn, TransactionOut, TransactionHash};

    #[test]
    fn test_basic_transaction_sign_and_verify() {
        // Prepare a dummy transaction
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

        // Set up secp256k1
        let secp = Secp256k1::new();
        let mut rng = thread_rng();

        // Generate ephemeral keypair
        let (secret_key, public_key) = secp.generate_keypair(&mut rng);

        // Sign the transaction data
        let signed_tx = tx_data.sign(&secp, &secret_key);

        // Make sure verification works
        assert!(signed_tx.verify(&secp, &public_key));

        // Mutate the transaction's data to invalidate the signature
        let mut tampered = signed_tx.clone();
        tampered.data.outputs[0].value = 9999; // change amount

        // Should fail verification now
        assert!(!tampered.verify(&secp, &public_key));
    }
}
