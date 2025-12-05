use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, Eq, PartialEq)]
#[serde(untagged)]
pub enum SeedValue {
    Single(u64),
    Ranged(Vec<SeedRange>),
}

/// A switch point in the seed ranges.
/// At block `height`, the RNG seed becomes `value`.
/// The seed remains `value` until the next switch point or the end of the chain.
#[derive(Debug, Deserialize, Clone, Copy, Eq, PartialEq)]
pub struct SeedRange {
    /// Inclusive block height where this seed becomes active.
    height: u64,

    /// The seed value from this height onward.
    value: u64,
}

impl SeedRange {
    /// getter for the `height` field
    #[inline]
    pub fn height(&self) -> u64 {
        self.height
    }
    /// getter for the `value` field
    #[inline]
    pub fn value(&self) -> u64 {
        self.value
    }
}

#[cfg(test)]
impl SeedRange {
    /// Test-only constructor to build ranges directly from values with no need of deserialization
    pub fn new_test(height: u64, value: u64) -> Self {
        Self { height, value }
    }
}

/// Validate the SeedValue.
/// num_blocks must always be > 0.
///
/// If Single, seed value must be >= 1.
///
/// If Ranged:
/// - First height must be 1.
/// - All heights must be in [1, num_blocks].
/// - Heights must be strictly increasing.
pub fn validate(seed: &SeedValue, num_blocks: u64) -> Result<(), String> {
    match seed {
        // if seed is a single value - just check it's >= 1
        SeedValue::Single(v) => {
            if *v == 0 {
                return Err("seed value must be >= 1".to_string());
            }
            Ok(())
        }

        // if seed is a list of ranges - validate them.
        SeedValue::Ranged(ranges) => {
            if num_blocks == 0 {
                return Err(
                    "seed ranges are not allowed when num_blocks == 0 (no post-genesis blocks)"
                        .to_string(),
                );
            }

            // if empty - error
            if ranges.is_empty() {
                return Err("seed ranges cannot be empty".to_string());
            }

            // First switch point must be 1.
            let first = &ranges[0];
            if first.height != 1 {
                return Err(format!(
                    "first seed range height must be 1 (got {})",
                    first.height
                ));
            }

            // First seed value must be >= 1.
            if first.value == 0 {
                return Err("seed range value must be >= 1 (at index 0)".to_string());
            }

            // First range height cannot exceed num_blocks.
            if first.height > num_blocks {
                return Err(format!(
                    "seed range height {} exceeds num_blocks {} (at index 0)",
                    first.height, num_blocks
                ));
            }

            // Strictly increasing heights, all within [1, num_blocks], all values >= 1.
            for (i, r) in ranges.iter().enumerate().skip(1) {
                if r.value == 0 {
                    return Err(format!("seed range value must be >= 1 (at index {})", i));
                }
                if r.height < 1 {
                    return Err(format!(
                        "seed range height must be >= 1 (at index {}, got {})",
                        i, r.height
                    ));
                }
                if r.height > num_blocks {
                    return Err(format!(
                        "seed range height {} exceeds num_blocks {} (at index {})",
                        r.height, num_blocks, i
                    ));
                }
                let prev = &ranges[i - 1];
                if r.height <= prev.height {
                    return Err(format!(
                        "seed range heights must be strictly increasing (prev={} at index {}, got {} at index {})",
                        prev.height,
                        i - 1,
                        r.height,
                        i
                    ));
                }
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct Case {
        name: &'static str,
        seed: SeedValue,
        num_blocks: u64,
        expect_ok: bool,
        expect_err_substr: Option<&'static str>,
    }

    /// Simple helper function to instantize seed range.
    fn range(height: u64, value: u64) -> SeedRange {
        SeedRange { height, value }
    }

    #[test]
    fn validate_seed_value_big_sweep() {
        let cases = vec![
            Case {
                name: "single_ok_basic",
                seed: SeedValue::Single(42),
                num_blocks: 200,
                expect_ok: true,
                expect_err_substr: None,
            },
            Case {
                name: "ranged_two_points_ok",
                seed: SeedValue::Ranged(vec![range(1, 42), range(100, 43)]),
                num_blocks: 200,
                expect_ok: true,
                expect_err_substr: None,
            },
            Case {
                name: "ranged_many_points_ok",
                seed: SeedValue::Ranged(vec![
                    range(1, 1),
                    range(5, 2),
                    range(50, 3),
                    range(199, 4),
                ]),
                num_blocks: 200,
                expect_ok: true,
                expect_err_substr: None,
            },
            Case {
                name: "ranged_empty_err",
                seed: SeedValue::Ranged(vec![]),
                num_blocks: 200,
                expect_ok: false,
                expect_err_substr: Some("seed ranges cannot be empty"),
            },
            Case {
                name: "ranged_first_not_one_err",
                seed: SeedValue::Ranged(vec![range(2, 42)]),
                num_blocks: 200,
                expect_ok: false,
                expect_err_substr: Some("first seed range height must be 1"),
            },
            Case {
                name: "ranged_value_zero_first_err",
                seed: SeedValue::Ranged(vec![range(1, 0)]),
                num_blocks: 200,
                expect_ok: false,
                expect_err_substr: Some("seed range value must be >= 1"),
            },
            Case {
                name: "ranged_value_zero_mid_err",
                seed: SeedValue::Ranged(vec![range(1, 42), range(100, 0)]),
                num_blocks: 200,
                expect_ok: false,
                expect_err_substr: Some("seed range value must be >= 1"),
            },
            Case {
                name: "ranged_height_zero_err",
                seed: SeedValue::Ranged(vec![range(1, 42), range(0, 43)]),
                num_blocks: 200,
                expect_ok: false,
                expect_err_substr: Some("seed range height must be >= 1"),
            },
            Case {
                name: "ranged_not_strictly_increasing_equal_err",
                seed: SeedValue::Ranged(vec![range(1, 42), range(100, 43), range(100, 44)]),
                num_blocks: 200,
                expect_ok: false,
                expect_err_substr: Some("strictly increasing"),
            },
            Case {
                name: "ranged_not_strictly_increasing_decrease_err",
                seed: SeedValue::Ranged(vec![range(1, 42), range(100, 43), range(50, 44)]),
                num_blocks: 200,
                expect_ok: false,
                expect_err_substr: Some("strictly increasing"),
            },
            Case {
                name: "ranged_height_exceeds_num_blocks_err",
                seed: SeedValue::Ranged(vec![range(1, 42), range(250, 43)]),
                num_blocks: 200,
                expect_ok: false,
                expect_err_substr: Some("exceeds num_blocks"),
            },
            Case {
                name: "ranged_first_exceeds_num_blocks_err",
                seed: SeedValue::Ranged(vec![range(1, 42), range(999, 43)]),
                num_blocks: 200,
                expect_ok: false,
                expect_err_substr: Some("exceeds num_blocks"),
            },
            Case {
                name: "single_with_zero_blocks_ok",
                seed: SeedValue::Single(1),
                num_blocks: 0,
                expect_ok: true,
                expect_err_substr: None,
            },
            Case {
                name: "ranged_with_zero_blocks_err",
                seed: SeedValue::Ranged(vec![range(1, 42)]),
                num_blocks: 0,
                expect_ok: false,
                expect_err_substr: Some("num_blocks == 0"),
            },
            Case {
                name: "single_zero_err",
                seed: SeedValue::Single(0),
                num_blocks: 123_456,
                expect_ok: false,
                expect_err_substr: Some("seed value must be >= 1"),
            },
        ];

        // Iterate through all the cases
        for case in cases {
            // validate
            let res = validate(&case.seed, case.num_blocks);

            // and compare validation result with expected.
            match (case.expect_ok, res) {
                // no error expected, no error occurred
                (true, Ok(())) => {}

                // no error expected, some error occurred
                (true, Err(e)) => panic!("[{}] expected Ok, got Err: {e}", case.name),

                // Error expected, got Ok
                (false, Ok(())) => panic!("[{}] expected Err, got Ok", case.name),

                // If expected error and got error - make sure that reason of error is identical to the one we expected.
                (false, Err(e)) => {
                    if let Some(substr) = case.expect_err_substr {
                        assert!(
                            e.contains(substr),
                            "[{}] error message mismatch.\nExpected to contain: '{}'\nActual:   '{}'",
                            case.name,
                            substr,
                            e
                        );
                    }
                }
            }
        }
    }
}
