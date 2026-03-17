use crate::chaingen::txgen::TransactionGenerationParams;
use crate::chaingen::utxo::{TransactionPattern, UtxoSelectionCriteria};
use rand::Rng;

/// Adaptive transaction generation strategy that adjusts based on context
/// The idea is to make chaingen do sane choices for each transaction,
/// so each block will be "better"
#[derive(Debug)]
pub struct TxGenerationStrategy {
    /// How many transactions we've successfully generated so far
    pub successful_txs: usize,
    /// How many transactions we're targeting
    pub target_txs: usize,
    /// How many attempts we've made for the current transaction
    /// after each successful generation must be reset.
    pub attempts: usize,
    /// Maximum attempts allowed
    pub max_attempts: usize,
}

impl TxGenerationStrategy {
    /// Constructs new instance of TxGenerationStrategy.
    /// The max_attempts is calculated as `target_txs.saturating_mul(3).max(10)`
    pub fn new(target_txs: usize) -> Self {
        Self {
            successful_txs: 0,
            target_txs,
            attempts: 0,
            max_attempts: target_txs.saturating_mul(3).max(10),
        }
    }

    /// Record a successful transaction generation
    pub fn record_success(&mut self) {
        self.successful_txs += 1;

        // reset attempts on success to give more chances
        if self.successful_txs < self.target_txs {
            self.attempts = 0;
        }
    }

    /// Record a failed attempt
    pub fn record_attempt(&mut self) {
        self.attempts += 1;
    }

    /// Get progress as a ratio (0.0 to 1.0)
    pub fn progress(&self) -> f32 {
        self.successful_txs as f32 / self.target_txs.max(1) as f32
    }

    /// Check if we should continue trying to generate transactions
    /// Also performs some checks, may panic if invariants are violated
    pub fn should_continue(&self) -> bool {
        // Panic if invariants are violated
        assert!(
            self.attempts <= self.max_attempts,
            "attempts count shall never outreach the limit `max_attempts`"
        );
        assert!(
            self.successful_txs <= self.target_txs,
            "should never be generated more transactions than `target_txs`"
        );

        // if we still have attempts
        let still_have_attempts = self.attempts < self.max_attempts;

        // if we need more tx to generate
        let need_more_txs = self.successful_txs < self.target_txs;

        // continue when we still have attempts AND we need more txs
        still_have_attempts && need_more_txs
    }

    /// Choose transaction pattern adaptively based on current context
    ///
    /// Strategy:
    /// - Early phase (0-30%): Favor splitting to create more UTXOs for future txs
    /// - Middle phase (30-70%): Use random patterns with slight consolidation bias
    /// - Late phase (70-100%): Favor consolidation and simple txs to clean up UTXOs
    /// - Fallback mechanism: If repeated failures, progressively simplify patterns
    /// - Economic constraints: Consider max_inputs/outputs limits and fee economics
    pub fn choose_pattern(
        &self,
        available_utxos: usize,
        params: &TransactionGenerationParams,
        rng: &mut impl Rng,
    ) -> TransactionPattern {
        let progress = self.progress();

        // Calculate failure rate for current transaction attempt
        let failure_rate = if self.attempts > 0 {
            self.attempts as f32 / self.max_attempts as f32
        } else {
            0.0
        };

        // If we've failed multiple times on this tx, simplify the pattern
        // Use increasingly aggressive simplification as failures accumulate
        if failure_rate >= 0.5 {
            if available_utxos >= 2 {
                return TransactionPattern::Consolidation;
            }
            return TransactionPattern::Simple;
        } else if failure_rate >= 0.3 {
            if available_utxos >= 2 && rng.random_bool(0.8) {
                return TransactionPattern::Consolidation;
            }
            return TransactionPattern::Simple;
        }
        // If very few UTXOs available, we need to either split or consolidate
        if available_utxos < 2 {
            // Only Simple or Splitting are possible with 1 UTXO
            if rng.random_bool(0.7) {
                return TransactionPattern::Splitting;
            }
            return TransactionPattern::Simple;
        }

        // Check if we have enough UTXOs for complex patterns
        // Complex and Consolidation need at least 2 UTXOs, preferably more
        let can_consolidate = available_utxos >= 2;
        let can_complex = available_utxos >= 2;

        // Check if outputs limits allow for splitting
        // Splitting needs to create at least 2 outputs, up to max_outputs
        let can_split_effectively = params.max_outputs >= 2;

        // Check if consolidation makes economic sense
        // Need enough UTXOs to justify consolidation fee
        let should_favor_consolidation = available_utxos >= params.max_inputs / 2;

        // Phase-based pattern selection with economic constraints
        match progress {
            // Early phase (0-30%): Create UTXO diversity
            p if p < 0.3 => {
                let roll: f32 = rng.random();

                // Adjust probabilities based on constraints
                if !can_split_effectively {
                    // If can't split effectively, redistribute those chances
                    match roll {
                        r if r < 0.40 => TransactionPattern::Simple,
                        r if r < 0.70 => {
                            if can_complex {
                                TransactionPattern::Complex
                            } else {
                                TransactionPattern::Simple
                            }
                        }
                        _ => {
                            if can_consolidate {
                                TransactionPattern::Consolidation
                            } else {
                                TransactionPattern::Simple
                            }
                        }
                    }
                } else {
                    match roll {
                        r if r < 0.40 => TransactionPattern::Splitting, // 40%
                        r if r < 0.65 => TransactionPattern::Simple,    // 25%
                        r if r < 0.85 => {
                            if can_complex {
                                TransactionPattern::Complex
                            } else {
                                TransactionPattern::Simple
                            }
                        } // 20%
                        _ => {
                            if can_consolidate {
                                TransactionPattern::Consolidation
                            } else {
                                TransactionPattern::Simple
                            }
                        } // 15%
                    }
                }
            }

            // Middle phase (30-70%): Balanced mix with slight consolidation bias
            p if p < 0.7 => {
                let roll: f32 = rng.random();

                // Favor consolidation if we have many UTXOs
                let consolidation_boost = if should_favor_consolidation {
                    0.10
                } else {
                    0.0
                };

                match roll {
                    r if r < 0.30 => TransactionPattern::Simple, // 30%
                    r if r < (0.55 + consolidation_boost) => {
                        if can_consolidate {
                            TransactionPattern::Consolidation
                        } else {
                            TransactionPattern::Simple
                        }
                    }
                    r if r < (0.75 + consolidation_boost) => {
                        if can_split_effectively {
                            TransactionPattern::Splitting
                        } else {
                            TransactionPattern::Simple
                        }
                    }
                    _ => {
                        if can_complex {
                            TransactionPattern::Complex
                        } else {
                            TransactionPattern::Simple
                        }
                    }
                }
            }

            // Late phase (70-100%): Clean up and consolidate
            _ => {
                let roll: f32 = rng.random();

                // Strong consolidation bias if we have many UTXOs
                let consolidation_boost = if should_favor_consolidation {
                    0.15
                } else {
                    0.0
                };

                match roll {
                    r if r < (0.45 + consolidation_boost) => {
                        if can_consolidate {
                            TransactionPattern::Consolidation
                        } else {
                            TransactionPattern::Simple
                        }
                    }
                    r if r < (0.75 + consolidation_boost) => TransactionPattern::Simple,
                    r if r < (0.90 + consolidation_boost) => {
                        if can_complex {
                            TransactionPattern::Complex
                        } else {
                            TransactionPattern::Simple
                        }
                    }
                    _ => {
                        if can_split_effectively {
                            TransactionPattern::Splitting
                        } else {
                            TransactionPattern::Simple
                        }
                    }
                }
            }
        }
    }

    /// Choose UTXO selection strategy adaptively based on fees and economics
    ///
    /// Returns: (UtxoSelectionCriteria, count)
    ///
    /// Strategy:
    /// - For Simple/Splitting: Select 1 UTXO using economically optimal criteria
    /// - For Consolidation: Select multiple UTXOs to maximize consolidation benefit
    /// - For Complex: Balance between input count and output diversity
    /// - Consider minimum viable output values to prevent UTXO dust accumulation
    /// - Use fee_policy to make economically sound decisions
    pub fn choose_utxo_strategy(
        &self,
        pattern: TransactionPattern,
        available_utxos: usize,
        params: &TransactionGenerationParams,
        rng: &mut impl Rng,
    ) -> (UtxoSelectionCriteria, usize) {
        let progress = self.progress();

        match pattern {
            TransactionPattern::Simple => {
                // For simple transactions, prefer larger UTXOs to ensure viability
                // As we progress, occasionally use smaller UTXOs to clean them up
                let criteria = if progress > 0.6 && rng.random_bool(0.3) {
                    // Late phase: occasionally consolidate small UTXOs
                    UtxoSelectionCriteria::Smallest
                } else {
                    // Default: use larger UTXOs for reliability
                    UtxoSelectionCriteria::Largest
                };
                (criteria, 1)
            }

            TransactionPattern::Splitting => {
                // Splitting needs 1 large UTXO to split effectively
                // Prefer largest or newest to maximize split potential
                // Largest ensures we have enough value to split meaningfully
                let criteria = if rng.random_bool(0.8) {
                    // 80% prefer largest for better economic viability
                    UtxoSelectionCriteria::Largest
                } else {
                    // 20% use newest to keep recent outputs circulating
                    UtxoSelectionCriteria::Newest
                };
                (criteria, 1)
            }

            TransactionPattern::Consolidation => {
                // Consolidation should target smaller UTXOs to reduce UTXO set size
                // Calculate economically optimal consolidation size
                let max_reasonable_inputs = params.max_inputs.min(available_utxos);

                let count = if progress < 0.3 {
                    max_reasonable_inputs.clamp(3, 6)
                } else if progress < 0.7 {
                    max_reasonable_inputs.clamp(4, 7)
                } else {
                    max_reasonable_inputs
                };

                // Prefer smallest or oldest UTXOs for consolidation
                // Smallest cleans up dust, Oldest cleans up stale UTXOs
                let criteria = if rng.random_bool(0.7) {
                    // 70% smallest - most economically beneficial
                    UtxoSelectionCriteria::Smallest
                } else {
                    // 30% oldest - cleans up old UTXOs
                    UtxoSelectionCriteria::Oldest
                };

                (criteria, count)
            }

            TransactionPattern::Complex => {
                // Complex transactions need balanced UTXO selection
                // Use multiple inputs but not too many to keep tx size reasonable
                let max_reasonable_inputs = params.max_inputs.min(available_utxos);

                // Complex txs should use 2-5 inputs typically for good balance
                // Too many inputs = high fees, too few = not really "complex"
                let optimal_range = 2..=5.min(max_reasonable_inputs);

                let count = if progress < 0.5 {
                    // Early/mid: prefer middle of range
                    let sweet_spot = (optimal_range.start() + optimal_range.end()) / 2;
                    if rng.random_bool(0.6) {
                        sweet_spot
                    } else {
                        rng.random_range(optimal_range)
                    }
                } else {
                    // Late: allow more inputs for cleanup
                    let extended_range = 2..=max_reasonable_inputs;
                    rng.random_range(extended_range)
                };

                // For complex txs, use mixed selection strategies to create diversity
                // But weight towards economically sensible choices
                let criteria = if rng.random_bool(0.4) {
                    // 40% largest - ensures sufficient total value
                    UtxoSelectionCriteria::Largest
                } else if rng.random_bool(0.5) {
                    // 30% newest - keeps recent outputs circulating
                    UtxoSelectionCriteria::Newest
                } else if rng.random_bool(0.67) {
                    // 20% smallest - opportunistic cleanup
                    UtxoSelectionCriteria::Smallest
                } else {
                    // 10% oldest - cleans up stale UTXOs
                    UtxoSelectionCriteria::Oldest
                };

                (criteria, count)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn test_strategy_progress() {
        let mut strategy = TxGenerationStrategy::new(10);
        assert_eq!(strategy.progress(), 0.0);

        strategy.record_success();
        assert_eq!(strategy.progress(), 0.1);

        for _ in 0..9 {
            strategy.record_success();
        }
        assert_eq!(strategy.progress(), 1.0);
    }

    #[test]
    fn test_strategy_continuation() {
        const TARGET_TXS: usize = 5;
        let mut strategy = TxGenerationStrategy::new(TARGET_TXS);
        let max_attempts = strategy.max_attempts; // 15

        // Should continue while attempts < max_attempts
        for attempt in 0..max_attempts {
            assert!(
                strategy.should_continue(),
                "Should continue at attempt {} (max: {})",
                attempt,
                max_attempts
            );
            strategy.record_attempt();
        }

        // After reaching max_attempts, should not continue
        assert!(
            !strategy.should_continue(),
            "Should not continue after {} attempts",
            max_attempts
        );
    }

    #[test]
    fn test_choose_pattern_early_phase() {
        let strategy = TxGenerationStrategy::new(10);
        let params = TransactionGenerationParams::default();
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        // Early phase should favor splitting
        let mut patterns = vec![];
        for _ in 0..100 {
            let pattern = strategy.choose_pattern(10, &params, &mut rng);
            patterns.push(pattern);
        }

        let splitting_count = patterns
            .iter()
            .filter(|p| **p == TransactionPattern::Splitting)
            .count();

        // Splitting should be most common in early phase
        assert!(
            splitting_count > 30,
            "Expected >30% splitting, got {}",
            splitting_count
        );
    }

    #[test]
    fn test_choose_pattern_late_phase() {
        let mut strategy = TxGenerationStrategy::new(10);
        // Simulate late phase
        for _ in 0..8 {
            strategy.record_success();
        }

        let params = TransactionGenerationParams::default();
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        // Late phase should favor consolidation
        let mut patterns = vec![];
        for _ in 0..100 {
            let pattern = strategy.choose_pattern(10, &params, &mut rng);
            patterns.push(pattern);
        }

        let consolidation_count = patterns
            .iter()
            .filter(|p| **p == TransactionPattern::Consolidation)
            .count();

        // Consolidation should be most common in late phase
        assert!(
            consolidation_count > 35,
            "Expected >35% consolidation, got {}",
            consolidation_count
        );
    }

    #[test]
    fn test_choose_utxo_strategy_simple() {
        let strategy = TxGenerationStrategy::new(10);
        let params = TransactionGenerationParams::default();
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        let (criteria, count) =
            strategy.choose_utxo_strategy(TransactionPattern::Simple, 10, &params, &mut rng);

        assert_eq!(count, 1, "Simple pattern should always select 1 UTXO");
        // Criteria should be Largest or Smallest
        assert!(matches!(
            criteria,
            UtxoSelectionCriteria::Largest | UtxoSelectionCriteria::Smallest
        ));
    }

    #[test]
    fn test_choose_utxo_strategy_consolidation() {
        let strategy = TxGenerationStrategy::new(10);
        let params = TransactionGenerationParams::default();
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        let (criteria, count) =
            strategy.choose_utxo_strategy(TransactionPattern::Consolidation, 10, &params, &mut rng);

        assert!(count >= 2, "Consolidation should select at least 2 UTXOs");
        assert!(count <= params.max_inputs, "Should not exceed max_inputs");
        // Should prefer Smallest or Oldest for consolidation
        assert!(matches!(
            criteria,
            UtxoSelectionCriteria::Smallest | UtxoSelectionCriteria::Oldest
        ));
    }

    #[test]
    fn test_fallback_prefers_consolidation_on_failures() {
        let mut strategy = TxGenerationStrategy::new(10);
        let params = TransactionGenerationParams::default();

        // Test 1: No failures - should have diverse patterns
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut simple_count = 0;
        for _ in 0..100 {
            let pattern = strategy.choose_pattern(10, &params, &mut rng);
            if pattern == TransactionPattern::Simple {
                simple_count += 1;
            }
        }
        // At 0% progress, early phase, Simple should be around 25% (+- 10)
        assert!(
            (15..=35).contains(&simple_count),
            "With no failures, Simple should be ~25%, got {}",
            simple_count
        );

        // Test 2: Simulate reaching 30% failure threshold (9 out of 30)
        for _ in 0..9 {
            strategy.record_attempt();
        }

        // Reset RNG for consistent comparison
        let mut rng = ChaCha8Rng::seed_from_u64(100);

        let mut consolidation_count = 0;
        for _ in 0..100 {
            let pattern = strategy.choose_pattern(10, &params, &mut rng);
            if pattern == TransactionPattern::Consolidation {
                consolidation_count += 1;
            }
        }

        // After 30% failures, should strongly favor consolidation.
        assert!(
            consolidation_count >= 70,
            "After 30% failure rate, should favor Consolidation pattern, got {}",
            consolidation_count
        );

        // Test 3: Push to 50% failure rate (15 out of 30)
        for _ in 0..6 {
            strategy.record_attempt();
        }

        let mut rng = ChaCha8Rng::seed_from_u64(200);

        consolidation_count = 0;
        for _ in 0..100 {
            let pattern = strategy.choose_pattern(10, &params, &mut rng);
            if pattern == TransactionPattern::Consolidation {
                consolidation_count += 1;
            }
        }

        // After 50% failures, consolidation should be the deterministic recovery path.
        assert_eq!(
            consolidation_count, 100,
            "After 50% failure rate, should always use Consolidation when possible, got {}",
            consolidation_count
        );
    }
}
