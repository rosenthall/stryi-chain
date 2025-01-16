mod utxo;
mod hash;
mod utxo_processor;

use serde::{Deserialize, Serialize};

use bincode::{self, config::standard};
use secp256k1::{
    All, Secp256k1, SecretKey, PublicKey, Message,
    ecdsa::{RecoverableSignature, RecoveryId, Signature as SecpSignature},
};
use crate::hash::HashKind;
pub use crate::transactions::hash::{TransactionHash};
use crate::transactions::hash::TransactionHasher;
pub use crate::transactions::utxo::{TransactionIn, TransactionOut, OutPoint, UTXO};

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
    ///
    /// Note: This signature is stored in "recoverable" format:
    /// - 1 byte for the recovery ID
    /// - 64 bytes for (r, s)
    ///
    /// This differs from the DER-encoded format.
    pub signature: Vec<u8>,
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
    pub fn sign(self, secp: &Secp256k1<All>, secret_key: &SecretKey) -> Transaction {
        // 1) Compute the 32-byte message hash from TransactionData
        let msg_bytes: [u8; TransactionHasher::SIZE] = self.hash().data;

        // Usually we do `Message::from_slice(&msg_bytes)` but your code uses `Message::from_digest`
        // which is a convenience constructor. We'll assume that's a function that also enforces
        // the 32-byte size. If not, replace with from_slice(&msg_bytes).unwrap().
        let message = secp256k1::Message::from_digest(msg_bytes);

        // 2) ECDSA sign with secp256k1, obtaining a recoverable signature
        let recoverable_sig = secp.sign_ecdsa_recoverable(&message, secret_key);

        // 3) Convert that to a 65-byte representation: 
        //    - 1 byte for recovery ID
        //    - 64 bytes for (r, s)
        let (recid, rs) = recoverable_sig.serialize_compact();
        let mut signature_bytes = [0u8; 65];
        signature_bytes[0] = recid as u8;  // recovery ID
        signature_bytes[1..].copy_from_slice(&rs[..]); // r, s

        Transaction {
            data: self,
            signature: signature_bytes.to_vec(),  // store the 65 bytes
        }
    }
}

impl Transaction {
    /// Verifies this transaction's signature in a **standard** ECDSA sense,
    /// expecting the signature to be in DER format. However, your stored signature
    /// is actually 65 bytes of recoverable data, so this `verify` method, as is,
    /// won't parse that format unless we do some conversion.
    ///
    /// Currently, the code below attempts to parse `self.signature` as DER.
    /// That will likely fail if `self.signature` is actually the 65-byte recoverable format.
    ///
    /// If you truly want a DER-based signature, you'd need to store that instead.
    /// If you want a recoverable signature, consider using `verify_recoverable(...)` instead.
    pub fn verify(&self, secp: &Secp256k1<All>, public_key: &PublicKey) -> bool {
        // 1) Recompute the message hash
        let message_hash = self.data.hash();
        let msg_bytes = message_hash.data;

        let message = Message::from_digest(msg_bytes);

        // 2) Attempt to parse self.signature as a DER-encoded signature
        let parsed_sig = match SecpSignature::from_der(&self.signature) {
            Ok(s) => s,
            // If it fails, it's probably because `self.signature` is not DER, 
            // but is actually the 65-byte recoverable format. 
            Err(_) => return false,
        };

        // 3) Standard ECDSA verify
        secp.verify_ecdsa(&message, &parsed_sig, public_key).is_ok()
    }

    /// Recovers the public key from the **recoverable** signature stored in `self.signature`.
    ///
    /// This is possible because we're storing the 65-byte format: 
    ///   [recovery ID (1 byte) | r, s (64 bytes)].
    /// If the signature is invalid or the format is wrong, returns `None`.
    pub fn recover_pubkey(&self, secp: &Secp256k1<All>) -> Option<PublicKey> {
        // 1) Recompute the message (32 bytes)
        let tx_hash_bytes: [u8; TransactionHasher::SIZE] = self.data.hash().data;
        let message = secp256k1::Message::from_digest(tx_hash_bytes);

        // 2) We expect a 65-byte recoverable signature
        if self.signature.len() != 65 {
            return None; // invalid length
        }

        // 3) Extract the 1-byte recovery ID + 64-byte (r, s)
        let recid_byte = self.signature[0];
        let recid = RecoveryId::try_from(recid_byte as i32).ok()?;

        let mut rs = [0u8; 64];
        rs.copy_from_slice(&self.signature[1..65]);

        // 4) Build a RecoverableSignature object
        let recoverable_sig = RecoverableSignature::from_compact(&rs, recid).ok()?;

        // 5) Attempt to recover the public key from that signature
        secp.recover_ecdsa(&message, &recoverable_sig).ok()
    }

    /// Verifies that this transaction's recoverable signature recovers to `expected_pubkey`.
    /// i.e., it checks that `recover_pubkey(...) == expected_pubkey`. 
    pub fn verify_recoverable(&self, secp: &Secp256k1<All>, pubkey: &PublicKey) -> bool {
        // 1) Recompute the 32-byte message hash (from TransactionData)
        let message_bytes = self.data.hash().data;

        // Convert the 32-byte array into a secp256k1 Message
        let message = Message::from_digest(message_bytes);

        // 2) We expect a 65-byte signature: [recovery_id_byte, r(32 bytes), s(32 bytes)]
        if self.signature.len() != 65 {
            return false;
        }

        // Extract the recovery ID from the first byte
        let recid_byte = self.signature[0];
        let recid = match RecoveryId::try_from(recid_byte as i32) {
            Ok(id) => id,
            Err(_) => return false,
        };

        // Extract the 64 bytes for (r, s)
        let mut rs = [0u8; 64];
        rs.copy_from_slice(&self.signature[1..65]);

        // Construct a RecoverableSignature from the compact bytes + recovery ID
        let recov_sig = match RecoverableSignature::from_compact(&rs, recid) {
            Ok(sig) => sig,
            Err(_) => return false,
        };

        // 3) Recover the public key from the signature
        let recovered_key = match secp.recover_ecdsa(&message, &recov_sig) {
            Ok(pk) => pk,
            Err(_) => return false,
        };

        // 4) Compare the recovered key to the expected key
        recovered_key == *pubkey
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::{Secp256k1, rand::thread_rng};
    use crate::{
        address::AccountAddress,
        transactions::{OutPoint, TransactionIn, TransactionOut, TransactionHash},
    };

    #[test]
    fn test_basic_transaction_sign_and_verify_recoverable() {
        // Prepare a dummy transaction with 1 input, 1 output.
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

        // Setup secp256k1
        let secp = Secp256k1::new();
        let mut rng = thread_rng();

        // Generate ephemeral keypair
        let (secret_key, public_key) = secp.generate_keypair(&mut rng);

        // Sign the transaction data (recoverable signature)
        let signed_tx = tx_data.sign(&secp, &secret_key);

        // 1) Test that we can recover the pubkey, and it matches the actual pubkey
        let recovered_pub = signed_tx.recover_pubkey(&secp)
            .expect("Failed to recover pubkey from transaction signature");
        assert_eq!(recovered_pub, public_key, "Recovered pubkey must match the signer's pubkey");

        // 2) Check `verify_recoverable`: ensures it recovers the same pubkey as `public_key`
        assert!(
            signed_tx.verify_recoverable(&secp, &public_key),
            "verify_recoverable should succeed"
        );

        // 3) Now let's mutate the transaction's data. This will break the signature
        let mut tampered = signed_tx.clone();
        tampered.data.outputs[0].value = 9999; // change an output amount

        // The recovered pubkey won't match anymore, so verify_recoverable should fail
        assert!(
            !tampered.verify_recoverable(&secp, &public_key),
            "Tampering should break the signature"
        );
    }

    #[test]
    fn test_transaction_verify_standard_der_fails_with_recoverable_sig() {
        // This test shows that if we store a **recoverable** signature (65 bytes),
        // then calling `verify()` (which expects DER) will fail by default.
        //
        // Because we haven't stored a DER signature, from_der(...) won't parse it properly.

        let secp = Secp256k1::new();
        let mut rng = thread_rng();
        let (secret_key, public_key) = secp.generate_keypair(&mut rng);

        let tx_data = TransactionData {
            version: 1,
            inputs: vec![],
            outputs: vec![],
        };

        let signed_tx = tx_data.sign(&secp, &secret_key);

        // Attempt standard verify, which tries to parse self.signature as DER
        let result = signed_tx.verify(&secp, &public_key);
        assert!(
            !result,
            "DER-based verify should fail if signature is the 65-byte recoverable format"
        );
    }

    #[test]
    fn test_transaction_recover_pubkey_wrong_size_signature() {
        // Construct a transaction that has a "wrong size" signature array
        // to ensure recover_pubkey returns None.
        let tx = Transaction {
            data: TransactionData {
                version: 1,
                inputs: vec![],
                outputs: vec![],
            },
            signature: vec![0u8; 64], // 64 bytes, missing the recovery ID
        };

        let secp = Secp256k1::new();
        let recovered = tx.recover_pubkey(&secp);
        assert!(recovered.is_none(), "Should not recover pubkey from a 64-byte signature");
    }
}
