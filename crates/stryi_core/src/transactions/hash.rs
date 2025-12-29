use crate::hash::{Hash, HashKind};

/// A specific hash kind for transactions.
//  We derive `Hash` to let it be used as a key in HashMaps if needed.
#[derive(Default, Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub struct TransactionHasher;

impl HashKind for TransactionHasher {
    /// We use a 32-byte (256-bit) hash for transactions.
    const SIZE: usize = 32;

    /// Prefix used in string form, e.g. "Tx...hex..."
    const PREFIX: &'static str = "Tx";

    // NOTE: using default hash() implementation, based on blake3
}

/// A typed alias for `Hash<TransactionHasher>`.
/// Helps distinguish transaction hashes from other hash types.
pub type TransactionHash = Hash<TransactionHasher>;
