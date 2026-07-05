//! On-disk SST (sorted string table) file layout.
//!
//! `format.lock` anchor: `SstFile:1` — bump [`SST_FORMAT_VERSION`] and this
//! doc comment together whenever the byte layout below changes.
//!
//! Whole-file layout (little-endian):
//!
//! ```text
//! magic:       u32  = SST_MAGIC
//! version:     u16  = SST_FORMAT_VERSION
//! entry_count: u64
//! entries[entry_count], each:
//!   op:      u8   1 = Put, 2 = Tombstone
//!   key_len: u32
//!   val_len: u32  (0 for Tombstone)
//!   key:     [u8; key_len]
//!   value:   [u8; val_len]   (omitted when op == Tombstone)
//! crc32:       u32  over every byte above (magic..last entry)
//! ```
//!
//! Entries must be sorted ascending by key (the memtable already iterates in
//! that order, and compaction merges preserve it) — phase A has no block
//! index or bloom filter; `store::sst` loads the whole file into memory and
//! binary-searches it.

use std::path::Path;

use super::checksum::crc32;
use crate::error::{EngineError, Result};

pub const SST_MAGIC: u32 = 0x5353_5442; // b"SSTB"
pub const SST_FORMAT_VERSION: u16 = 1;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`super::lock`]). Field list and order must mirror the whole-file byte
/// layout documented above exactly — update both together, never one
/// without the other. `entries[].*` fields describe one repeated entry,
/// not a fixed-count set of fields.
pub fn spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "SstFile",
        version: SST_FORMAT_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("entry_count", "u64"),
            ("entries[].op", "u8"),
            ("entries[].key_len", "u32"),
            ("entries[].val_len", "u32"),
            ("entries[].key", "bytes(key_len)"),
            ("entries[].value", "bytes(val_len)?"),
            ("crc32", "u32"),
        ],
    }
}

const FILE_HEADER_LEN: usize = 4 + 2 + 8;
const CRC_LEN: usize = 4;
const ENTRY_HEADER_LEN: usize = 1 + 4 + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SstOp {
    Put = 1,
    Tombstone = 2,
}

impl SstOp {
    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Put),
            2 => Some(Self::Tombstone),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SstEntry {
    pub key: Vec<u8>,
    /// `None` encodes a tombstone.
    pub value: Option<Vec<u8>>,
}

/// Encodes a full SST file body (header + entries + trailing crc32).
///
/// `entries` must already be sorted ascending by key — this function does
/// not sort, it trusts the caller (both the flushed memtable and the
/// compaction merge already produce sorted input).
pub fn encode(entries: &[SstEntry]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&SST_MAGIC.to_le_bytes());
    buf.extend_from_slice(&SST_FORMAT_VERSION.to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for entry in entries {
        let (op, val_bytes): (SstOp, &[u8]) = match &entry.value {
            Some(v) => (SstOp::Put, v.as_slice()),
            None => (SstOp::Tombstone, &[]),
        };
        buf.push(op as u8);
        buf.extend_from_slice(&(entry.key.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(val_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&entry.key);
        buf.extend_from_slice(val_bytes);
    }
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

/// Decodes a full SST file body previously produced by [`encode`].
pub fn decode(buf: &[u8], path: &Path) -> Result<Vec<SstEntry>> {
    let corrupt = |reason: String| EngineError::CorruptSst {
        path: path.to_path_buf(),
        reason,
    };

    if buf.len() < FILE_HEADER_LEN + CRC_LEN {
        return Err(corrupt("file shorter than fixed header + trailing crc32".to_string()));
    }
    let crc_at = buf.len() - CRC_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != SST_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != SST_FORMAT_VERSION {
        return Err(EngineError::UnsupportedFormatVersion {
            path: path.to_path_buf(),
            expected: SST_FORMAT_VERSION,
            found: version,
        });
    }
    let entry_count = u64::from_le_bytes(buf[6..14].try_into().expect("slice is exactly 8 bytes"));

    // `entry_count` comes straight from the file and must be treated as attacker-controlled:
    // bound it against the smallest an entry could possibly be before trusting it as a
    // `Vec::with_capacity` argument, otherwise a crafted `entry_count = u64::MAX` panics with
    // a capacity overflow instead of surfacing as `CorruptSst`.
    let remaining = crc_at.saturating_sub(FILE_HEADER_LEN);
    let max_possible_entries = remaining / ENTRY_HEADER_LEN;
    if entry_count as u128 > max_possible_entries as u128 {
        return Err(corrupt(format!(
            "entry_count {entry_count} exceeds what the buffer could possibly hold ({max_possible_entries})"
        )));
    }

    let mut pos = FILE_HEADER_LEN;
    let mut entries = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        if pos + ENTRY_HEADER_LEN > crc_at {
            return Err(corrupt("truncated entry header".to_string()));
        }
        let op = SstOp::from_tag(buf[pos]).ok_or_else(|| corrupt("unknown entry op tag".to_string()))?;
        pos += 1;
        let key_len = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes")) as usize;
        pos += 4;
        let val_len = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes")) as usize;
        pos += 4;
        if pos + key_len + val_len > crc_at {
            return Err(corrupt("truncated entry body".to_string()));
        }
        let key = buf[pos..pos + key_len].to_vec();
        pos += key_len;
        let value = match op {
            SstOp::Put => Some(buf[pos..pos + val_len].to_vec()),
            SstOp::Tombstone => None,
        };
        pos += val_len;
        entries.push(SstEntry { key, value });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("test.sst")
    }

    #[test]
    fn roundtrips_mixed_entries() {
        let entries = vec![
            SstEntry {
                key: b"a".to_vec(),
                value: Some(b"1".to_vec()),
            },
            SstEntry {
                key: b"b".to_vec(),
                value: None,
            },
            SstEntry {
                key: b"c".to_vec(),
                value: Some(Vec::new()),
            },
        ];
        let bytes = encode(&entries);
        let decoded = decode(&bytes, &path()).expect("decode ok");
        assert_eq!(decoded, entries);
    }

    #[test]
    fn empty_entries_roundtrip() {
        let bytes = encode(&[]);
        let decoded = decode(&bytes, &path()).expect("decode ok");
        assert!(decoded.is_empty());
    }

    #[test]
    fn corrupt_checksum_is_rejected() {
        let entries = vec![SstEntry {
            key: b"a".to_vec(),
            value: Some(b"1".to_vec()),
        }];
        let mut bytes = encode(&entries);
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let err = decode(&bytes, &path()).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptSst { .. }));
    }

    #[test]
    fn huge_entry_count_is_rejected_not_panicking() {
        // Crafted header claiming u64::MAX entries in an otherwise-empty file, with a
        // correctly-computed trailing crc32 so the checksum gate doesn't short-circuit first.
        let mut buf = Vec::new();
        buf.extend_from_slice(&SST_MAGIC.to_le_bytes());
        buf.extend_from_slice(&SST_FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&u64::MAX.to_le_bytes());
        let crc = crc32(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());

        let err = decode(&buf, &path()).expect_err("bogus entry_count should be rejected");
        assert!(matches!(err, EngineError::CorruptSst { .. }));
    }
}
