// SPDX-License-Identifier: BUSL-1.1
//! On-disk layouts for the block-based SST format (ADR-039). Four persisted
//! types, one file section each, read in this order at open (N8.4):
//! header -> footer -> block index -> bloom filter -> (data blocks, on
//! demand, never all at once — see [`crate::store::sst_block`]'s
//! `BlockSstFile::get`/`entries` for the two read paths):
//!
//! ```text
//! SstHeader        magic, version, sst_id, block_size, entry_count, dim(reserved)
//! Data block 0..N  SstDataBlock, one per on-disk block
//! Block index      SstBlockIndex — one entry per data block
//! Bloom filter     SstBloomFilter — over every key in the file
//! Footer           SstFooter — fixed size, always the last bytes of the file
//! ```
//!
//! This module is **codecs only**: encode/decode + the `format.lock` spec
//! for each type, with the same bounds-before-allocation discipline as
//! [`super::crypto`]. It does not decide how these sections are assembled
//! into one file or sealed under encryption — that is
//! [`crate::store::sst_block`]'s job (the writer/reader) together with
//! [`super::crypto`]'s `EncryptedSstBlock` (per-section AEAD, ADR-039 §3).
//! This module's `encode`/`decode` functions only ever handle *plaintext*
//! section bytes; in an encrypted store, every section except the header is
//! sealed as one `EncryptedSstBlock` envelope around exactly those bytes.
//!
//! `SstHeader` stays plaintext even in an encrypted store: it is the
//! bootstrap record (`sst_id`, needed to compute every other section's AAD)
//! and must stay independently readable before any key-derived state exists.
//!
//! ## `SstDataBlock:1`
//!
//! Entries use the [`SstEntry`]/[`SstOp`] shape defined just below (formerly
//! the whole-file `SstFile:1` format's, before that format was deleted —
//! N8.5), framed as one block among many instead of one whole file.
//! Deliberately **no `first_key`/`last_key`** duplicated in the block
//! (ADR-039 §1): the
//! block index carries routing keys, the block itself only needs to be
//! self-describing enough to rebuild from (its entries already carry their
//! own keys) and to detect a misdirected read (wrong offset from a
//! corrupt/tampered index) via its own magic + trailing crc32.
//!
//! ```text
//! magic:       u32  = SST_DATA_BLOCK_MAGIC
//! version:     u16  = SST_DATA_BLOCK_VERSION
//! entry_count: u32
//! entries[entry_count], each:
//!   op:      u8   1 = Put, 2 = Tombstone
//!   key_len: u32
//!   val_len: u32  (0 for Tombstone)
//!   key:     [u8; key_len]
//!   value:   [u8; val_len]   (omitted when op == Tombstone)
//! crc32:       u32  over every byte above (magic..last entry)
//! ```
//!
//! ## `SstBlockIndex:2`
//!
//! One entry per data block, in the same ascending order as the blocks
//! themselves — a point lookup binary-searches this by `last_key` to find
//! the single block that could contain a key, without reading any block.
//!
//! Bumped to `:2` in N8.4 (ADR-039) to add `tombstone_count` per entry: the
//! optimized reader's `open()` never decodes a data block just to compute
//! [`crate::store::EngineStats`]'s `tombstone_count` gauge (that would
//! defeat the whole point of a lazy, O(metadata) open) — summing this field
//! across the index gives the same total for free, from metadata already
//! resident after open.
//!
//! ```text
//! magic:        u32  = SST_BLOCK_INDEX_MAGIC
//! version:      u16  = SST_BLOCK_INDEX_VERSION
//! block_count:  u32
//! entries[block_count], each:
//!   first_key_len:    u32
//!   first_key:        [u8; first_key_len]
//!   last_key_len:     u32
//!   last_key:         [u8; last_key_len]
//!   offset:           u64   byte offset of this block in the SST file
//!   len:              u32   on-disk length of this block (sealed length, if encrypted)
//!   entry_count:      u32
//!   tombstone_count:  u32   tombstones among this block's entry_count entries
//! crc32:        u32
//! ```
//!
//! ## `SstBloomFilter:1`
//!
//! One filter per SST (ADR-039 §6 — per-block filters only if measured
//! beneficial later). Double hashing (`h1 + i*h2`), the same scheme
//! prototyped and measured in the N8.1 spike (`src/bin/block_spike.rs`).
//!
//! ```text
//! magic:       u32  = SST_BLOOM_MAGIC
//! version:     u16  = SST_BLOOM_VERSION
//! num_bits:    u64
//! num_hashes:  u32
//! bits_len:    u32   == ceil(num_bits / 8), stored explicitly and
//!                     cross-checked (same defense-in-depth as CryptoMeta's
//!                     wrapped_len) rather than only implied by the buffer
//! bits:        [u8; bits_len]
//! crc32:       u32
//! ```
//!
//! ## `SstFooter:1`
//!
//! Fixed size, always the last [`SST_FOOTER_LEN`] bytes of the file — the
//! only section whose location does not require reading anything else
//! first. `footer_magic` is a second, trailing copy of the same magic
//! constant: a cheap "is this even a footer" sanity check a reader can do
//! by comparing the last 4 bytes, before trusting `crc32` or any offset
//! field enough to seek elsewhere.
//!
//! ```text
//! magic:         u32  = SST_FOOTER_MAGIC
//! version:       u16  = SST_FOOTER_VERSION
//! index_offset:  u64
//! index_len:     u32
//! bloom_offset:  u64
//! bloom_len:     u32
//! block_count:   u32
//! crc32:         u32   over every byte above (magic..block_count)
//! footer_magic:  u32  = SST_FOOTER_MAGIC
//! ```

use std::path::Path;

use super::checksum::crc32;
use crate::error::{EngineError, Result};

// ── entry types (formerly `format::sst::{SstEntry, SstOp}`, N8.5) ──────────
//
// Moved here when the whole-file `SstFile:1` format (and its `format::sst`
// module) was deleted (ADR-039 §5.3/N8.5, "no dual-format transition") —
// `SstDataBlock` is the only remaining consumer of this in-memory entry
// shape, so it lives next to the codec that uses it.

/// One decoded (or about-to-be-encoded) key/value pair inside a data block.
/// `value: None` encodes a tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SstEntry {
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SstOp {
    Put = 1,
    Tombstone = 2,
}

impl SstOp {
    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Put),
            2 => Some(Self::Tombstone),
            _ => None,
        }
    }
}

// ── SstHeader:2 ──────────────────────────────────────────────────────────

pub const SST_HEADER_MAGIC: u32 = 0x5342_4844; // b"DHBS" (LE bytes: "SBHD")
pub const SST_HEADER_VERSION: u16 = 2;

const SST_HEADER_LEN: usize = 4 + 2 + 8 + 4 + 8 + 4; // magic..dim
/// Fixed on-disk size of the header — always the first
/// [`SST_HEADER_TOTAL_LEN`] bytes of an SST file. `pub(crate)` (not just
/// `const`) so `store::sst_block`'s writer/reader can compute absolute
/// data-block offsets without duplicating this arithmetic.
pub(crate) const SST_HEADER_TOTAL_LEN: usize = SST_HEADER_LEN + 4; // + crc32

/// Decoded `SstHeader` — the bootstrap record every SST file starts with.
/// Always plaintext, even in an encrypted store (see module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SstHeader {
    pub sst_id: u64,
    pub block_size: u32,
    pub entry_count: u64,
    /// Reserved for a future per-dimension SST variant. Always `0` from
    /// this build's writer; `decode` carries the value through unvalidated
    /// (a genuinely reserved field, not yet a documented invariant).
    pub dim: u32,
}

pub fn sst_header_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "SstHeader",
        version: SST_HEADER_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("sst_id", "u64"),
            ("block_size", "u32"),
            ("entry_count", "u64"),
            ("dim", "u32"),
            ("crc32", "u32"),
        ],
    }
}

#[must_use]
pub fn encode_sst_header(header: &SstHeader) -> Vec<u8> {
    let mut buf = Vec::with_capacity(SST_HEADER_TOTAL_LEN);
    buf.extend_from_slice(&SST_HEADER_MAGIC.to_le_bytes());
    buf.extend_from_slice(&SST_HEADER_VERSION.to_le_bytes());
    buf.extend_from_slice(&header.sst_id.to_le_bytes());
    buf.extend_from_slice(&header.block_size.to_le_bytes());
    buf.extend_from_slice(&header.entry_count.to_le_bytes());
    buf.extend_from_slice(&header.dim.to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

pub fn decode_sst_header(buf: &[u8], path: &Path) -> Result<SstHeader> {
    let corrupt = |reason: String| EngineError::CorruptSstHeader {
        path: path.to_path_buf(),
        reason,
    };

    if buf.len() != SST_HEADER_TOTAL_LEN {
        return Err(corrupt(format!(
            "header must be exactly {SST_HEADER_TOTAL_LEN} bytes, got {}",
            buf.len()
        )));
    }
    let crc_at = SST_HEADER_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != SST_HEADER_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != SST_HEADER_VERSION {
        return Err(EngineError::UnsupportedSstHeaderVersion {
            path: path.to_path_buf(),
            expected: SST_HEADER_VERSION,
            found: version,
        });
    }
    let sst_id = u64::from_le_bytes(buf[6..14].try_into().expect("slice is exactly 8 bytes"));
    let block_size = u32::from_le_bytes(buf[14..18].try_into().expect("slice is exactly 4 bytes"));
    if block_size == 0 {
        return Err(corrupt("block_size must be nonzero".to_string()));
    }
    let entry_count = u64::from_le_bytes(buf[18..26].try_into().expect("slice is exactly 8 bytes"));
    let dim = u32::from_le_bytes(buf[26..30].try_into().expect("slice is exactly 4 bytes"));

    Ok(SstHeader {
        sst_id,
        block_size,
        entry_count,
        dim,
    })
}

// ── SstDataBlock:1 ───────────────────────────────────────────────────────

pub const SST_DATA_BLOCK_MAGIC: u32 = 0x5342_4C4B; // "KLBS" LE
pub const SST_DATA_BLOCK_VERSION: u16 = 1;

const BLOCK_HEADER_LEN: usize = 4 + 2 + 4; // magic, version, entry_count
const BLOCK_ENTRY_HEADER_LEN: usize = 1 + 4 + 4; // op, key_len, val_len
const CRC_LEN: usize = 4;

pub fn sst_data_block_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "SstDataBlock",
        version: SST_DATA_BLOCK_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("entry_count", "u32"),
            ("entries[].op", "u8"),
            ("entries[].key_len", "u32"),
            ("entries[].val_len", "u32"),
            ("entries[].key", "bytes(key_len)"),
            ("entries[].value", "bytes(val_len)?"),
            ("crc32", "u32"),
        ],
    }
}

/// Encodes one data block. `entries` must already be sorted ascending by
/// key (the writer's job, N8.3) — this function trusts the caller, same
/// contract as [`super::sst::encode`].
#[must_use]
pub fn encode_sst_data_block(entries: &[SstEntry]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&SST_DATA_BLOCK_MAGIC.to_le_bytes());
    buf.extend_from_slice(&SST_DATA_BLOCK_VERSION.to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
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

/// Decodes one data block previously produced by [`encode_sst_data_block`].
/// Every length is bounded against the real buffer before it drives an
/// allocation or a slice (N2/N3 fuzzing discipline, same as
/// [`super::sst::decode`]).
pub fn decode_sst_data_block(buf: &[u8], path: &Path) -> Result<Vec<SstEntry>> {
    let corrupt = |reason: String| EngineError::CorruptSstDataBlock {
        path: path.to_path_buf(),
        reason,
    };

    if buf.len() < BLOCK_HEADER_LEN + CRC_LEN {
        return Err(corrupt("block shorter than fixed header + trailing crc32".to_string()));
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
    if magic != SST_DATA_BLOCK_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != SST_DATA_BLOCK_VERSION {
        return Err(EngineError::UnsupportedSstDataBlockVersion {
            path: path.to_path_buf(),
            expected: SST_DATA_BLOCK_VERSION,
            found: version,
        });
    }
    let entry_count = u32::from_le_bytes(buf[6..10].try_into().expect("slice is exactly 4 bytes"));

    let remaining = crc_at.saturating_sub(BLOCK_HEADER_LEN);
    let max_possible_entries = remaining / BLOCK_ENTRY_HEADER_LEN;
    if entry_count as usize > max_possible_entries {
        return Err(corrupt(format!(
            "entry_count {entry_count} exceeds what the block could possibly hold ({max_possible_entries})"
        )));
    }

    let mut pos = BLOCK_HEADER_LEN;
    let mut entries = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        if pos + BLOCK_ENTRY_HEADER_LEN > crc_at {
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
    if pos != crc_at {
        return Err(corrupt(format!(
            "trailing bytes after declared entries ({} bytes)",
            crc_at - pos
        )));
    }
    Ok(entries)
}

// ── SstBlockIndex:1 ──────────────────────────────────────────────────────

pub const SST_BLOCK_INDEX_MAGIC: u32 = 0x5342_4958; // "XIBS" LE
pub const SST_BLOCK_INDEX_VERSION: u16 = 2;

const INDEX_HEADER_LEN: usize = 4 + 2 + 4; // magic, version, block_count
/// Minimum bytes one index entry can possibly occupy, excluding its two
/// variable-length keys — the bound used to reject a lying `block_count`
/// before it drives an allocation.
const INDEX_ENTRY_MIN_LEN: usize = 4 + 4 + 8 + 4 + 4 + 4; // first_key_len, last_key_len, offset, len, entry_count, tombstone_count

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SstBlockIndexEntry {
    pub first_key: Vec<u8>,
    pub last_key: Vec<u8>,
    pub offset: u64,
    pub len: u32,
    pub entry_count: u32,
    /// Tombstones among this block's `entry_count` entries — see the module
    /// doc's `SstBlockIndex:2` section for why this lives here instead of
    /// being recomputed by decoding the block.
    pub tombstone_count: u32,
}

pub fn sst_block_index_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "SstBlockIndex",
        version: SST_BLOCK_INDEX_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("block_count", "u32"),
            ("entries[].first_key_len", "u32"),
            ("entries[].first_key", "bytes(first_key_len)"),
            ("entries[].last_key_len", "u32"),
            ("entries[].last_key", "bytes(last_key_len)"),
            ("entries[].offset", "u64"),
            ("entries[].len", "u32"),
            ("entries[].entry_count", "u32"),
            ("entries[].tombstone_count", "u32"),
            ("crc32", "u32"),
        ],
    }
}

#[must_use]
pub fn encode_sst_block_index(entries: &[SstBlockIndexEntry]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&SST_BLOCK_INDEX_MAGIC.to_le_bytes());
    buf.extend_from_slice(&SST_BLOCK_INDEX_VERSION.to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        buf.extend_from_slice(&(entry.first_key.len() as u32).to_le_bytes());
        buf.extend_from_slice(&entry.first_key);
        buf.extend_from_slice(&(entry.last_key.len() as u32).to_le_bytes());
        buf.extend_from_slice(&entry.last_key);
        buf.extend_from_slice(&entry.offset.to_le_bytes());
        buf.extend_from_slice(&entry.len.to_le_bytes());
        buf.extend_from_slice(&entry.entry_count.to_le_bytes());
        buf.extend_from_slice(&entry.tombstone_count.to_le_bytes());
    }
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

pub fn decode_sst_block_index(buf: &[u8], path: &Path) -> Result<Vec<SstBlockIndexEntry>> {
    let corrupt = |reason: String| EngineError::CorruptSstBlockIndex {
        path: path.to_path_buf(),
        reason,
    };

    if buf.len() < INDEX_HEADER_LEN + CRC_LEN {
        return Err(corrupt("index shorter than fixed header + trailing crc32".to_string()));
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
    if magic != SST_BLOCK_INDEX_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != SST_BLOCK_INDEX_VERSION {
        return Err(EngineError::UnsupportedSstBlockIndexVersion {
            path: path.to_path_buf(),
            expected: SST_BLOCK_INDEX_VERSION,
            found: version,
        });
    }
    let block_count = u32::from_le_bytes(buf[6..10].try_into().expect("slice is exactly 4 bytes"));

    let remaining = crc_at.saturating_sub(INDEX_HEADER_LEN);
    let max_possible_entries = remaining / INDEX_ENTRY_MIN_LEN;
    if block_count as usize > max_possible_entries {
        return Err(corrupt(format!(
            "block_count {block_count} exceeds what the index could possibly hold ({max_possible_entries})"
        )));
    }

    let mut pos = INDEX_HEADER_LEN;
    let mut entries = Vec::with_capacity(block_count as usize);
    for _ in 0..block_count {
        if pos + 4 > crc_at {
            return Err(corrupt("truncated first_key_len".to_string()));
        }
        let first_key_len =
            u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes")) as usize;
        pos += 4;
        if pos + first_key_len > crc_at {
            return Err(corrupt("truncated first_key".to_string()));
        }
        let first_key = buf[pos..pos + first_key_len].to_vec();
        pos += first_key_len;

        if pos + 4 > crc_at {
            return Err(corrupt("truncated last_key_len".to_string()));
        }
        let last_key_len = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes")) as usize;
        pos += 4;
        if pos + last_key_len > crc_at {
            return Err(corrupt("truncated last_key".to_string()));
        }
        let last_key = buf[pos..pos + last_key_len].to_vec();
        pos += last_key_len;

        if pos + 20 > crc_at {
            return Err(corrupt("truncated offset/len/entry_count/tombstone_count".to_string()));
        }
        let offset = u64::from_le_bytes(buf[pos..pos + 8].try_into().expect("slice is exactly 8 bytes"));
        pos += 8;
        let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes"));
        pos += 4;
        let entry_count = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes"));
        pos += 4;
        let tombstone_count = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes"));
        pos += 4;

        entries.push(SstBlockIndexEntry {
            first_key,
            last_key,
            offset,
            len,
            entry_count,
            tombstone_count,
        });
    }
    if pos != crc_at {
        return Err(corrupt(format!(
            "trailing bytes after declared entries ({} bytes)",
            crc_at - pos
        )));
    }
    Ok(entries)
}

// ── SstBloomFilter:1 ─────────────────────────────────────────────────────

pub const SST_BLOOM_MAGIC: u32 = 0x5342_4C4D; // "MLBS" LE
pub const SST_BLOOM_VERSION: u16 = 1;

const BLOOM_HEADER_LEN: usize = 4 + 2 + 8 + 4 + 4; // magic, version, num_bits, num_hashes, bits_len

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SstBloomFilter {
    pub num_bits: u64,
    pub num_hashes: u32,
    pub bits: Vec<u8>,
}

pub fn sst_bloom_filter_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "SstBloomFilter",
        version: SST_BLOOM_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("num_bits", "u64"),
            ("num_hashes", "u32"),
            ("bits_len", "u32"),
            ("bits", "bytes(bits_len)"),
            ("crc32", "u32"),
        ],
    }
}

#[must_use]
pub fn encode_sst_bloom_filter(filter: &SstBloomFilter) -> Vec<u8> {
    let mut buf = Vec::with_capacity(BLOOM_HEADER_LEN + filter.bits.len() + CRC_LEN);
    buf.extend_from_slice(&SST_BLOOM_MAGIC.to_le_bytes());
    buf.extend_from_slice(&SST_BLOOM_VERSION.to_le_bytes());
    buf.extend_from_slice(&filter.num_bits.to_le_bytes());
    buf.extend_from_slice(&filter.num_hashes.to_le_bytes());
    buf.extend_from_slice(&(filter.bits.len() as u32).to_le_bytes());
    buf.extend_from_slice(&filter.bits);
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

pub fn decode_sst_bloom_filter(buf: &[u8], path: &Path) -> Result<SstBloomFilter> {
    let corrupt = |reason: String| EngineError::CorruptSstBloomFilter {
        path: path.to_path_buf(),
        reason,
    };

    if buf.len() < BLOOM_HEADER_LEN + CRC_LEN {
        return Err(corrupt(
            "bloom filter shorter than fixed header + trailing crc32".to_string(),
        ));
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
    if magic != SST_BLOOM_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != SST_BLOOM_VERSION {
        return Err(EngineError::UnsupportedSstBloomFilterVersion {
            path: path.to_path_buf(),
            expected: SST_BLOOM_VERSION,
            found: version,
        });
    }
    let num_bits = u64::from_le_bytes(buf[6..14].try_into().expect("slice is exactly 8 bytes"));
    let num_hashes = u32::from_le_bytes(buf[14..18].try_into().expect("slice is exactly 4 bytes"));
    let bits_len = u32::from_le_bytes(buf[18..22].try_into().expect("slice is exactly 4 bytes")) as usize;

    let expected_bits_len = num_bits.div_ceil(8);
    if bits_len as u64 != expected_bits_len {
        return Err(corrupt(format!(
            "bits_len {bits_len} does not match ceil(num_bits / 8) = {expected_bits_len}"
        )));
    }
    if bits_len != crc_at - BLOOM_HEADER_LEN {
        return Err(corrupt(format!(
            "bits_len {bits_len} does not match the {} bytes actually present",
            crc_at - BLOOM_HEADER_LEN
        )));
    }
    let bits = buf[BLOOM_HEADER_LEN..crc_at].to_vec();

    Ok(SstBloomFilter {
        num_bits,
        num_hashes,
        bits,
    })
}

// ── SstFooter:1 ──────────────────────────────────────────────────────────

pub const SST_FOOTER_MAGIC: u32 = 0x5342_4654; // "TFBS" LE
pub const SST_FOOTER_VERSION: u16 = 1;

/// Fixed on-disk size of the footer — always the last `SST_FOOTER_LEN`
/// bytes of an SST file, so a reader can locate it with one `seek` to
/// `file_len - SST_FOOTER_LEN`, no other section read first.
pub const SST_FOOTER_LEN: usize = 4 + 2 + 8 + 4 + 8 + 4 + 4 + 4 + 4; // magic..crc32..footer_magic

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SstFooter {
    pub index_offset: u64,
    pub index_len: u32,
    pub bloom_offset: u64,
    pub bloom_len: u32,
    pub block_count: u32,
}

pub fn sst_footer_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "SstFooter",
        version: SST_FOOTER_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("index_offset", "u64"),
            ("index_len", "u32"),
            ("bloom_offset", "u64"),
            ("bloom_len", "u32"),
            ("block_count", "u32"),
            ("crc32", "u32"),
            ("footer_magic", "u32"),
        ],
    }
}

#[must_use]
pub fn encode_sst_footer(footer: &SstFooter) -> Vec<u8> {
    let mut buf = Vec::with_capacity(SST_FOOTER_LEN);
    buf.extend_from_slice(&SST_FOOTER_MAGIC.to_le_bytes());
    buf.extend_from_slice(&SST_FOOTER_VERSION.to_le_bytes());
    buf.extend_from_slice(&footer.index_offset.to_le_bytes());
    buf.extend_from_slice(&footer.index_len.to_le_bytes());
    buf.extend_from_slice(&footer.bloom_offset.to_le_bytes());
    buf.extend_from_slice(&footer.bloom_len.to_le_bytes());
    buf.extend_from_slice(&footer.block_count.to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf.extend_from_slice(&SST_FOOTER_MAGIC.to_le_bytes());
    debug_assert_eq!(buf.len(), SST_FOOTER_LEN);
    buf
}

pub fn decode_sst_footer(buf: &[u8], path: &Path) -> Result<SstFooter> {
    let corrupt = |reason: String| EngineError::CorruptSstFooter {
        path: path.to_path_buf(),
        reason,
    };

    if buf.len() != SST_FOOTER_LEN {
        return Err(corrupt(format!(
            "footer must be exactly {SST_FOOTER_LEN} bytes, got {}",
            buf.len()
        )));
    }
    let footer_magic_at = SST_FOOTER_LEN - 4;
    let footer_magic = u32::from_le_bytes(buf[footer_magic_at..].try_into().expect("slice is exactly 4 bytes"));
    if footer_magic != SST_FOOTER_MAGIC {
        return Err(corrupt("bad trailing footer_magic".to_string()));
    }

    let crc_at = footer_magic_at - 4;
    let expected_crc = u32::from_le_bytes(
        buf[crc_at..footer_magic_at]
            .try_into()
            .expect("slice is exactly 4 bytes"),
    );
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != SST_FOOTER_MAGIC {
        return Err(corrupt("bad leading magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != SST_FOOTER_VERSION {
        return Err(EngineError::UnsupportedSstFooterVersion {
            path: path.to_path_buf(),
            expected: SST_FOOTER_VERSION,
            found: version,
        });
    }
    let index_offset = u64::from_le_bytes(buf[6..14].try_into().expect("slice is exactly 8 bytes"));
    let index_len = u32::from_le_bytes(buf[14..18].try_into().expect("slice is exactly 4 bytes"));
    let bloom_offset = u64::from_le_bytes(buf[18..26].try_into().expect("slice is exactly 8 bytes"));
    let bloom_len = u32::from_le_bytes(buf[26..30].try_into().expect("slice is exactly 4 bytes"));
    let block_count = u32::from_le_bytes(buf[30..34].try_into().expect("slice is exactly 4 bytes"));

    Ok(SstFooter {
        index_offset,
        index_len,
        bloom_offset,
        bloom_len,
        block_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("test.sst")
    }

    // ── SstHeader ──

    fn sample_header() -> SstHeader {
        SstHeader {
            sst_id: 42,
            block_size: 16 * 1024,
            entry_count: 12_345,
            dim: 0,
        }
    }

    #[test]
    fn header_roundtrips() {
        let header = sample_header();
        let bytes = encode_sst_header(&header);
        let decoded = decode_sst_header(&bytes, &path()).expect("decode ok");
        assert_eq!(decoded, header);
    }

    #[test]
    fn header_bit_flip_is_corrupt_error() {
        let mut bytes = encode_sst_header(&sample_header());
        bytes[10] ^= 0xFF;
        let err = decode_sst_header(&bytes, &path()).expect_err("bit flip should fail");
        assert!(matches!(err, EngineError::CorruptSstHeader { .. }));
    }

    #[test]
    fn header_truncation_is_corrupt_error_at_every_cut() {
        let bytes = encode_sst_header(&sample_header());
        for cut in 0..bytes.len() {
            let err = decode_sst_header(&bytes[..cut], &path()).expect_err("truncated header is corrupt");
            assert!(matches!(err, EngineError::CorruptSstHeader { .. }), "cut={cut}: {err}");
        }
    }

    #[test]
    fn header_rejects_zero_block_size() {
        let mut header = sample_header();
        header.block_size = 0;
        let bytes = encode_sst_header(&header);
        let err = decode_sst_header(&bytes, &path()).expect_err("zero block_size is corrupt");
        assert!(matches!(err, EngineError::CorruptSstHeader { .. }));
    }

    #[test]
    fn header_wrong_version_is_unsupported() {
        let mut bytes = encode_sst_header(&sample_header());
        bytes[4..6].copy_from_slice(&99u16.to_le_bytes());
        let crc_at = SST_HEADER_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode_sst_header(&bytes, &path()).expect_err("wrong version is unsupported");
        assert!(matches!(err, EngineError::UnsupportedSstHeaderVersion { .. }));
    }

    // ── SstDataBlock ──

    fn sample_entries() -> Vec<SstEntry> {
        vec![
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
        ]
    }

    #[test]
    fn data_block_roundtrips() {
        let entries = sample_entries();
        let bytes = encode_sst_data_block(&entries);
        let decoded = decode_sst_data_block(&bytes, &path()).expect("decode ok");
        assert_eq!(decoded, entries);
    }

    #[test]
    fn data_block_empty_roundtrips() {
        let bytes = encode_sst_data_block(&[]);
        let decoded = decode_sst_data_block(&bytes, &path()).expect("decode ok");
        assert!(decoded.is_empty());
    }

    #[test]
    fn data_block_bit_flip_is_corrupt_error() {
        let mut bytes = encode_sst_data_block(&sample_entries());
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let err = decode_sst_data_block(&bytes, &path()).expect_err("bit flip should fail");
        assert!(matches!(err, EngineError::CorruptSstDataBlock { .. }));
    }

    #[test]
    fn data_block_truncation_is_corrupt_error_at_every_cut() {
        let bytes = encode_sst_data_block(&sample_entries());
        for cut in 0..bytes.len() {
            let err = decode_sst_data_block(&bytes[..cut], &path()).expect_err("truncated block is corrupt");
            assert!(
                matches!(err, EngineError::CorruptSstDataBlock { .. }),
                "cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn data_block_huge_entry_count_is_rejected_not_panicking() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&SST_DATA_BLOCK_MAGIC.to_le_bytes());
        buf.extend_from_slice(&SST_DATA_BLOCK_VERSION.to_le_bytes());
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        let crc = crc32(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());

        let err = decode_sst_data_block(&buf, &path()).expect_err("bogus entry_count should be rejected");
        assert!(matches!(err, EngineError::CorruptSstDataBlock { .. }));
    }

    // ── SstBlockIndex ──

    fn sample_index_entries() -> Vec<SstBlockIndexEntry> {
        vec![
            SstBlockIndexEntry {
                first_key: b"a".to_vec(),
                last_key: b"m".to_vec(),
                offset: 0,
                len: 4096,
                entry_count: 50,
                tombstone_count: 3,
            },
            SstBlockIndexEntry {
                first_key: b"n".to_vec(),
                last_key: b"z".to_vec(),
                offset: 4096,
                len: 3800,
                entry_count: 48,
                tombstone_count: 0,
            },
        ]
    }

    #[test]
    fn block_index_roundtrips() {
        let entries = sample_index_entries();
        let bytes = encode_sst_block_index(&entries);
        let decoded = decode_sst_block_index(&bytes, &path()).expect("decode ok");
        assert_eq!(decoded, entries);
    }

    #[test]
    fn block_index_empty_roundtrips() {
        let bytes = encode_sst_block_index(&[]);
        let decoded = decode_sst_block_index(&bytes, &path()).expect("decode ok");
        assert!(decoded.is_empty());
    }

    #[test]
    fn block_index_bit_flip_is_corrupt_error() {
        let mut bytes = encode_sst_block_index(&sample_index_entries());
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let err = decode_sst_block_index(&bytes, &path()).expect_err("bit flip should fail");
        assert!(matches!(err, EngineError::CorruptSstBlockIndex { .. }));
    }

    #[test]
    fn block_index_truncation_is_corrupt_error_at_every_cut() {
        let bytes = encode_sst_block_index(&sample_index_entries());
        for cut in 0..bytes.len() {
            let err = decode_sst_block_index(&bytes[..cut], &path()).expect_err("truncated index is corrupt");
            assert!(
                matches!(err, EngineError::CorruptSstBlockIndex { .. }),
                "cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn block_index_huge_block_count_is_rejected_not_panicking() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&SST_BLOCK_INDEX_MAGIC.to_le_bytes());
        buf.extend_from_slice(&SST_BLOCK_INDEX_VERSION.to_le_bytes());
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        let crc = crc32(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());

        let err = decode_sst_block_index(&buf, &path()).expect_err("bogus block_count should be rejected");
        assert!(matches!(err, EngineError::CorruptSstBlockIndex { .. }));
    }

    // ── SstBloomFilter ──

    fn sample_bloom() -> SstBloomFilter {
        SstBloomFilter {
            num_bits: 64,
            num_hashes: 7,
            bits: vec![0xAB; 8],
        }
    }

    #[test]
    fn bloom_filter_roundtrips() {
        let filter = sample_bloom();
        let bytes = encode_sst_bloom_filter(&filter);
        let decoded = decode_sst_bloom_filter(&bytes, &path()).expect("decode ok");
        assert_eq!(decoded, filter);
    }

    #[test]
    fn bloom_filter_bit_flip_is_corrupt_error() {
        let mut bytes = encode_sst_bloom_filter(&sample_bloom());
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let err = decode_sst_bloom_filter(&bytes, &path()).expect_err("bit flip should fail");
        assert!(matches!(err, EngineError::CorruptSstBloomFilter { .. }));
    }

    #[test]
    fn bloom_filter_truncation_is_corrupt_error_at_every_cut() {
        let bytes = encode_sst_bloom_filter(&sample_bloom());
        for cut in 0..bytes.len() {
            let err = decode_sst_bloom_filter(&bytes[..cut], &path()).expect_err("truncated bloom is corrupt");
            assert!(
                matches!(err, EngineError::CorruptSstBloomFilter { .. }),
                "cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bloom_filter_lying_num_bits_is_corrupt_error() {
        // num_bits inconsistent with the actual bits length present, crc
        // recomputed so the checksum gate doesn't short-circuit first.
        let mut bytes = encode_sst_bloom_filter(&sample_bloom());
        bytes[6..14].copy_from_slice(&999u64.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode_sst_bloom_filter(&bytes, &path()).expect_err("lying num_bits is corrupt");
        assert!(matches!(err, EngineError::CorruptSstBloomFilter { .. }));
    }

    #[test]
    fn bloom_filter_lying_bits_len_is_rejected_not_panicking() {
        let mut bytes = encode_sst_bloom_filter(&sample_bloom());
        let bits_len_at = 18;
        bytes[bits_len_at..bits_len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode_sst_bloom_filter(&bytes, &path()).expect_err("lying bits_len should be rejected");
        assert!(matches!(err, EngineError::CorruptSstBloomFilter { .. }));
    }

    // ── SstFooter ──

    fn sample_footer() -> SstFooter {
        SstFooter {
            index_offset: 1_048_576,
            index_len: 4096,
            bloom_offset: 1_052_672,
            bloom_len: 512,
            block_count: 64,
        }
    }

    #[test]
    fn footer_roundtrips() {
        let footer = sample_footer();
        let bytes = encode_sst_footer(&footer);
        assert_eq!(bytes.len(), SST_FOOTER_LEN);
        let decoded = decode_sst_footer(&bytes, &path()).expect("decode ok");
        assert_eq!(decoded, footer);
    }

    #[test]
    fn footer_bit_flip_is_corrupt_error() {
        let mut bytes = encode_sst_footer(&sample_footer());
        bytes[10] ^= 0xFF;
        let err = decode_sst_footer(&bytes, &path()).expect_err("bit flip should fail");
        assert!(matches!(err, EngineError::CorruptSstFooter { .. }));
    }

    #[test]
    fn footer_truncation_is_corrupt_error_at_every_cut() {
        let bytes = encode_sst_footer(&sample_footer());
        for cut in 0..bytes.len() {
            let err = decode_sst_footer(&bytes[..cut], &path()).expect_err("truncated footer is corrupt");
            assert!(matches!(err, EngineError::CorruptSstFooter { .. }), "cut={cut}: {err}");
        }
    }

    #[test]
    fn footer_bad_trailing_magic_is_corrupt_error() {
        let mut bytes = encode_sst_footer(&sample_footer());
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let err = decode_sst_footer(&bytes, &path()).expect_err("bad trailing magic should fail");
        assert!(matches!(err, EngineError::CorruptSstFooter { .. }));
    }

    #[test]
    fn footer_wrong_version_is_unsupported() {
        let mut bytes = encode_sst_footer(&sample_footer());
        bytes[4..6].copy_from_slice(&99u16.to_le_bytes());
        let crc_at = SST_FOOTER_LEN - 8;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());
        let err = decode_sst_footer(&bytes, &path()).expect_err("wrong version is unsupported");
        assert!(matches!(err, EngineError::UnsupportedSstFooterVersion { .. }));
    }
}
