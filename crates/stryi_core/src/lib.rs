#![allow(incomplete_features)]
#![feature(generic_const_exprs)]


/// `hash` module contains some higher-level abstractions for typed hashing
/// There is a lot of stuff that has own hash format in the Stryi-Chain : 
/// - `@{20_hex_bytes}` which stands for addresses. @ prefix is inspired by usernames in social networks.
/// for example : `@b7e3a9c2d4f5061728394b5c6d7e8f9012345678`, `@faded2c4d5e60718293a4b5c6d7e8f9012345678`
/// - `Bx{32_hex_bytes}` which stands for block hash (BlockHash structure) format.
/// Example of BlockHash with 24 leading zero bits : `Bx000000a3f4b2c1d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6`
mod hash;

/// Definition of StryiError enum.
mod error;

/// Implementation Block primitive of the blockchain, includes high-level APIs and parallel CPU mining module.
mod block;

/// Definition of AccountAddress type and some batteries for constructing it from public key.
mod address;

/// Simple implementation of the (Merkle Tree)[https://en.wikipedia.org/wiki/Merkle_tree]
/// Provides simple api for constructing trees, generating and checking proofs
mod merkletree;

/// Definition of Transaction, TransactionHash, API for signing and validating, module for checking and performing UTXOs logic.
mod transactions;

/// Definitions of traits that we use as abstract layer for storing data. Exports 'UtxoStorage' and BlockStorage so far
mod storage;

/// Abstraction for consensus model of StryiChain. 
/// Provides a convenient way for all the nodes to follow the same, strict rules of consensus
mod consensus;


// Contains tests for some matter functionality.
// Some of the tests are present in the concrete modules, but this module contains larger ones with more complex cases like integration tests
#[cfg(test)]
mod tests;