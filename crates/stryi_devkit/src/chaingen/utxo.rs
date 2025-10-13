use rand::Rng;
use rand::distr::StandardUniform;
use rand::distr::weighted::WeightedIndex;
use rand::prelude::Distribution;
use std::sync::LazyLock;
use stryi_core::transactions::{FeePolicy, OutPoint};

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
    /// *NOTE*: Inputs amount are the same as outputs.
    /// So if it burns 4 txins, it must create 4 new txouts in order to keep balance.
    Complex,
}

impl Distribution<UtxoSelectionCriteria> for StandardUniform {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> UtxoSelectionCriteria {
        match rng.random_range(0..=3) {
            0 => UtxoSelectionCriteria::Oldest,
            1 => UtxoSelectionCriteria::Newest,
            2 => UtxoSelectionCriteria::Largest,
            _ => UtxoSelectionCriteria::Smallest,
        }
    }
}

/// Weights for distribution of transaction patterns.
/// - 50% of transactions are `Simple`
/// - 20% of transactions are `Consolidations`
/// - 20% of transactions are `Splitting`
/// - 10% of transactions are `Complex`
static PATTERN_WEIGHTED_INDEX: LazyLock<WeightedIndex<u32>> = LazyLock::new(|| {
    let weights: [u32; 4] = [50, 20, 20, 10];
    WeightedIndex::new(weights).expect("weights must be non-empty and positive")
});

/// Sampler for getting random TransactionPattern, uses hardcoded weights. See `PATTERN_WEIGHTED_INDEX`
pub(crate) fn sample_transaction_pattern<R: Rng + ?Sized>(rng: &mut R) -> TransactionPattern {
    match PATTERN_WEIGHTED_INDEX.sample(rng) {
        0 => TransactionPattern::Simple,
        1 => TransactionPattern::Consolidation,
        2 => TransactionPattern::Splitting,
        _ => TransactionPattern::Complex,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_chacha::ChaCha8Rng;
    use rand_chacha::rand_core::SeedableRng;
    use std::collections::HashSet;

    #[test]
    fn test_deterministic_distribution() {
        let seed = 42u64;

        // Deterministic RNGs
        let mut rng1 = ChaCha8Rng::seed_from_u64(seed);
        let mut rng2 = ChaCha8Rng::seed_from_u64(seed);
        let mut rng3 = ChaCha8Rng::seed_from_u64(seed + 1);

        // RNG equality/inequality
        assert_eq!(
            rng1.clone(),
            rng2.clone(),
            "Same seed should produce identical RNG state"
        );
        assert_ne!(
            rng1, rng3,
            "Different seeds should produce different RNG state"
        );

        // Sample size large enough to cover weighted options
        const SAMPLES: usize = 1000;

        // === UtxoSelectionCriteria deterministic test ===
        let criteria1: Vec<_> = (0..SAMPLES)
            .map(|_| StandardUniform.sample(&mut rng1))
            .collect();
        let criteria2: Vec<_> = (0..SAMPLES)
            .map(|_| StandardUniform.sample(&mut rng2))
            .collect();
        let criteria3: Vec<_> = (0..SAMPLES)
            .map(|_| StandardUniform.sample(&mut rng3))
            .collect();

        assert_eq!(
            criteria1, criteria2,
            "Same seed should produce identical sequences"
        );
        assert_ne!(
            criteria1, criteria3,
            "Different seeds should produce different sequences"
        );

        let unique_criteria: HashSet<UtxoSelectionCriteria> = criteria1.iter().cloned().collect();
        assert_eq!(
            unique_criteria.len(),
            4,
            "Should generate all UTXO selection criteria"
        );

        // === TransactionPattern weighted test ===
        let patterns1: Vec<_> = (0..SAMPLES)
            .map(|_| sample_transaction_pattern(&mut rng1))
            .collect();
        let patterns2: Vec<_> = (0..SAMPLES)
            .map(|_| sample_transaction_pattern(&mut rng2))
            .collect();
        let patterns3: Vec<_> = (0..SAMPLES)
            .map(|_| sample_transaction_pattern(&mut rng3))
            .collect();

        assert_eq!(
            patterns1, patterns2,
            "Same seed RNGs should remain in sync and produce identical pattern sequences"
        );
        assert_ne!(
            patterns1, patterns3,
            "Different seed RNGs should produce different pattern sequences"
        );

        let unique_patterns: HashSet<TransactionPattern> = patterns1.iter().copied().collect();
        assert_eq!(
            unique_patterns.len(),
            4,
            "Should generate all transaction patterns"
        );

        // test approximate weight ratios
        let counts = patterns1.iter().fold([0usize; 4], |mut acc, p| {
            match p {
                TransactionPattern::Simple => acc[0] += 1,
                TransactionPattern::Consolidation => acc[1] += 1,
                TransactionPattern::Splitting => acc[2] += 1,
                TransactionPattern::Complex => acc[3] += 1,
            }
            acc
        });

        println!("TransactionPattern counts: {:?}", counts);

        // check that distribution is expected with 5% adjustment
        assert!(
            (counts[0] as f64 - (SAMPLES as f64 * 0.5)).abs() < SAMPLES as f64 * 0.05,
            "Simple pattern count should be ~50%"
        );
        assert!(
            (counts[1] as f64 - (SAMPLES as f64 * 0.2)).abs() < SAMPLES as f64 * 0.05,
            "Consolidation pattern count should be ~20%"
        );
        assert!(
            (counts[2] as f64 - (SAMPLES as f64 * 0.2)).abs() < SAMPLES as f64 * 0.05,
            "Splitting pattern count should be ~20%"
        );
        assert!(
            (counts[3] as f64 - (SAMPLES as f64 * 0.1)).abs() < SAMPLES as f64 * 0.05,
            "Complex pattern count should be ~10%"
        );
    }
}
