#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
#![feature(new_range_api)]

/// `hash` module contains some higher-level abstractions for typed hashing
/// There is a lot of stuff that has own hash format in the Stryi-Chain : 
/// - `@{20_hex_bytes}` which stands for addresses. @ prefix is inspired by usernames in social networks.
/// for example : `@b7e3a9c2d4f5061728394b5c6d7e8f9012345678`, `@faded2c4d5e60718293a4b5c6d7e8f9012345678`
/// - `Bx{32_hex_bytes}` which stands for block hash (BlockHash structure) format.
/// Example of BlockHash with 24 leading zero bits : `Bx000000a3f4b2c1d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6`
mod hash;

/// Definition of StryiError enum.
mod error;
pub use error::StryiCoreError;

/// Implementation Block primitive of the blockchain, includes high-level APIs and parallel CPU mining module.
pub mod block;

/// Definition of AccountAddress type and some batteries for constructing it from public key.
pub mod address;

/// Simple implementation of the [Merkle Tree](https://en.wikipedia.org/wiki/Merkle_tree)
/// Provides simple api for constructing trees, generating and checking proofs
pub mod merkletree;

/// Definition of Transaction, TransactionHash, API for signing and validating, module for checking and performing UTXOs logic.
pub mod transactions;

/// Definitions of traits that we use as abstract layer for storing data. Exports 'UtxoStorage' and BlockStorage so far
pub mod storage;

/// Abstraction for consensus model of StryiChain. 
/// Provides a convenient way for all the nodes to follow the same, strict rules of consensus
pub mod consensus;

/// Transaction's dependencies handling primitives
/// dependencies among transactions within a block. It utilizes a Directed
/// Acyclic Graph [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph) to represent and resolve dependencies, ensuring that
/// transactions are processed in an order that respects their interdependencies.
mod dependencies;

/// Definitions of BlockUndo and related logic for the reorganization system.
mod undo;
pub use undo::BlockUndo;

/// Implementation of the transactions [mempool](https://www.geeksforgeeks.org/what-is-a-memory-pool/).
pub mod mempool;

/// A tree‐based structure for managing blockchain forks:
/// keeps orphaned blocks indexed by hash and height,
/// tracks each fork’s cumulative difficulty and divergence point,
/// and provides efficient ancestor discovery, chain reconstruction
mod forktree;

// public export of common libraries across the project
pub use blake3;

// Contains tests for some matter functionality.
// Some of the tests are present in the concrete modules, but this module contains larger ones with more complex cases like integration tests
#[cfg(test)]
mod tests;