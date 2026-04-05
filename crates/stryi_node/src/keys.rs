//! Utilities for **peer-identity key** handling.
//!
//! * Generate or restore a `stryi_network::Keypair` (Ed25519).
//! * Back up the key into a single binary file (`*.stryi_keys` / `*.bin`)
//!   using libp2p-protobuf encoding and `0o600` permissions.
//!
//! This module deals with the *peer* key only; wallet keys are stored elsewhere.

use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use stryi_network::Keypair;
use tracing::info;
use zeroize::Zeroize;

use crate::error::StryiNodeError;

/// Wrapper around `Keypair` that guarantees secret wipe on drop.
pub struct PeerKey(pub Keypair);

impl PeerKey {
    /// Generate a fresh Ed25519 peer key.
    pub fn generate_random() -> Self {
        Self(Keypair::generate_ed25519())
    }

    /// Restore an existing key or generate and persist a new one when it is missing.
    pub fn restore_or_generate<P: AsRef<Path>>(path: P) -> Result<Self, StryiNodeError> {
        let path = path.as_ref();

        match Self::restore(path) {
            Ok(key) => {
                info!("Restored peer key from {}", path.display());
                Ok(key)
            }
            Err(StryiNodeError::Io(err)) if err.kind() == ErrorKind::NotFound => {
                info!("No existing peer key, generating a fresh one");
                let fresh = Self::generate_random();
                fresh.backup(path).map_err(|e| {
                    StryiNodeError::other(format!(
                        "Failed to persist generated peer key at {}: {e}",
                        path.display()
                    ))
                })?;
                Ok(fresh)
            }
            Err(err) => Err(StryiNodeError::other(format!(
                "Failed to restore peer key from {}: {err}",
                path.display()
            ))),
        }
    }

    /// Back up the key to `path` (protobuf encoding, 0o600).
    ///
    /// Extensions like `.stryi_keys` or `.bin` are customary but not enforced.
    pub fn backup<P: AsRef<Path>>(&self, path: P) -> Result<(), StryiNodeError> {
        let path = path.as_ref();

        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(StryiNodeError::from)?;
        }

        // serialize
        let mut bytes = self
            .0
            .to_protobuf_encoding()
            .map_err(StryiNodeError::from)?; // KeyEncode

        // secure file create/overwrite
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(StryiNodeError::from)?; // Io

        // write & flush
        file.write_all(&bytes)
            .and_then(|_| file.flush())
            .map_err(StryiNodeError::from)?;

        // wipe buffer
        bytes.zeroize();
        Ok(())
    }

    /// Restore a key from `path`.
    pub fn restore<P: AsRef<Path>>(path: P) -> Result<Self, StryiNodeError> {
        // read file
        let mut buf = Vec::new();
        File::open(path.as_ref())?.read_to_end(&mut buf)?;

        // deserialize
        let key = Keypair::from_protobuf_encoding(&buf).map_err(StryiNodeError::from)?; // KeyDecode

        // wipe buffer
        buf.zeroize();

        Ok(Self(key))
    }

    /// Immutable access to the inner `Keypair`.
    pub fn inner(&self) -> &Keypair {
        &self.0
    }
}
