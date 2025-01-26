use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde::de::{Error as DeError, Visitor};
use std::fmt;
use k256::ecdsa::{RecoveryId, Signature};
use crate::error::StryiCoreError;

#[derive(Debug, Clone, Eq, PartialEq)]
/// Represents 65 bytes recoverable signature, implements serde's traits so can be easily serialized and deserialized
pub struct StryiSignature(pub Box<[u8; 65]>);


impl Default for StryiSignature {
    fn default() -> Self {
        // Default value is nulls.
        Self(Box::new([0u8; 65]))
    }
}


impl StryiSignature {
    // Try parse a 65-byte representation of signature:
    // - 1 byte for recovery ID
    // - 64 bytes for (r, s)
    // Returns Ok((RecoveryId, Signature)) if no error happened.
    pub(crate) fn extract_signature_parts(&self) -> Result<(RecoveryId, Signature), StryiCoreError> {
        
        // 1) Try restore recovery id from first byte
        let recovery_id = RecoveryId::try_from(self.0[0]).map_err(|_| {
            StryiCoreError::InvalidSignature {
                msg: "Cannot restore Recovery Id from signature.".to_string(),
            }
        })?;

        // 2) construct Signature object from rest of StryiSignature
        let sig_bytes = &self.0[1..];
        let signature = Signature::from_slice(sig_bytes).map_err(|_| {
            StryiCoreError::InvalidSignature {
                msg: "Failed to construct Signature object from bytes.".to_string(),
            }
        })?;

        Ok((recovery_id, signature))
    }
}



impl Serialize for StryiSignature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Serialize the inner array as bytes
        serializer.serialize_bytes(self.0.as_slice())
    }
}

struct StryiSignatureVisitor;

impl<'de> Visitor<'de> for StryiSignatureVisitor {
    type Value = StryiSignature;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a 65-byte signature")
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        if v.len() != 65 {
            return Err(DeError::invalid_length(v.len(), &self));
        }

        // Convert slice to a fixed-size array
        let mut arr = [0u8; 65];
        arr.copy_from_slice(v);
        Ok(StryiSignature(Box::new(arr)))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut arr = [0u8; 65];
        for i in 0..65 {
            arr[i] = match seq.next_element()? {
                Some(val) => val,
                None => return Err(DeError::invalid_length(i, &self)),
            };
        }
        Ok(StryiSignature(Box::new(arr)))
    }
}

impl<'de> Deserialize<'de> for StryiSignature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_bytes(StryiSignatureVisitor)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn serialize_deserialize_signature() {
        // Create a StryiSignature with dummy data (65 bytes of value 42)
        let original_signature = StryiSignature(Box::new([42u8; 65]));

        // Serialize the signature to a JSON string
        let serialized = serde_json::to_string(&original_signature)
            .expect("Serialization should succeed");

        // Deserialize the JSON string back into a StryiSignature
        let deserialized_signature: StryiSignature = serde_json::from_str(&serialized)
            .expect("Deserialization should succeed");

        // Assert that the original and deserialized signatures are identical
        assert_eq!(original_signature, deserialized_signature);
    }
}
