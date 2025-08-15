use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde::de::{Error as DeError, Visitor};
use std::fmt;
use k256::ecdsa::{RecoveryId, Signature};
use crate::error::StryiCoreError;
use base64::{engine::general_purpose::STANDARD, Engine as _};

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
/// Represents 65 bytes recoverable signature, implements serde's traits so can be easily serialized and deserialized
pub struct StryiSignature(pub Box<[u8; 65]>);

impl Default for StryiSignature {
    fn default() -> Self {
        Self(Box::new([0u8; 65]))
    }
}

impl StryiSignature {
    /// Parse a 65-byte representation of signature:
    /// - 1 byte for recovery ID
    /// - 64 bytes for (r, s)
    pub(crate) fn extract_signature_parts(&self) -> Result<(RecoveryId, Signature), StryiCoreError> {
        let recovery_id = RecoveryId::try_from(self.0[0]).map_err(|_| {
            StryiCoreError::InvalidSignature {
                msg: "Cannot restore Recovery Id from signature.".to_string(),
            }
        })?;

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
        let encoded = STANDARD.encode(self.0.as_slice());
        serializer.serialize_str(&encoded)
    }
}

struct StryiSignatureVisitor;

impl<'de> Visitor<'de> for StryiSignatureVisitor {
    type Value = StryiSignature;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a base64-encoded 65-byte signature string")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: DeError,
    {
        let decoded = STANDARD
            .decode(v)
            .map_err(|_| DeError::custom("Invalid base64 for StryiSignature"))?;

        if decoded.len() != 65 {
            return Err(DeError::invalid_length(decoded.len(), &self));
        }

        let mut arr = [0u8; 65];
        arr.copy_from_slice(&decoded);
        Ok(StryiSignature(Box::new(arr)))
    }
}

impl<'de> Deserialize<'de> for StryiSignature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(StryiSignatureVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn serialize_deserialize_signature() {
        let original_signature = StryiSignature(Box::new([42u8; 65]));

        let serialized = serde_json::to_string(&original_signature)
            .expect("Serialization should succeed");

        println!("Serialized: {}", serialized);
        assert!(serialized.starts_with("\"") && serialized.ends_with("\""));

        let deserialized: StryiSignature = serde_json::from_str(&serialized)
            .expect("Deserialization should succeed");

        assert_eq!(original_signature, deserialized);
    }
}
