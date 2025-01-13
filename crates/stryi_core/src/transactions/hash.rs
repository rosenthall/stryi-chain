
use crate::hash::{Hash, HashKind};
use blake3;

/// A specific hash kind for transactions.
//  We derive `Hash` to let it be used as a key in HashMaps if needed.
#[derive(Default, Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub struct TransactionHasher;

impl HashKind for TransactionHasher {
    /// We use a 32-byte (256-bit) hash for transactions.
    const SIZE: usize = 32;

    /// Prefix used in string form, e.g. "Tx...hex..."
    const PREFIX: &'static str = "Tx";

    /// Hash function using Blake3.
    fn hash(data: &[u8]) -> [u8; Self::SIZE] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(data);

        let mut output = [0u8; Self::SIZE];
        hasher.finalize_xof().fill(&mut output);
        output
    }
}

/// A typed alias for `Hash<TransactionHasher>`.
/// Helps distinguish transaction hashes from other hash types.
pub type TransactionHash = Hash<TransactionHasher>;
