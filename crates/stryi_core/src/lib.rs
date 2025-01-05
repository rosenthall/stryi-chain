#![allow(incomplete_features)]
#![feature(generic_const_exprs)]


/// `hash` module contains some higher-level abstractions for hashing
/// There is a lot of stuff that has own hash format in the Stryi-Chain : 
/// - `@{20_hex_bytes}` which stands for addresses. @ prefix is inspired by usernames in social networks.
/// for example : `@b7e3a9c2d4f5061728394b5c6d7e8f9012345678`, `@faded2c4d5e60718293a4b5c6d7e8f9012345678`
/// - `Bx{32_hex_bytes}` which stands for block hash (BlockHash structure) format.
/// Example of BlockHash with 24 leading zero bits : `Bx000000a3f4b2c1d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6`
mod hash;
mod error;

mod address;