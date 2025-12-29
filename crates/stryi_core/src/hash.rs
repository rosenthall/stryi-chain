use crate::error::StryiCoreError;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use std::convert::TryFrom;

// --- Sealed helper to avoid recursive "well-formed" obligations that cause E0275 ---
// Implementing a blanket trait for arrays prevents the compiler from re-entering
// the same obligation while still guaranteeing arrays of any const size are valid.
mod sealed {
    pub trait ValidSize {}
    impl<const N: usize> ValidSize for [u8; N] {}
}

/// Trait defines how a specific object in the blockchain should be hashed.
pub trait HashKind: Default {
    /// Size of the hash in bytes.
    const SIZE: usize;
    /// Prefix used for this type of hash.
    /// # Note
    ///
    /// The `PREFIX` should have a maximum length of 4 characters.
    const PREFIX: &'static str;

    /// Hashes the input bytes using blake3 and returns a fixed-size byte array.
    fn hash(public_key: &[u8]) -> [u8; Self::SIZE] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(public_key);

        let mut data = [0u8; Self::SIZE];
        hasher.finalize_xof().fill(&mut data);

        data
    }
}

/// Generic `Hash` struct parameterized by a `HashKind`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hash<K: HashKind>
where
    [u8; K::SIZE]: sealed::ValidSize,
{
    /// The type of hash, defining the hashing algorithm and prefix.
    pub(crate) kind: K,
    /// The hash data as a fixed-size byte array.
    pub data: [u8; K::SIZE],
}

impl<K: HashKind> Hash<K>
where
    [u8; K::SIZE]: sealed::ValidSize,
{
    /// Creates a new `Hash` by hashing the provided input bytes.
    pub fn new(input: &[u8]) -> Self {
        let data = K::hash(input);
        Self {
            kind: K::default(),
            data,
        }
    }

    /// Creates a `Hash` from **already-hashed** bytes (a final digest).
    fn from_digest(digest: [u8; K::SIZE]) -> Self {
        Self {
            kind: K::default(),
            data: digest,
        }
    }

    /// Parses a hash string into a `Hash`.
    /// Can return an error if the string has an incorrect prefix or invalid hex.
    pub fn from_hash_string(s: &str) -> Result<Self, StryiCoreError> {
        if !s.starts_with(K::PREFIX) {
            return Err(StryiCoreError::InvalidPrefix {
                expected: K::PREFIX.to_string(),
                actual: s.chars().take(K::PREFIX.len()).collect(),
            });
        }

        // Strip off the prefix and decode the hex
        let hex_part = &s[K::PREFIX.len()..];
        let bytes = hex::decode(hex_part).map_err(StryiCoreError::InvalidHex)?;

        // This calls `TryFrom<&[u8]> for Hash<K>`, which uses `from_digest(...)`
        Self::try_from(bytes.as_slice())
    }
}

impl<K: HashKind> TryFrom<&[u8]> for Hash<K>
where
    [u8; K::SIZE]: sealed::ValidSize,
{
    type Error = StryiCoreError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() != K::SIZE {
            return Err(StryiCoreError::InvalidLength {
                expected: K::SIZE,
                actual: value.len(),
            });
        }
        let mut array = [0u8; K::SIZE];
        array.copy_from_slice(value);

        // Use `from_digest` here to avoid re-hashing these final bytes
        Ok(Self::from_digest(array))
    }
}

impl<K: HashKind> TryFrom<Vec<u8>> for Hash<K>
where
    [u8; K::SIZE]: sealed::ValidSize,
{
    type Error = StryiCoreError;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Self::try_from(value.as_slice())
    }
}

impl<K: HashKind> fmt::Display for Hash<K>
where
    [u8; K::SIZE]: sealed::ValidSize,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Hex-encode and prepend the prefix
        let hex_data = hex::encode(self.data);
        let s = format!("{}{}", K::PREFIX, hex_data);
        write!(f, "{}", s)
    }
}

/// Custom Serialize implementations for `Hash<K>`.
/// We are serializing hash value as a string
impl<K: HashKind> Serialize for Hash<K>
where
    [u8; K::SIZE]: sealed::ValidSize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Custom Deserialize implementations for `Hash<K>`.
impl<'de, K: HashKind> Deserialize<'de> for Hash<K>
where
    [u8; K::SIZE]: sealed::ValidSize,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HashVisitor<K: HashKind>(std::marker::PhantomData<K>);

        impl<'de, K: HashKind> Visitor<'de> for HashVisitor<K>
        where
            [u8; K::SIZE]: sealed::ValidSize,
        {
            type Value = Hash<K>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(&format!(
                    "a string starting with '{}' followed by {} hexadecimal characters",
                    K::PREFIX,
                    K::SIZE * 2 // Each byte is represented as 2 hex characters
                ))
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Hash::<K>::from_hash_string(v).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_str(HashVisitor::<K>(std::marker::PhantomData))
    }
}

// implementation of Debug for Hash<K>, so it prints not the raw bytes but the debug string representation
// which looks like "StryiHash({HashKindName}){hash_string}"
impl<K: HashKind> fmt::Debug for Hash<K>
where
    [u8; K::SIZE]: sealed::ValidSize,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // get kind name e.g. stryi_core::address::AddressHasher
        let hash_kind_name = std::any::type_name::<K>();

        // only use last part after '::' via rsplit_once
        // now it will be just 'AddressHasher'
        let kind_name = hash_kind_name
            .rsplit_once("::")
            .map(|(_left_tail, name)| name)
            .unwrap_or(hash_kind_name);

        // get rid of "Hasher" or "HashKind" suffix if present
        let kind_name = kind_name
            .strip_suffix("Hasher")
            .or_else(|| kind_name.strip_suffix("HashKind"))
            .unwrap_or(kind_name);

        write!(f, "StryiHash({}, {})", kind_name, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::TryFrom;

    /// A mock HashKind for testing purposes.
    /// Prefix: "TEST", Size: 16 bytes (128 bits)
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestHasher;

    impl Default for TestHasher {
        fn default() -> Self {
            TestHasher
        }
    }

    impl HashKind for TestHasher {
        const SIZE: usize = 16; // 16 bytes = 128 bits
        const PREFIX: &'static str = "TEST";

        /// A simple hash function for testing that fills the array with the first 16 bytes of the input.
        /// If the input is shorter than 16 bytes, it pads the rest with zeros.
        fn hash(input: &[u8]) -> [u8; Self::SIZE] {
            let mut hash = [0u8; Self::SIZE];
            let len = input.len().min(Self::SIZE);
            hash[..len].copy_from_slice(&input[..len]);
            hash
        }
    }

    type TestHash = Hash<TestHasher>;

    /// Helper function to generate a vector of bytes of a specific length.
    fn generate_bytes(len: usize) -> Vec<u8> {
        (0..len).map(|i| i as u8).collect()
    }

    #[test]
    fn test_create_hash_with_valid_input() {
        let input = b"test input data";
        let hash = TestHash::new(input);
        let expected_data = {
            let mut data = [0u8; 16];
            let len = input.len().min(16);
            data[..len].copy_from_slice(&input[..len]);
            data
        };
        assert_eq!(hash.data, expected_data);
        assert_eq!(hash.kind, TestHasher);
    }

    #[test]
    fn test_display_hash() {
        let input = b"test input data";
        let hash = Hash::<TestHasher>::new(input);
        let hex_data = hex::encode(hash.data);
        let expected_string = format!("{}{}", TestHasher::PREFIX, hex_data);
        assert_eq!(hash.to_string(), expected_string);
        assert_eq!(format!("{}", hash), expected_string);
    }

    #[test]
    fn test_from_hash_string_with_valid_input() {
        let input = b"valid input";
        let hash = Hash::<TestHasher>::new(input);
        let hash_string = hash.to_string();

        let parsed_hash = Hash::<TestHasher>::from_hash_string(&hash_string);
        assert!(parsed_hash.is_ok());
        let parsed = parsed_hash.unwrap();
        assert_eq!(parsed, hash);
    }

    #[test]
    fn test_from_hash_string_with_invalid_prefix() {
        let invalid_prefix = "WRON";
        let hex_part = "000102030405060708090a0b0c0d0e0f";
        let hash_string = format!("{}{}", invalid_prefix, hex_part);

        let parsed_hash = Hash::<TestHasher>::from_hash_string(&hash_string);
        assert!(parsed_hash.is_err());

        match parsed_hash {
            Err(StryiCoreError::InvalidPrefix { expected, actual }) => {
                assert_eq!(expected, TestHasher::PREFIX.to_string());
                assert_eq!(actual, "WRON".to_string());
            }
            _ => panic!("Expected InvalidPrefix error."),
        }
    }

    #[test]
    fn test_from_hash_string_with_invalid_hex() {
        let invalid_hex = "TESTGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG"; // G is not a hex value
        let parsed_hash = Hash::<TestHasher>::from_hash_string(invalid_hex);
        assert!(parsed_hash.is_err());

        match parsed_hash {
            Err(StryiCoreError::InvalidHex(_)) => {}
            _ => panic!("Expected InvalidHex error."),
        }
    }

    #[test]
    fn test_from_hash_string_with_invalid_length() {
        let prefix = TestHasher::PREFIX;
        let hex_part = "00010203"; // Only 8 characters instead of 32 for 16 bytes
        let hash_string = format!("{}{}", prefix, hex_part);

        let parsed_hash = Hash::<TestHasher>::from_hash_string(&hash_string);
        assert!(parsed_hash.is_err());

        match parsed_hash {
            Err(StryiCoreError::InvalidLength { expected, actual }) => {
                assert_eq!(expected, TestHasher::SIZE);
                let actual_bytes = hex::decode(hex_part).unwrap().len();
                assert_eq!(actual, actual_bytes);
            }
            _ => panic!("Expected InvalidLength error."),
        }
    }

    #[test]
    fn test_try_from_slice_with_valid_length() {
        let bytes = generate_bytes(16); // Exact length
        let hash = Hash::<TestHasher>::try_from(bytes.as_slice());
        assert!(hash.is_ok());

        let expected_hash = Hash::<TestHasher>::new(&bytes);
        assert_eq!(hash.unwrap(), expected_hash);
    }

    #[test]
    fn test_try_from_slice_with_invalid_length() {
        let bytes = generate_bytes(10); // Less than required
        let hash = Hash::<TestHasher>::try_from(bytes.as_slice());
        assert!(hash.is_err());

        match hash {
            Err(StryiCoreError::InvalidLength { expected, actual }) => {
                assert_eq!(expected, TestHasher::SIZE);
                assert_eq!(actual, 10);
            }
            _ => panic!("Expected InvalidLength error."),
        }
    }

    #[test]
    fn test_try_from_vec_with_valid_length() {
        let bytes = generate_bytes(16);

        let hash = Hash::<TestHasher>::try_from(bytes.clone());
        assert!(hash.is_ok());

        let expected_hash = Hash::<TestHasher>::new(&bytes);
        assert_eq!(hash.unwrap(), expected_hash);
    }

    #[test]
    fn test_try_from_vec_with_invalid_length() {
        let bytes = generate_bytes(20); // More than required
        let hash = Hash::<TestHasher>::try_from(bytes);
        assert!(hash.is_err());

        match hash {
            Err(StryiCoreError::InvalidLength { expected, actual }) => {
                assert_eq!(expected, TestHasher::SIZE);
                assert_eq!(actual, 20);
            }
            _ => panic!("Expected InvalidLength error."),
        }
    }
    #[test]
    fn test_hash_serialization() {
        let data = b"Serialize this data.";
        let custom_hash = TestHash::new(data);

        // Serialize to JSON
        let serialized = serde_json::to_string(&custom_hash).expect("Serialization failed");
        let expected = format!("\"{}\"", custom_hash);
        assert_eq!(serialized, expected);

        // Deserialize back
        let deserialized: TestHash =
            serde_json::from_str(&serialized).expect("Deserialization failed");
        assert_eq!(custom_hash, deserialized);
    }

    #[test]
    fn test_hash_deserialization_invalid_data() {
        // Missing prefix
        let json_str = "\"abcdef123456\"";
        let result: Result<TestHash, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
        // Invalid hex
        let json_str = format!("\"{}ZZZZ\"", TestHasher::PREFIX);
        let result: Result<TestHash, _> = serde_json::from_str(&json_str);
        assert!(result.is_err());

        // Incorrect length
        let hex_part = "a3f1"; // Too short
        let json_str = format!("\"{}{}\"", TestHasher::PREFIX, hex_part);
        let result: Result<TestHash, _> = serde_json::from_str(&json_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_hash_debug_format() {
        let bytes = generate_bytes(16);

        let hash = Hash::<TestHasher>::try_from(bytes.clone()).unwrap();

        let debug_str = format!("{:?}", hash);

        // Ensure it contains the kind name without "Hasher" suffix
        assert!(!debug_str.contains("Hasher"));

        // Full expected debug string
        assert_eq!(debug_str, format!("StryiHash({}, {})", "Test", hash));
    }
}
