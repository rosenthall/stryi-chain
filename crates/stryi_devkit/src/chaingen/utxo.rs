use stryi_core::transactions::OutPoint;

/// Possible option of "how to choose available UTXO to spend in this transaction"
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum UtxoSelectionCriteria {
    Oldest,
    Newest,
    Largest,
    Smallest,
}

/// Enhanced UTXO tracking with metadata
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UtxoInfo {
    pub outpoint: OutPoint,
    pub value: u64,
    pub height_created: u64,
    pub is_coinbase: bool,
}

/// Possible options of transaction patterns that can be generated.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum TransactionPattern {
    /// Simple 1-input, 1-output
    Simple,
    /// Consolidate multiple small UTXOs into one
    Consolidation,
    /// Split one large UTXO into multiple smaller ones
    Splitting,
    /// Multiple inputs, multiple outputs (complex)
    ///
    /// *NOTE*: Inputs amount is the same as outputs.
    /// So if it burns 4 txins, it must create 4 new txouts to keep balance.
    Complex,
}
