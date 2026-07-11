// SPDX-License-Identifier: BUSL-1.1
//! Block-based SST writer + optimized reader (ADR-039, N8.3/N8.4/N8.5/N8.8):
//! the sole SST implementation [`crate::store::Engine`] uses — the whole-file
//! `SstFile` this replaced (ADR-025 Phase A) is deleted, per ADR-039 §5.3's
//! "no dual-format transition, no migration" policy.
//!
//! Layout assembled here from the `format::sst_block` codecs: header, data
//! blocks, block index, bloom filter, footer — see that module's doc for the
//! exact wire layout of each section.
//!
//! ## Lazy open (N8.4)
//!
//! [`BlockSstFile::load`] reads **only** the header, footer, block index and
//! bloom filter — never a data block. That is O(metadata), independent of
//! how much payload the file holds (ADR-039 §8.1's exit criterion: RSS/open
//! cost bounded by metadata + cache, never proportional to data). Every data
//! block is read on demand, through exactly two paths:
//!
//! - [`BlockSstFile::get`] — a point lookup: bloom filter (zero I/O on a
//!   miss) -> binary search the block index by `last_key` -> read the
//!   *single* candidate block -> binary search within it. Structurally
//!   cannot touch more than one data block; see `store::engine::Engine::get`
//!   for how this feeds the `point_lookup_full_sst_read` invariant counter
//!   (ADR-039 §4/§5.5).
//! - [`BlockSstFile::entries`] — a full scan, every block in order. The
//!   *only* legitimate caller of a full walk: compaction and prefix scans,
//!   which by nature must see every key. `get` never calls this.
//!
//! ## Per-block AEAD (N8.8)
//!
//! With a [`CryptoContext`] supplied, every section except the header is
//! sealed individually as one `EncryptedSstBlock` envelope
//! ([`crate::format::crypto`]) bound by AAD to `(sst_id, section_type,
//! section_no)` — moving a block between two SSTs or reordering it within
//! one fails Poly1305 authentication even though the bytes are individually
//! intact (ADR-039 §3). The header always stays plaintext: it is the
//! bootstrap record every other section's AAD needs `sst_id` from, and the
//! sealed footer's on-disk length is fixed (its plaintext length is fixed),
//! so the reader can still locate it with one seek from EOF.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::crypto::CryptoContext;
use crate::error::{EngineError, Result};
use crate::fail_point;
use crate::format::crypto::{self as envelope, SstSectionType};
use crate::format::sst_block::{
    self, SST_FOOTER_LEN, SST_HEADER_TOTAL_LEN, SstBlockIndexEntry, SstBloomFilter, SstEntry, SstFooter, SstHeader,
};
use crate::key::Key;
use crate::store::Value;
use crate::store::block_cache::BlockCache;

// ── bloom (double hashing h1 + i*h2, ADR-039 §6) ────────────────────────
//
// Wraps `format::sst_block::SstBloomFilter` (the wire bytes) with the
// actual hash-key-into-bits algorithm — home-grown, zero-dep, and the same
// scheme measured in the N8.1 spike (`src/bin/block_spike.rs`). The hash
// function itself is part of what `SstBloomFilter:1` freezes (ADR-039 §6):
// changing it without a version bump would make an old filter's bits
// meaningless to a new build's `contains`, silently turning "maybe
// present" into wrong answers.

const BLOOM_BITS_PER_KEY: u32 = 10; // ~1% false-positive rate, N8.1 spike
const BLOOM_NUM_HASHES: u64 = 7; // N8.1 spike

struct Bloom {
    bits: Vec<u8>,
    num_bits: u64,
}

impl Bloom {
    /// Sizes the bit array for `expected_keys` at [`BLOOM_BITS_PER_KEY`]
    /// bits/key, rounded up to a whole byte and floored at 64 bits so an
    /// empty SST still produces a well-formed (if useless) filter.
    fn new(expected_keys: usize) -> Self {
        let num_bits = (expected_keys as u64 * u64::from(BLOOM_BITS_PER_KEY)).max(64);
        let num_bytes = num_bits.div_ceil(8);
        Self {
            bits: vec![0u8; num_bytes as usize],
            num_bits: num_bytes * 8,
        }
    }

    fn hashes(&self, key: &[u8]) -> (u64, u64) {
        let mut h1 = DefaultHasher::new();
        key.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        0xB10C_5EED_u64.hash(&mut h2);
        key.hash(&mut h2);
        (h1.finish(), h2.finish() | 1)
    }

    fn insert(&mut self, key: &[u8]) {
        let (h1, h2) = self.hashes(key);
        for i in 0..BLOOM_NUM_HASHES {
            let bit = h1.wrapping_add(i.wrapping_mul(h2)) % self.num_bits;
            self.bits[(bit / 8) as usize] |= 1 << (bit % 8);
        }
    }

    fn contains(&self, key: &[u8]) -> bool {
        let (h1, h2) = self.hashes(key);
        (0..BLOOM_NUM_HASHES).all(|i| {
            let bit = h1.wrapping_add(i.wrapping_mul(h2)) % self.num_bits;
            self.bits[(bit / 8) as usize] & (1 << (bit % 8)) != 0
        })
    }

    /// Wire-format snapshot of the current bits — non-consuming (the writer
    /// keeps `self` afterward, as the freshly-written file's resident
    /// filter).
    fn to_filter(&self) -> SstBloomFilter {
        SstBloomFilter {
            num_bits: self.num_bits,
            num_hashes: BLOOM_NUM_HASHES as u32,
            bits: self.bits.clone(),
        }
    }

    fn from_filter(filter: SstBloomFilter) -> Self {
        Self {
            bits: filter.bits,
            num_bits: filter.num_bits,
        }
    }
}

// ── per-section AEAD helpers (N8.8) ─────────────────────────────────────

/// Seals `plain` as one section if `crypto` is `Some`, else passes it
/// through unchanged. `section`/`section_no` become part of the AAD
/// (ADR-039 §3) — the caller must pass the same coordinates it will read
/// back with, or [`open_section`] will (correctly) fail authentication.
fn seal_section(
    crypto: Option<&CryptoContext>,
    plain: Vec<u8>,
    sst_id: u64,
    section: SstSectionType,
    section_no: u32,
) -> Result<Vec<u8>> {
    match crypto {
        None => Ok(plain),
        Some(crypto) => {
            let aad = envelope::encrypted_sst_block_aad(sst_id, section, section_no);
            let sealed = crypto.seal(&plain, &aad)?;
            Ok(envelope::encode_encrypted_sst_block(&sealed.nonce, &sealed.ciphertext))
        }
    }
}

/// Inverse of [`seal_section`]: unseals `bytes` if `crypto` is `Some` (an
/// AEAD failure is [`EngineError::CorruptEncryptedSstBlock`] — by the time a
/// section is opened, the key has already been verified against
/// `crypto.meta`, so a failed tag means tampering or corruption, never a
/// wrong key), else returns `bytes` as plaintext unchanged.
fn open_section(
    crypto: Option<&CryptoContext>,
    bytes: &[u8],
    sst_id: u64,
    section: SstSectionType,
    section_no: u32,
    path: &Path,
) -> Result<Vec<u8>> {
    match crypto {
        None => Ok(bytes.to_vec()),
        Some(crypto) => {
            let (nonce, ciphertext) = envelope::decode_encrypted_sst_block(bytes, path)?;
            let aad = envelope::encrypted_sst_block_aad(sst_id, section, section_no);
            crypto
                .open(&nonce, ciphertext, &aad)
                .ok_or_else(|| EngineError::CorruptEncryptedSstBlock {
                    path: path.to_path_buf(),
                    reason: "envelope failed AEAD authentication (tampered or corrupt)".to_string(),
                })
        }
    }
}

// ── writer + optimized reader ───────────────────────────────────────────

pub(crate) struct BlockSstFile {
    pub(crate) id: u64,
    pub(crate) path: PathBuf,
    /// On-disk file length (every section included, sealed lengths when
    /// encrypted) — feeds the `sst_bytes` gauge and compaction byte
    /// counters of [`crate::EngineStats`] without re-statting files.
    pub(crate) file_bytes: u64,
    /// Tombstones across every block, summed from [`SstBlockIndexEntry::tombstone_count`]
    /// at open — O(metadata), never a data-block decode (see the module
    /// doc's "Lazy open" section for why that distinction matters).
    pub(crate) tombstones: u64,
    /// Bytes actually read from disk by [`Self::load`] to open this file —
    /// header + footer + index + bloom, **not** [`Self::file_bytes`]. Feeds
    /// `EngineStats::bytes_read` so that counter reflects the real,
    /// O(metadata) cost of opening a block-based SST (ADR-039 §8.1), rather
    /// than the whole file's size as the superseded whole-file reader did.
    /// `0` for a freshly [`Self::write_new`]'d file (nothing was "read").
    pub(crate) bytes_read_at_open: u64,
    entry_count: u64,
    block_index: Vec<SstBlockIndexEntry>,
    bloom: Bloom,
    /// Cloned once at construction (write or load) so [`Self::get`]/
    /// [`Self::entries`] can unseal on-demand block reads without every
    /// call site threading a `crypto: Option<&CryptoContext>` parameter —
    /// `CryptoContext` is a cheap `Clone` (just the DEK + a ready cipher).
    crypto: Option<CryptoContext>,
}

/// Bytes one entry occupies inside a data block: op(1) + key_len(4) +
/// val_len(4) + key + value — mirrors `format::sst_block`'s private
/// `BLOCK_ENTRY_HEADER_LEN`, recomputed here since it's only needed to
/// decide flush boundaries, not to decode anything.
fn entry_wire_size(key: &Key, value: &Option<Value>) -> usize {
    9 + key.as_bytes().len() + value.as_ref().map_or(0, Vec::len)
}

/// Flushes `current` as one (optionally sealed) data block if non-empty:
/// encodes it, seals it under `(sst_id, Data, *block_no)` when `crypto` is
/// `Some`, appends the on-disk bytes to `file_bytes`, records its index
/// entry (offset absolute from the start of the file, since `cursor` starts
/// at [`SST_HEADER_TOTAL_LEN`]) — including its `tombstone_count`, computed
/// here once so a lazy reader never has to decode the block just to sum
/// tombstones — and resets the accumulator.
#[allow(clippy::too_many_arguments)]
fn flush_block(
    current: &mut Vec<SstEntry>,
    current_size: &mut usize,
    file_bytes: &mut Vec<u8>,
    cursor: &mut u64,
    block_index: &mut Vec<SstBlockIndexEntry>,
    block_no: &mut u32,
    sst_id: u64,
    crypto: Option<&CryptoContext>,
) -> Result<()> {
    if current.is_empty() {
        return Ok(());
    }
    let first_key = current.first().expect("checked non-empty above").key.clone();
    let last_key = current.last().expect("checked non-empty above").key.clone();
    let entry_count = current.len() as u32;
    let tombstone_count = current.iter().filter(|e| e.value.is_none()).count() as u32;
    let plain = sst_block::encode_sst_data_block(current);
    let sealed = seal_section(crypto, plain, sst_id, SstSectionType::Data, *block_no)?;
    let len = sealed.len() as u32;
    block_index.push(SstBlockIndexEntry {
        first_key,
        last_key,
        offset: *cursor,
        len,
        entry_count,
        tombstone_count,
    });
    file_bytes.extend_from_slice(&sealed);
    *cursor += u64::from(len);
    *block_no += 1;
    current.clear();
    *current_size = 0;
    Ok(())
}

fn read_span(file: &mut File, path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| EngineError::io(path.to_path_buf(), e))?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf)
        .map_err(|e| EngineError::io(path.to_path_buf(), e))?;
    Ok(buf)
}

fn sum_entries(block_index: &[SstBlockIndexEntry]) -> u64 {
    block_index.iter().map(|e| u64::from(e.entry_count)).sum()
}

fn sum_tombstones(block_index: &[SstBlockIndexEntry]) -> u64 {
    block_index.iter().map(|e| u64::from(e.tombstone_count)).sum()
}

pub(crate) fn sst_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id:020}.sst"))
}

impl BlockSstFile {
    /// Writes `entries` (already sorted ascending by key — same contract as
    /// the superseded whole-file writer) as a new block-based SST file,
    /// splitting into data blocks once the accumulated wire size reaches
    /// `block_size` (a target, not an exact bound — ADR-039 §1). Crash-safe
    /// sequence: temp file, fsync, rename. With `crypto: Some`, every
    /// section except the header is sealed individually before being
    /// written (N8.8).
    pub(crate) fn write_new(
        dir: &Path,
        id: u64,
        entries: Vec<(Key, Option<Value>)>,
        block_size: u32,
        crypto: Option<&CryptoContext>,
    ) -> Result<Self> {
        let final_path = sst_path(dir, id);
        let tmp_path = final_path.with_extension("sst.tmp");

        let mut block_bytes = Vec::new();
        let mut block_index: Vec<SstBlockIndexEntry> = Vec::new();
        let mut bloom = Bloom::new(entries.len());

        let mut current: Vec<SstEntry> = Vec::new();
        let mut current_size = 0usize;
        let mut cursor = SST_HEADER_TOTAL_LEN as u64;
        let mut block_no: u32 = 0;

        for (key, value) in &entries {
            bloom.insert(key.as_bytes());
            current_size += entry_wire_size(key, value);
            current.push(SstEntry {
                key: key.as_bytes().to_vec(),
                value: value.clone(),
            });
            if current_size >= block_size as usize {
                flush_block(
                    &mut current,
                    &mut current_size,
                    &mut block_bytes,
                    &mut cursor,
                    &mut block_index,
                    &mut block_no,
                    id,
                    crypto,
                )?;
            }
        }
        flush_block(
            &mut current,
            &mut current_size,
            &mut block_bytes,
            &mut cursor,
            &mut block_index,
            &mut block_no,
            id,
            crypto,
        )?;

        let block_count = block_index.len() as u32;
        let index_plain = sst_block::encode_sst_block_index(&block_index);
        let index_sealed = seal_section(crypto, index_plain, id, SstSectionType::Index, 0)?;
        let bloom_plain = sst_block::encode_sst_bloom_filter(&bloom.to_filter());
        let bloom_sealed = seal_section(crypto, bloom_plain, id, SstSectionType::Bloom, 0)?;

        let header_bytes = sst_block::encode_sst_header(&SstHeader {
            sst_id: id,
            block_size,
            entry_count: entries.len() as u64,
            dim: 0,
        });
        debug_assert_eq!(header_bytes.len(), SST_HEADER_TOTAL_LEN);

        let index_offset = cursor;
        let bloom_offset = index_offset + index_sealed.len() as u64;
        let footer_plain = sst_block::encode_sst_footer(&SstFooter {
            index_offset,
            index_len: index_sealed.len() as u32,
            bloom_offset,
            bloom_len: bloom_sealed.len() as u32,
            block_count,
        });
        let footer_sealed = seal_section(crypto, footer_plain, id, SstSectionType::Footer, 0)?;

        let mut file_bytes = Vec::with_capacity(
            header_bytes.len() + block_bytes.len() + index_sealed.len() + bloom_sealed.len() + footer_sealed.len(),
        );
        file_bytes.extend_from_slice(&header_bytes);
        file_bytes.extend_from_slice(&block_bytes);
        file_bytes.extend_from_slice(&index_sealed);
        file_bytes.extend_from_slice(&bloom_sealed);
        file_bytes.extend_from_slice(&footer_sealed);

        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
            file.write_all(&file_bytes)
                .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
            fail_point!("after_sst_tmp_write");
            file.sync_all().map_err(|e| EngineError::io(tmp_path.clone(), e))?;
            fail_point!("after_sst_tmp_fsync");
        }
        fs::rename(&tmp_path, &final_path).map_err(|e| EngineError::io(final_path.clone(), e))?;
        fail_point!("after_sst_rename");
        // Same known, documented deviation as the superseded whole-file
        // writer: no extra fsync of the containing directory entry after
        // the rename (not portable on Windows, the primary dev/CI target).
        // Not a correctness gap for the WAL-truncate ordering rule — if the
        // rename itself doesn't survive a crash, the SST simply doesn't
        // exist on reopen and the data replays out of the (not yet
        // truncated) WAL instead.

        let tombstones = sum_tombstones(&block_index);
        Ok(Self {
            id,
            path: final_path,
            file_bytes: file_bytes.len() as u64,
            tombstones,
            bytes_read_at_open: 0,
            entry_count: entries.len() as u64,
            block_index,
            bloom,
            crypto: crypto.cloned(),
        })
    }

    /// Lazily opens an existing block-based SST: reads and verifies
    /// **only** the header, footer, block index and bloom filter — never a
    /// data block (N8.4, ADR-039 §8.1's O(metadata)-open exit criterion).
    /// Cross-checks the footer's `block_count` against the index's actual
    /// length and the header's `entry_count` against the index's summed
    /// per-block `entry_count`s — real corruption detection, still entirely
    /// from metadata already resident. Individual data blocks are only
    /// decoded (and their own first/last-key + entry-count cross-checked
    /// against their index entry) lazily, by [`Self::get`]/[`Self::entries`].
    pub(crate) fn load(path: PathBuf, id: u64, crypto: Option<&CryptoContext>) -> Result<Self> {
        let mut file = File::open(&path).map_err(|e| EngineError::io(path.clone(), e))?;
        let file_len = file.metadata().map_err(|e| EngineError::io(path.clone(), e))?.len();

        // 1. Header — always plaintext, fixed offset 0, the bootstrap
        //    record every other section's AAD needs `sst_id` from.
        if file_len < SST_HEADER_TOTAL_LEN as u64 {
            return Err(EngineError::CorruptSstHeader {
                path: path.clone(),
                reason: format!("file shorter than the fixed header ({SST_HEADER_TOTAL_LEN} bytes)"),
            });
        }
        let header_bytes = read_span(&mut file, &path, 0, SST_HEADER_TOTAL_LEN as u64)?;
        let header = sst_block::decode_sst_header(&header_bytes, &path)?;
        if header.sst_id != id {
            return Err(EngineError::CorruptSstHeader {
                path: path.clone(),
                reason: format!(
                    "header sst_id {} does not match this file's numeric id {id}",
                    header.sst_id
                ),
            });
        }

        // 2. Footer — fixed on-disk length even when encrypted (the
        //    plaintext footer has a fixed length, so its sealed envelope
        //    does too), located with one seek from EOF, no other section
        //    needed first.
        let footer_on_disk_len = match crypto {
            None => SST_FOOTER_LEN as u64,
            Some(_) => envelope::encrypted_sst_block_sealed_len(SST_FOOTER_LEN) as u64,
        };
        if file_len < footer_on_disk_len {
            return Err(EngineError::CorruptSstFooter {
                path: path.clone(),
                reason: format!("file shorter than the fixed footer ({footer_on_disk_len} bytes)"),
            });
        }
        let footer_raw = read_span(&mut file, &path, file_len - footer_on_disk_len, footer_on_disk_len)?;
        let footer_plain = open_section(crypto, &footer_raw, id, SstSectionType::Footer, 0, &path)?;
        let footer = sst_block::decode_sst_footer(&footer_plain, &path)?;

        // 3. Block index.
        let index_raw = read_span(&mut file, &path, footer.index_offset, u64::from(footer.index_len))?;
        let index_plain = open_section(crypto, &index_raw, id, SstSectionType::Index, 0, &path)?;
        let block_index = sst_block::decode_sst_block_index(&index_plain, &path)?;

        // 4. Bloom filter.
        let bloom_raw = read_span(&mut file, &path, footer.bloom_offset, u64::from(footer.bloom_len))?;
        let bloom_plain = open_section(crypto, &bloom_raw, id, SstSectionType::Bloom, 0, &path)?;
        let bloom = Bloom::from_filter(sst_block::decode_sst_bloom_filter(&bloom_plain, &path)?);

        // Cross-checks against metadata already resident — still
        // O(metadata), no data block touched.
        if block_index.len() as u32 != footer.block_count {
            return Err(EngineError::CorruptSstBlockIndex {
                path: path.clone(),
                reason: format!(
                    "footer declares block_count {} but the index has {} entries",
                    footer.block_count,
                    block_index.len()
                ),
            });
        }
        let total_entries = sum_entries(&block_index);
        if total_entries != header.entry_count {
            return Err(EngineError::CorruptSstHeader {
                path: path.clone(),
                reason: format!(
                    "header entry_count {} does not match {total_entries} entries implied by the block index",
                    header.entry_count
                ),
            });
        }

        let bytes_read_at_open =
            header_bytes.len() as u64 + footer_raw.len() as u64 + index_raw.len() as u64 + bloom_raw.len() as u64;
        let tombstones = sum_tombstones(&block_index);

        Ok(Self {
            id,
            path,
            file_bytes: file_len,
            tombstones,
            bytes_read_at_open,
            entry_count: header.entry_count,
            block_index,
            bloom,
            crypto: crypto.cloned(),
        })
    }

    /// Reads, unseals (if encrypted) and structurally verifies the data
    /// block at index `block_no` — cross-checking its actual first/last key
    /// and entry count against `entry`'s index metadata, the same checks
    /// the old full-decode reader made once at open, now made lazily at
    /// first read of that specific block.
    fn read_and_verify_block(
        &self,
        file: &mut File,
        block_no: usize,
        entry: &SstBlockIndexEntry,
    ) -> Result<Vec<(Key, Option<Value>)>> {
        let raw = read_span(file, &self.path, entry.offset, u64::from(entry.len))?;
        let plain = open_section(
            self.crypto.as_ref(),
            &raw,
            self.id,
            SstSectionType::Data,
            block_no as u32,
            &self.path,
        )?;
        let decoded = sst_block::decode_sst_data_block(&plain, &self.path)?;
        let Some(first) = decoded.first() else {
            return Err(EngineError::CorruptSstBlockIndex {
                path: self.path.clone(),
                reason: "block index references an empty data block".to_string(),
            });
        };
        let last = decoded.last().expect("non-empty, checked above");
        if first.key != entry.first_key || last.key != entry.last_key {
            return Err(EngineError::CorruptSstBlockIndex {
                path: self.path.clone(),
                reason: "block index first/last key does not match the block's actual entries".to_string(),
            });
        }
        if decoded.len() != entry.entry_count as usize {
            return Err(EngineError::CorruptSstBlockIndex {
                path: self.path.clone(),
                reason: "block index entry_count does not match the block's actual entry count".to_string(),
            });
        }
        Ok(decoded.into_iter().map(|e| (Key::from(e.key), e.value)).collect())
    }

    /// Point lookup: bloom filter (zero I/O on a miss) -> binary search the
    /// block index by `last_key` -> the *single* candidate block, from
    /// `cache` if resident (N8.7) or read from disk and inserted into it ->
    /// binary search within it. Structurally touches at most one data block
    /// — never falls back to a full scan (ADR-039 §4/§5.5's
    /// `point_lookup_full_sst_read == 0` invariant; see
    /// `store::engine::Engine::get` for the counter this feeds).
    ///
    /// Returns `(hit, blocks_read)`. `hit` mirrors the superseded reader's
    /// contract: `Some(None)` = tombstone, `Some(Some(v))` = value, `None` =
    /// absent from this SST. `blocks_read` counts **disk** block reads: `0`
    /// on a bloom miss, a key sorting outside every block's range, or a
    /// cache hit; `1` on a genuine disk read. Never more than `1` by
    /// construction.
    pub(crate) fn get(&self, key: &Key, cache: &BlockCache) -> Result<(Option<Option<Value>>, u64)> {
        if !self.bloom.contains(key.as_bytes()) {
            return Ok((None, 0));
        }
        let idx = self
            .block_index
            .partition_point(|entry| entry.last_key.as_slice() < key.as_bytes());
        let Some(entry) = self.block_index.get(idx) else {
            // Sorts after every block's last_key: a bloom false positive,
            // not an actual member of this SST.
            return Ok((None, 0));
        };
        if key.as_bytes() < entry.first_key.as_slice() {
            // Falls in the gap before this block's first_key — same bloom
            // false-positive case, no block genuinely covers this key.
            return Ok((None, 0));
        }
        let (block_entries, blocks_read) = match cache.get(self.id, idx as u32) {
            Some(cached) => (cached, 0u64),
            None => {
                let mut file = File::open(&self.path).map_err(|e| EngineError::io(self.path.clone(), e))?;
                let decoded = self.read_and_verify_block(&mut file, idx, entry)?;
                cache.insert(self.id, idx as u32, decoded.clone());
                (decoded, 1u64)
            }
        };
        let hit = block_entries
            .binary_search_by(|(k, _)| k.cmp(key))
            .ok()
            .map(|i| block_entries[i].1.clone());
        Ok((hit, blocks_read))
    }

    /// Full scan of every entry, in block order (equivalently, ascending key
    /// order — blocks and their entries are both sorted). Legitimate for
    /// compaction and prefix scans, which by nature must see every key;
    /// [`Self::get`] never calls this. Reuses one open file handle across
    /// every block.
    pub(crate) fn entries(&self) -> Result<Vec<(Key, Option<Value>)>> {
        let mut file = File::open(&self.path).map_err(|e| EngineError::io(self.path.clone(), e))?;
        let mut out = Vec::with_capacity(self.entry_count as usize);
        for (block_no, entry) in self.block_index.iter().enumerate() {
            out.extend(self.read_and_verify_block(&mut file, block_no, entry)?);
        }
        Ok(out)
    }
}

/// Scans `dir` for existing `*.sst` files (ignores `*.sst.tmp` orphans left
/// by a crash between write and rename) and returns them lazily opened
/// ([`BlockSstFile::load`]), sorted ascending by id (oldest first). Same
/// contract as the whole-file format's predecessor.
pub(crate) fn scan_existing(dir: &Path, crypto: Option<&CryptoContext>) -> Result<Vec<BlockSstFile>> {
    let mut found = Vec::new();
    if !dir.exists() {
        return Ok(found);
    }
    for entry in fs::read_dir(dir).map_err(|e| EngineError::io(dir.to_path_buf(), e))? {
        let entry = entry.map_err(|e| EngineError::io(dir.to_path_buf(), e))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sst") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(id) = stem.parse::<u64>() else { continue };
        found.push(BlockSstFile::load(path, id, crypto)?);
    }
    found.sort_by_key(|s| s.id);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(n: usize, val_len: usize) -> Vec<(Key, Option<Value>)> {
        (0..n)
            .map(|i| {
                let key = Key::from(format!("k/{i:06}").as_bytes());
                let value = if i % 7 == 0 { None } else { Some(vec![b'v'; val_len]) };
                (key, value)
            })
            .collect()
    }

    /// No tombstones, fixed per-entry wire size — deterministic block
    /// boundaries, used by the anti-permutation tests below where two
    /// blocks need to line up byte-for-byte in length.
    fn fixed_size_entries(n: usize, val_len: usize) -> Vec<(Key, Option<Value>)> {
        (0..n)
            .map(|i| (Key::from(format!("k/{i:06}").as_bytes()), Some(vec![b'v'; val_len])))
            .collect()
    }

    fn test_crypto(dir: &Path) -> CryptoContext {
        crate::crypto::create_meta(dir, b"sst block test key").expect("create crypto meta")
    }

    #[test]
    fn write_then_load_roundtrips_small() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(5, 10);
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 16 * 1024, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        assert_eq!(loaded.entries().expect("entries"), data);
    }

    #[test]
    fn empty_entries_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let written = BlockSstFile::write_new(dir.path(), 0, Vec::new(), 16 * 1024, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        assert!(loaded.entries().expect("entries").is_empty());
        assert_eq!(loaded.tombstones, 0);
    }

    #[test]
    fn write_then_load_roundtrips_across_many_blocks() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Small block_size forces many block boundaries with real payload.
        let data = entries(500, 200);
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 2048, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        assert_eq!(loaded.entries().expect("entries"), data);
    }

    #[test]
    fn get_finds_put_and_tombstone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(10, 5);
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 16 * 1024, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        let cache = BlockCache::new(1024 * 1024);
        for (key, value) in &data {
            let (hit, blocks_read) = loaded.get(key, &cache).expect("get");
            assert_eq!(hit, Some(value.clone()));
            assert!(blocks_read <= 1);
        }
        let (hit, blocks_read) = loaded.get(&Key::from(&b"missing"[..]), &cache).expect("get");
        assert_eq!(hit, None);
        assert!(blocks_read <= 1);
    }

    #[test]
    fn get_across_many_blocks_never_reads_more_than_one_block() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(2000, 100);
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 4096, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        assert!(loaded.block_index.len() > 10, "test needs many blocks");
        // A fresh, disabled-in-effect cache per lookup (each `get` here is
        // its own miss) so this test's `blocks_read == 1` assertion still
        // pins "exactly one disk block" rather than being satisfied by a
        // cache hit from an earlier lookup landing in the same block.
        for (key, _) in data.iter().step_by(37) {
            let cache = BlockCache::new(1024 * 1024);
            let (_, blocks_read) = loaded.get(key, &cache).expect("get");
            assert_eq!(
                blocks_read, 1,
                "a present key must resolve within exactly one disk block read on a cold cache"
            );
        }
        let cache = BlockCache::new(1024 * 1024);
        for i in 0..50u32 {
            let (hit, blocks_read) = loaded
                .get(&Key::from(format!("zzz/absent/{i}").as_bytes()), &cache)
                .expect("get");
            assert_eq!(hit, None);
            assert!(blocks_read <= 1);
        }
    }

    #[test]
    fn get_reuses_cached_block_on_repeated_lookup_in_the_same_block() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(200, 50);
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 4096, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        let cache = BlockCache::new(1024 * 1024);
        let key = &data[0].0;
        let (_, first) = loaded.get(key, &cache).expect("get");
        assert_eq!(first, 1, "first lookup of a block is a real disk read");
        let (_, second) = loaded.get(key, &cache).expect("get");
        assert_eq!(
            second, 0,
            "repeated lookup of the same block must hit the cache, not disk"
        );
    }

    #[test]
    fn bloom_has_no_false_negatives() {
        let mut bloom = Bloom::new(1000);
        let keys: Vec<Vec<u8>> = (0..1000).map(|i| format!("key-{i}").into_bytes()).collect();
        for key in &keys {
            bloom.insert(key);
        }
        for key in &keys {
            assert!(bloom.contains(key), "false negative for {key:?}");
        }
    }

    #[test]
    fn bloom_filter_roundtrips_through_wire_format() {
        let mut bloom = Bloom::new(100);
        bloom.insert(b"present");
        let filter = bloom.to_filter();
        let reloaded = Bloom::from_filter(filter);
        assert!(reloaded.contains(b"present"));
    }

    #[test]
    fn corrupted_data_block_is_rejected_lazily_not_at_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(50, 50);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 512, None).expect("write");
        let mut raw = std::fs::read(&written.path).expect("read raw");
        // Flip a byte well inside the data-block region (past the header).
        raw[SST_HEADER_TOTAL_LEN + 4] ^= 0xFF;
        std::fs::write(&written.path, &raw).expect("write tampered");
        // N8.4: `load` never touches data blocks, so a corrupt one does not
        // surface until something actually reads it.
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load succeeds: metadata is untouched");
        let err = loaded.entries().expect_err("reading the corrupt block must fail");
        assert!(
            matches!(
                err,
                EngineError::CorruptSstDataBlock { .. } | EngineError::CorruptSstBlockIndex { .. }
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn truncated_file_is_rejected_not_panicking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(20, 20);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 512, None).expect("write");
        let raw = std::fs::read(&written.path).expect("read raw");
        std::fs::write(&written.path, &raw[..raw.len() - 10]).expect("write truncated");
        let Err(err) = BlockSstFile::load(written.path, 0, None) else {
            panic!("truncated file must be rejected")
        };
        assert!(matches!(
            err,
            EngineError::CorruptSstFooter { .. } | EngineError::Io { .. }
        ));
    }

    // ── encryption (N8.8) ──

    #[test]
    fn encrypted_write_then_load_roundtrips_and_hides_plaintext() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        // Sorted ascending by key, per the writer's contract.
        let data = vec![
            (Key::from(&b"tombstoned"[..]), None),
            (Key::from(&b"visible-key"[..]), Some(b"secret-value".to_vec())),
        ];
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 16 * 1024, Some(&crypto)).expect("write");
        let raw = std::fs::read(&written.path).expect("read raw sst");
        for needle in [&b"visible-key"[..], b"secret-value", b"tombstoned"] {
            assert!(
                !raw.windows(needle.len()).any(|w| w == needle),
                "plaintext {needle:?} leaked into the encrypted SST file"
            );
        }
        let loaded = BlockSstFile::load(written.path, 0, Some(&crypto)).expect("load");
        assert_eq!(loaded.entries().expect("entries"), data);
        let cache = BlockCache::new(1024 * 1024);
        for (key, value) in &data {
            let (hit, _) = loaded.get(key, &cache).expect("get");
            assert_eq!(hit, Some(value.clone()));
        }
    }

    #[test]
    fn encrypted_load_rejects_tampered_footer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        let written = BlockSstFile::write_new(
            dir.path(),
            0,
            vec![(Key::from(&b"a"[..]), Some(b"1".to_vec()))],
            16 * 1024,
            Some(&crypto),
        )
        .expect("write");
        let mut raw = std::fs::read(&written.path).expect("read");
        let last = raw.len() - 1;
        raw[last] ^= 0xFF;
        std::fs::write(&written.path, &raw).expect("write tampered");
        let Err(err) = BlockSstFile::load(written.path, 0, Some(&crypto)) else {
            panic!("tampered footer must fail at open")
        };
        assert!(matches!(
            err,
            EngineError::CorruptEncryptedSstBlock { .. } | EngineError::CorruptSstFooter { .. }
        ));
    }

    #[test]
    fn encrypted_load_rejects_tampered_data_block_lazily() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        let data = entries(50, 50);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 512, Some(&crypto)).expect("write");
        let mut raw = std::fs::read(&written.path).expect("read raw");
        // Flip a byte inside the first sealed data block's *ciphertext*
        // (past its 34-byte `EncryptedSstBlock` envelope header — magic(4) +
        // version(2) + nonce(24) + ct_len(4) — so this lands on an AEAD tag
        // failure, not a version-field corruption).
        raw[SST_HEADER_TOTAL_LEN + 40] ^= 0xFF;
        std::fs::write(&written.path, &raw).expect("write tampered");
        let loaded = BlockSstFile::load(written.path, 0, Some(&crypto)).expect("load: metadata sections untouched");
        let err = loaded.entries().expect_err("reading the tampered block must fail");
        assert!(matches!(err, EngineError::CorruptEncryptedSstBlock { .. }));
    }

    #[test]
    fn plaintext_sst_read_in_encrypted_mode_is_loud_corruption() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        let written = BlockSstFile::write_new(
            dir.path(),
            0,
            vec![(Key::from(&b"a"[..]), Some(b"1".to_vec()))],
            16 * 1024,
            None,
        )
        .expect("write");
        let Err(err) = BlockSstFile::load(written.path, 0, Some(&crypto)) else {
            panic!("mode mismatch must be loud")
        };
        assert!(matches!(
            err,
            EngineError::CorruptEncryptedSstBlock { .. } | EngineError::CorruptSstFooter { .. }
        ));
    }

    #[test]
    fn encrypted_block_moved_between_two_ssts_fails_authentication() {
        // ADR-039 §3's anti-permutation requirement: a block moved between
        // two SSTs must fail authentication even though it is individually
        // intact, because `sst_id` is bound into the AAD.
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        let data = fixed_size_entries(60, 50);
        let a = BlockSstFile::write_new(dir.path(), 0, data.clone(), 512, Some(&crypto)).expect("write a");
        let b = BlockSstFile::write_new(dir.path(), 1, data, 512, Some(&crypto)).expect("write b");
        assert!(!a.block_index.is_empty() && !b.block_index.is_empty());

        let mut bytes_a = std::fs::read(&a.path).expect("read a");
        let bytes_b = std::fs::read(&b.path).expect("read b");
        let block0 = &a.block_index[0];
        let (offset, len) = (block0.offset as usize, block0.len as usize);
        // Same plaintext + same-length ciphertext (identical entries, same
        // block boundaries) — splicing b's sealed block 0 into a's file at
        // the same span is a mechanically valid substitution, only foiled
        // by the differing `sst_id` in the AAD.
        assert_eq!(
            bytes_b.len(),
            bytes_a.len(),
            "identical entries must produce identical file layouts"
        );
        bytes_a[offset..offset + len].copy_from_slice(&bytes_b[offset..offset + len]);
        std::fs::write(&a.path, &bytes_a).expect("write spliced");

        let loaded = BlockSstFile::load(a.path.clone(), 0, Some(&crypto)).expect("metadata sections untouched");
        let err = loaded
            .entries()
            .expect_err("a block from another SST must fail authentication");
        assert!(matches!(err, EngineError::CorruptEncryptedSstBlock { .. }));
    }

    #[test]
    fn encrypted_blocks_swapped_within_the_same_sst_fail_authentication() {
        // ADR-039 §3's other anti-permutation case: reordering two blocks
        // within the *same* SST must fail too, because `section_no` is
        // bound into the AAD.
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        let data = fixed_size_entries(60, 50);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 512, Some(&crypto)).expect("write");
        assert!(written.block_index.len() >= 2, "test needs at least two blocks");
        let (b0, b1) = (written.block_index[0].clone(), written.block_index[1].clone());
        assert_eq!(b0.len, b1.len, "fixed-size entries must produce equal-length blocks");

        let mut bytes = std::fs::read(&written.path).expect("read raw");
        let (o0, o1, len) = (b0.offset as usize, b1.offset as usize, b0.len as usize);
        let block0 = bytes[o0..o0 + len].to_vec();
        let block1 = bytes[o1..o1 + len].to_vec();
        bytes[o0..o0 + len].copy_from_slice(&block1);
        bytes[o1..o1 + len].copy_from_slice(&block0);
        std::fs::write(&written.path, &bytes).expect("write swapped");

        let loaded = BlockSstFile::load(written.path, 0, Some(&crypto)).expect("metadata sections untouched");
        let err = loaded.entries().expect_err("swapped blocks must fail authentication");
        assert!(matches!(err, EngineError::CorruptEncryptedSstBlock { .. }));
    }

    // ── scan_existing ──

    #[test]
    fn scan_existing_ignores_tmp_orphans() {
        let dir = tempfile::tempdir().expect("tempdir");
        BlockSstFile::write_new(
            dir.path(),
            0,
            vec![(Key::from(&b"a"[..]), Some(b"1".to_vec()))],
            16 * 1024,
            None,
        )
        .expect("write");
        // Simulate an orphaned tmp file left by a crash between write and rename.
        std::fs::write(dir.path().join("00000000000000000001.sst.tmp"), b"garbage").expect("write orphan");

        let found = scan_existing(dir.path(), None).expect("scan");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, 0);
    }

    #[test]
    fn scan_existing_sorts_ascending_by_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        BlockSstFile::write_new(
            dir.path(),
            5,
            vec![(Key::from(&b"a"[..]), Some(b"1".to_vec()))],
            16 * 1024,
            None,
        )
        .expect("write");
        BlockSstFile::write_new(
            dir.path(),
            2,
            vec![(Key::from(&b"b"[..]), Some(b"2".to_vec()))],
            16 * 1024,
            None,
        )
        .expect("write");

        let found = scan_existing(dir.path(), None).expect("scan");
        let ids: Vec<_> = found.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![2, 5]);
    }

    #[test]
    fn open_reads_only_metadata_not_data_payload() {
        // N8.4's core promise: `bytes_read_at_open` must stay far below
        // `file_bytes` once there is real payload beyond one block.
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(5000, 200);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 4096, None).expect("write");
        let file_bytes = written.file_bytes;
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        assert!(
            loaded.bytes_read_at_open < file_bytes / 4,
            "open read {} bytes out of a {file_bytes}-byte file — not O(metadata)",
            loaded.bytes_read_at_open
        );
    }
}
