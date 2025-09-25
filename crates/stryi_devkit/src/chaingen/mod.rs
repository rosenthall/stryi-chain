/// Configuration struct for ChainGen.
mod config;
pub use config::*;

/// Definition and helpers for Seed struct used in ChainGen.
pub mod seed;

/// Seed schedule: mapping from height to active seed.
/// Used to create ChaCha8Rng instances for block synthesis.
mod seed_schedule;