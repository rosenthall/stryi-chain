use crypto_bigint::{U256, U512};

/// Applies ASERT to a full-width upper target, without compact rounding.
/// `drift` is elapsed time minus expected time; positive drift makes mining easier.
/// Returns None for zero initial, cap or half-life, or when initial exceeds cap.
/// Fixed-point calculation and polynomial coefficients:
/// https://upgradespecs.bitcoincashnode.org/2020-11-15-asert/
pub fn asert_upper(initial: U256, drift: i128, half_life_secs: u32, cap: U256) -> Option<U256> {
    const FRACTION_BITS: i128 = 16;
    const FRACTION_SCALE: i128 = 1 << FRACTION_BITS;

    if initial == U256::ZERO || cap == U256::ZERO || initial > cap || half_life_secs == 0 {
        return None;
    }
    let half_life = i128::from(half_life_secs);
    let cutoff = 256 * half_life;
    // Every positive U256 saturates at these exponents, before drift multiplication.
    if drift >= cutoff {
        return Some(cap);
    }
    if drift <= -cutoff {
        return Some(U256::ONE);
    }

    // FRACTION_SCALE units represent one half-life; factor uses the same scale.
    // Signed division truncates toward zero; only the split below uses floor.
    let exponent = (drift * FRACTION_SCALE) / half_life;
    let whole_half_lives = exponent.div_euclid(FRACTION_SCALE);
    let fraction = exponent.rem_euclid(FRACTION_SCALE) as u128;
    let factor = FRACTION_SCALE as u128
        + ((195_766_423_245_049u128 * fraction
            + 971_821_376u128 * fraction * fraction
            + 5_127u128 * fraction * fraction * fraction
            + (1u128 << 47))
            >> 48);

    // factor < 2^17 and the left shift is at most 239, so U512 retains every bit.
    let scaled_target: U512 = initial.concatenating_mul(&U256::from(factor as u64));
    // Subtracting FRACTION_BITS removes factor's scale without losing bits first.
    let target = if whole_half_lives >= FRACTION_BITS {
        scaled_target.shl_vartime((whole_half_lives - FRACTION_BITS) as u32)
    } else {
        scaled_target.shr_vartime((FRACTION_BITS - whole_half_lives) as u32)
    };
    let wide_cap: U512 = cap.resize();
    if target == U512::ZERO {
        Some(U256::ONE)
    } else if target > wide_cap {
        Some(cap)
    } else {
        target.resize_checked().into_option()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asert_whole_half_lives_scale_target() {
        let initial = U256::from(1_000_000u64);
        let cap = U256::from(10_000_000u64);
        for (drift, expected) in [
            (-7200, 250_000u64),
            (-3600, 500_000),
            (0, 1_000_000),
            (3600, 2_000_000),
            (7200, 4_000_000),
        ] {
            assert_eq!(
                asert_upper(initial, drift, 3600, cap),
                Some(U256::from(expected))
            );
        }
    }

    #[test]
    fn asert_fractional_half_lives_use_fixed_point_polynomial() {
        let initial = U256::from(1_000_000u64);
        let cap = U256::from(10_000_000u64);
        for (drift, expected) in [
            (-1800, 707_046u64),
            (-1, 999_809),
            (1, 1_000_198),
            (1800, 1_414_093),
        ] {
            assert_eq!(
                asert_upper(initial, drift, 3600, cap),
                Some(U256::from(expected))
            );
        }
    }

    #[test]
    fn asert_negative_division_truncates_toward_zero() {
        let initial = U256::from(1_000_000u64);
        assert_eq!(asert_upper(initial, -1, 172_800, U256::MAX), Some(initial));
    }

    #[test]
    fn asert_extremes_clamp_without_overflow() {
        for half_life_secs in [1, u32::MAX] {
            assert_eq!(
                asert_upper(U256::ONE, i128::MIN, half_life_secs, U256::MAX),
                Some(U256::ONE)
            );
            assert_eq!(
                asert_upper(U256::ONE, i128::MAX, half_life_secs, U256::MAX),
                Some(U256::MAX)
            );
        }
        assert_eq!(asert_upper(U256::MAX, 0, 1, U256::MAX), Some(U256::MAX));
    }

    #[test]
    fn asert_clamps_to_positive_target_range() {
        assert_eq!(
            asert_upper(U256::ONE, -3600, 3600, U256::MAX),
            Some(U256::ONE)
        );
        assert_eq!(
            asert_upper(U256::from(9u64), 3600, 3600, U256::from(10u64)),
            Some(U256::from(10u64))
        );
        assert_eq!(asert_upper(U256::ONE, 1, 1, U256::ONE), Some(U256::ONE));
    }

    #[test]
    fn asert_rejects_invalid_parameters() {
        for (initial, half_life_secs, cap) in [
            (U256::ZERO, 1, U256::MAX),
            (U256::ONE, 0, U256::MAX),
            (U256::ONE, 1, U256::ZERO),
            (U256::MAX, 1, U256::ONE),
        ] {
            assert_eq!(asert_upper(initial, i128::MAX, half_life_secs, cap), None);
        }
    }
}
