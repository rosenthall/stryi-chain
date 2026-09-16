use crate::StryiStorageError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

// Header of our binary meta file
const META_MAGIC: &[u8; 8] = b"STRYIMBI"; // "Stryi Meta BIN"
const META_VERSION: u32 = 1;
const META_HEADER_SIZE: usize = 8 + 4 + 4 + 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum StorageStatus {
    /// No committed genesis yet (directory may or may not exist).
    ///
    /// NOTE: This does NOT guarantee that the chain is empty.
    /// It only means that `metainfo.bin` does not exist.
    NoGenesis,
    /// Storage has a committed genesis and carries essential metadata.
    Initialized {
        chain_name: String,
        protocol_version: u64,
        created_unix_ts: i64,
    },
    /// Meta exists but is unreadable or malformed.
    Corrupted { reason: String },
}

// helper to build a path to a file with metainfo about storage
fn meta_file_path(root: &Path) -> PathBuf {
    root.join("metainfo.bin")
}

impl StorageStatus {
    pub fn from_path<P: AsRef<Path>>(root: P) -> Result<StorageStatus, StryiStorageError> {
        // Check if root even exists
        let root = root.as_ref();
        if !root.exists() {
            return Ok(StorageStatus::NoGenesis);
        }

        // Check if metafile already exists
        let path = meta_file_path(root);
        if !path.exists() {
            return Ok(StorageStatus::NoGenesis);
        }

        read_status_bin(&path)
    }
}

/// Atomically write metainfo.bin with a small header + postcard payload.
pub fn write_status_atomic(root: &Path, status: StorageStatus) -> Result<(), StryiStorageError> {
    if !root.exists() {
        fs::create_dir_all(root).map_err(StryiStorageError::Io)?;
    }

    let tmp = root.join("metainfo.bin.tmp");
    let dst = meta_file_path(root);

    // Build payload
    let payload = postcard::to_stdvec(&status)
        .map_err(StryiStorageError::SerializationError)?;

    // Header: MAGIC(8) + VERSION(u32 LE) + LEN(u32 LE) + BLAKE3(payload)(32)
    let mut header = Vec::with_capacity(8 + 4 + 4 + 32);
    header.extend_from_slice(META_MAGIC);
    header.extend_from_slice(&META_VERSION.to_le_bytes());
    header.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let mut hasher = blake3::Hasher::new();
    hasher.update(&payload);
    header.extend_from_slice(hasher.finalize().as_bytes());

    {
        let mut f = File::create(&tmp)?;
        f.write_all(&header)?;
        f.write_all(&payload)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &dst)?;
    Ok(())
}

/// reads the metafile
pub fn read_status_bin(path: &Path) -> Result<StorageStatus, StryiStorageError> {
    let mut buf = Vec::new();
    let mut f = File::open(path).map_err(|e| StryiStorageError::IncorrectPath {
        msg: format!("Cannot open meta file in the root of storage : {e}"),
    })?;

    f.read_to_end(&mut buf)
        .map_err(|e| StryiStorageError::IncorrectPath {
            msg: format!("Cannot read meta file in the root of storage : {e}"),
        })?;

    // Minimum header length.
    if buf.len() < META_HEADER_SIZE {
        return Ok(StorageStatus::Corrupted {
            reason: "truncated header".to_string(),
        });
    }

    let mut off = 0usize;

    // MAGIC
    if &buf[off..off + 8] != META_MAGIC {
        return Ok(StorageStatus::Corrupted {
            reason: "bad magic".to_string(),
        });
    }
    off += 8;

    // VERSION
    let version = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
    if version != META_VERSION {
        return Ok(StorageStatus::Corrupted {
            reason: format!("unsupported version {version}"),
        });
    }
    off += 4;

    // LEN
    let payload_len = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
    off += 4;

    // Check lengths
    if buf.len() < off + 32 + payload_len {
        return Ok(StorageStatus::Corrupted {
            reason: "truncated payload".to_string(),
        });
    }

    // DIGEST
    let digest = &buf[off..off + 32];
    off += 32;

    // PAYLOAD
    let payload = &buf[off..off + payload_len];

    // Verify checksum
    let mut hasher = blake3::Hasher::new();
    hasher.update(payload);
    let computed = hasher.finalize();
    if computed.as_bytes() != digest {
        return Ok(StorageStatus::Corrupted {
            reason: "checksum mismatch".to_string(),
        });
    }

    // Decode postcard payload
    let meta: StorageStatus = postcard::from_bytes(payload)
        .map_err(StryiStorageError::DeserializationError)?;

    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn from_path_on_empty_dir_returns_no_genesis() {
        let dir = TempDir::new().expect("tempdir");
        let status = StorageStatus::from_path(dir.path()).expect("probe");
        assert_eq!(status, StorageStatus::NoGenesis);
    }

    #[test]
    fn write_initialized_then_read_back_roundtrip() {
        let dir = TempDir::new().expect("tempdir");

        let expected = StorageStatus::Initialized {
            chain_name: "StryiChain Devnet".to_string(),
            protocol_version: 1,
            created_unix_ts: 1_725_000_000,
        };

        write_status_atomic(dir.path(), expected.clone()).expect("write");
        let read_back = StorageStatus::from_path(dir.path()).expect("read");
        assert_eq!(read_back, expected);
    }

    #[test]
    fn write_no_genesis_then_read_back_roundtrip() {
        let dir = TempDir::new().expect("tempdir");

        let expected = StorageStatus::NoGenesis;

        write_status_atomic(dir.path(), expected.clone()).expect("write");
        let read_back = StorageStatus::from_path(dir.path()).expect("read");
        assert_eq!(read_back, expected);
    }

    #[test]
    fn write_creates_directory_if_missing() {
        let dir = TempDir::new().expect("tempdir");
        // Intentionally point to a non-existent subdirectory inside tempdir
        let sub = dir.path().join("nested").join("chaindata");

        let expected = StorageStatus::Initialized {
            chain_name: "StryiChain Local".to_string(),
            protocol_version: 7,
            created_unix_ts: 1_735_000_000,
        };

        // Should create the directory and write the meta file atomically
        write_status_atomic(&sub, expected.clone()).expect("write");

        // Now probe back from that subdirectory
        let read_back = StorageStatus::from_path(&sub).expect("read");
        assert_eq!(read_back, expected);

        // Additionally ensure the meta file physically exists
        let meta_path = super::meta_file_path(&sub);
        assert!(
            meta_path.exists(),
            "meta file should exist at {}",
            meta_path.display()
        );
    }
}
