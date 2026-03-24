#![allow(incomplete_features)]
#![feature(generic_const_exprs)]

/// `hash` module contains some higher-level abstractions for typed hashing
/// There is a lot of stuff that has own hash format in the Stryi-Chain :
/// - `@{20_hex_bytes}` which stands for addresses. @ prefix is inspired by usernames in social networks.
///   for example : `@b7e3a9c2d4f5061728394b5c6d7e8f9012345678`, `@faded2c4d5e60718293a4b5c6d7e8f9012345678`
/// - `Bx{32_hex_bytes}` which stands for block hash (BlockHash structure) format.
///   Example of BlockHash with 24 leading zero bits : `Bx000000a3f4b2c1d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6`
mod hash;

mod error;
pub use error::StryiCoreError;

/// Implementation of the Block primitive for the blockchain.
/// Includes high-level APIs and parallel CPU mining module.
pub mod block;

/// Account addresses and utilities for deriving them from public keys.
pub mod address;

/// Simple implementation of the [Merkle Tree](https://en.wikipedia.org/wiki/Merkle_tree)
/// Provides simple api for constructing trees, generating and checking proofs
pub mod merkletree;

/// Definition of [`Transaction`], [`TransactionHash`], utilities for signing and validating the authority, and
/// module for checking and performing UTXOs logic.
pub mod transactions;

/// Definitions of traits that we use as abstract layer for storing data.
pub mod storage;

/// The consensus model of StryiChain.
/// Provides a convenient way for all the nodes to follow the same, strict rules of consensus
pub mod consensus;

/// Primitives for tracking transaction dependencies within a block.
/// Uses a directed acyclic graph ([DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph)) to model dependency order.
mod dependencies;

/// Definitions of BlockUndo and related logic for the reorganization system.
mod undo;
pub use undo::BlockUndo;

/// Implementation of the [Memory Pool](https://learnmeabitcoin.com/technical/mining/memory-pool/)
pub mod mempool;

// public export of common libraries across the project
pub use blake3;

/// tools for proper difficulty calculation/validation for blocks
pub mod difficulty;

/// Simple serialization/deserialization for private keys in StryiChain
mod private_key;
pub use private_key::*;

// Some tests are present in the concrete modules, this module contains larger ones with more complex cases like integration tests
#[cfg(test)]
mod tests;
