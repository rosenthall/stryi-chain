use crate::hash::{Hash, HashKind};

#[derive(Default, Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub struct TransactionHasher;

impl HashKind for TransactionHasher {
    /// We use a 32-byte (256-bit) hash for transactions.
    const SIZE: usize = 32;

    /// Prefix used in string form, e.g. "Tx...hex..."
    const PREFIX: &'static str = "Tx";

    // NOTE: using default hash() implementation, based on blake3
}

pub type TransactionHash = Hash<TransactionHasher>;
