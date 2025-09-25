use crate::chaingen::seed::SeedValue;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

/// Precomputed seed switches: (switch_height, seed), sorted by height.
/// Heights are 1-based (post-genesis). Single(x) is encoded as [(1, x)].
#[derive(Debug, Clone)]
pub struct SeedSchedule {
    ranges: Vec<(u64, u64)>, // (height, seed)
}

impl SeedSchedule {
    /// Build a schedule from a validated SeedValue.
    pub fn new(seed: &SeedValue) -> Self {
        let mut ranges = Vec::new();
        match seed {
            // single - return single pair
            SeedValue::Single(v) => ranges.push((1, *v)),
            // ranged - build pairs
            SeedValue::Ranged(v) => {
                ranges.reserve(v.len());
                for r in v {
                    ranges.push((r.height(), r.value()));
                }
                ranges.sort_unstable_by_key(|(h, _)| *h);
            }
        }
        debug_assert!(
            !ranges.is_empty(),
            "SeedSchedule must not be empty after validate()"
        );
        Self { ranges }
    }

    /// ChaCha8 RNG from a seed.
    #[inline]
    pub fn rng_from_seed(seed: u64) -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(seed)
    }

    /// Seed active at height (>= 1).
    pub fn seed_at(&self, height: u64) -> u64 {
        // First index with switch_height > height; take previous entry (last <= height)
        let idx = self.ranges.partition_point(|(h, _)| *h <= height);
        self.ranges[if idx == 0 { 0 } else { idx - 1 }].1
    }

    /// True if there is an exact seed switch at height.
    pub fn is_switch_height(&self, height: u64) -> bool {
        self.ranges
            .binary_search_by_key(&height, |(h, _)| *h)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chaingen::seed::{SeedRange, SeedValue};
    use rand::Rng;
    use rand_chacha::ChaCha8Rng;

    /// Build SeedValue::Ranged from (height, seed) pairs.
    fn ranged_pairs(pairs: &[(u64, u64)]) -> SeedValue {
        let v: Vec<SeedRange> = pairs
            .iter()
            .copied()
            .map(|(h, s)| SeedRange::new_test(h, s))
            .collect();
        SeedValue::Ranged(v)
    }

    #[test]
    fn single_seed_basic() {
        let sv = SeedValue::Single(42);
        let sched = SeedSchedule::new(&sv);

        assert_eq!(sched.seed_at(1), 42);
        assert_eq!(sched.seed_at(2), 42);
        assert_eq!(sched.seed_at(999_999), 42);

        assert!(sched.is_switch_height(1));
        assert!(!sched.is_switch_height(2));
        assert!(!sched.is_switch_height(10_000));
    }

    #[test]
    fn ranged_two_switches_boundaries() {
        let sv = ranged_pairs(&[(1, 10), (100, 11)]);
        let sched = SeedSchedule::new(&sv);

        assert_eq!(sched.seed_at(1), 10);
        assert_eq!(sched.seed_at(2), 10);
        assert_eq!(sched.seed_at(99), 10);
        assert_eq!(sched.seed_at(100), 11);
        assert_eq!(sched.seed_at(101), 11);
        assert_eq!(sched.seed_at(10_000), 11);

        assert!(sched.is_switch_height(1));
        assert!(sched.is_switch_height(100));
        assert!(!sched.is_switch_height(99));
        assert!(!sched.is_switch_height(101));
    }

    #[test]
    fn ranged_many_segments() {
        let sv = ranged_pairs(&[(1, 1), (5, 2), (50, 3), (199, 4)]);
        let sched = SeedSchedule::new(&sv);

        // [1..=4] -> 1
        assert_eq!(sched.seed_at(1), 1);
        assert_eq!(sched.seed_at(4), 1);
        // [5..=49] -> 2
        assert_eq!(sched.seed_at(5), 2);
        assert_eq!(sched.seed_at(49), 2);
        // [50..=198] -> 3
        assert_eq!(sched.seed_at(50), 3);
        assert_eq!(sched.seed_at(198), 3);
        // [199..=inf) -> 4
        assert_eq!(sched.seed_at(199), 4);
        assert_eq!(sched.seed_at(1_000), 4);

        for h in [1_u64, 5, 50, 199] {
            assert!(sched.is_switch_height(h));
        }
        for h in [2_u64, 4, 6, 48, 51, 150, 198, 200] {
            assert!(!sched.is_switch_height(h));
        }
    }

    #[test]
    fn reseeding_produces_chunked_sequences() {
        // Switches: 1->1234, 5->42, 8->7
        let sv = ranged_pairs(&[(1, 1234), (5, 42), (8, 7)]);
        let sched = SeedSchedule::new(&sv);

        // Simulate production with reseeding at exact switch heights.
        let mut rng = SeedSchedule::rng_from_seed(sched.seed_at(1));
        let mut produced = Vec::new();
        for h in 1..=10 {
            if h != 1 && sched.is_switch_height(h) {
                rng = SeedSchedule::rng_from_seed(sched.seed_at(h));
            }
            produced.push(rng.random::<u32>());
        }

        // Expected = concat of first k values per range: [1..=4], [5..=7], [8..=10].
        let mut expected = Vec::new();
        let mut r1 = ChaCha8Rng::seed_from_u64(1234);
        for _ in 0..4 {
            expected.push(r1.random::<u32>());
        }
        let mut r2 = ChaCha8Rng::seed_from_u64(42);
        for _ in 0..3 {
            expected.push(r2.random::<u32>());
        }
        let mut r3 = ChaCha8Rng::seed_from_u64(7);
        for _ in 0..3 {
            expected.push(r3.random::<u32>());
        }

        assert_eq!(produced, expected);
    }
}
