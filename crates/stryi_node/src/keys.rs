//! Utilities for **peer‑identity key** handling.
//!
//! * Generate or restore a `stryi_network::Keypair` (note: we use ed25519).
//! * Back up the key into a single binary file (`*.stryi_keys` / `*.bin`) with `0o600` permissions.
//! * Zeroize sensitive buffers after use and on drop.
//!
//! This module handles the *peer* key only; wallet keys live elsewhere.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use stryi_network::{Keypair};
use zeroize::Zeroize;

use crate::error::StryiNodeError;

/// Wrapper around Keypair for convenient backups and restoring from file.
pub struct PeerKey(pub Keypair);

impl PeerKey {
    /// Generate a fresh Ed25519 peer key.
    pub fn generate_random() -> Self {
        Self(Keypair::generate_ed25519())
    }

    /// Write the key to `path` in libp2p protobuf encoding.
    /// `path` meant to have either .stryi_keys or .bin extension for better consistency, but you can use whatever you want
    /// 
    /// The file is created (or truncated) with mode `0o600`.
    pub fn backup<P: AsRef<Path>>(&self, path: P) -> Result<(), StryiNodeError> {
        
        // Serialize the key
        let mut bytes = self
            .0
            .to_protobuf_encoding()
            .map_err(|e| StryiNodeError::other(format!("encode peer key: {e:?}")))?;

        // Securely create or overwrite the file
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path.as_ref())
            .map_err(|e| StryiNodeError::other(format!("open backup file: {e}")))?;

        // Write and flush
        file.write_all(&bytes)
            .and_then(|_| file.flush())
            .map_err(|e| StryiNodeError::other(format!("write backup file: {e}")))?;

        // Zeroize the serialized buffer
        bytes.zeroize();
        Ok(())
    }

    /// Restore a key from `path`.
    pub fn restore<P: AsRef<Path>>(path: P) -> Result<Self, StryiNodeError> {
        // Read file contents
        let mut file = File::open(path.as_ref())
            .map_err(|e| StryiNodeError::other(format!("open restore file: {e}")))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|e| StryiNodeError::other(format!("read restore file: {e}")))?;

        // Deserialize
        let key = Keypair::from_protobuf_encoding(&buf)
            .map_err(|e| StryiNodeError::other(format!("decode peer key: {e:?}")))?;

        buf.zeroize();
        Ok(Self(key))
    }

    /// Immutable access to the inner `Keypair`.
    pub fn inner(&self) -> &Keypair {
        &self.0
    }
}