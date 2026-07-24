// SPDX-License-Identifier: BUSL-1.1
//! Lazy open (N8.4) plus the block-decode path every reader — [`Self::get`],
//! the [`super::scan`] walks, and `store::verify`'s accessors — shares
//! through [`BlockSstFile::read_and_verify_block`], so verification exercises
//! the real reader instead of a parallel decoder.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::crypto::CryptoContext;
use crate::error::{EngineError, Result};
use crate::format::crypto::{self as envelope, SstSectionType};
use crate::format::sst_block::{self, SST_FOOTER_LEN, SST_HEADER_TOTAL_LEN, SstBlockIndexEntry};
use crate::key::Key;
use crate::store::Value;
use crate::store::block_cache::BlockCache;

use super::{BlockSstFile, sum_tombstones};

fn sum_entries(block_index: &[SstBlockIndexEntry]) -> u64 {
    block_index.iter().map(|e| u64::from(e.entry_count)).sum()
}

/// Inverse of `write::seal_section`: unseals `bytes` if `crypto` is `Some`
/// (an AEAD failure is [`EngineError::CorruptEncryptedSstBlock`] — by the
/// time a section is opened, the key has already been verified against
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

/// Reads exactly `len` bytes starting at `offset` — the single centralized
/// bound every section read (header, footer, block index, bloom filter,
/// data block) goes through. Refuses **before allocating** if `offset + len`
/// overflows or would extend past `file_len` (the file's real, already-known
/// on-disk length), via `make_err` so each call site reports its own
/// accurate section-specific error variant.
///
/// SST-ALLOC (BaseMyAI adversarial audit, 2026-07-22): before this bound
/// existed here, the header and footer reads happened to be safe only
/// because their *callers* separately checked `file_len` immediately above
/// each call — but the block index, bloom filter, and data block reads
/// trusted `offset`/`len` fields taken directly from the footer or block
/// index (on-disk, attacker/corruption-controlled) with no such check
/// anywhere, letting a tiny forged `.sst` file claim a section length near
/// `u64::MAX` and force a multi-gigabyte `vec![0u8; len]` allocation before
/// any I/O error could occur — reachable via `Engine::open`, `basemyai
/// verify`, and `rebuild_indexes`. Centralizing the check here, unconditional
/// for every caller, removes the possibility of a future call site
/// forgetting it the way those three did.
fn read_span(
    file: &mut File,
    path: &Path,
    offset: u64,
    len: u64,
    file_len: u64,
    make_err: impl Fn(String) -> EngineError,
) -> Result<Vec<u8>> {
    let end = offset.checked_add(len).ok_or_else(|| {
        make_err(format!(
            "section offset {offset} + length {len} overflows a 64-bit length"
        ))
    })?;
    if end > file_len {
        return Err(make_err(format!(
            "section [{offset}, {end}) extends past the file's actual on-disk length ({file_len} bytes)"
        )));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| EngineError::io(path.to_path_buf(), e))?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf)
        .map_err(|e| EngineError::io(path.to_path_buf(), e))?;
    Ok(buf)
}

impl BlockSstFile {
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
        let header_bytes = read_span(&mut file, &path, 0, SST_HEADER_TOTAL_LEN as u64, file_len, |reason| {
            EngineError::CorruptSstHeader {
                path: path.clone(),
                reason,
            }
        })?;
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
        let footer_raw = read_span(
            &mut file,
            &path,
            file_len - footer_on_disk_len,
            footer_on_disk_len,
            file_len,
            |reason| EngineError::CorruptSstFooter {
                path: path.clone(),
                reason,
            },
        )?;
        let footer_plain = open_section(crypto, &footer_raw, id, SstSectionType::Footer, 0, &path)?;
        let footer = sst_block::decode_sst_footer(&footer_plain, &path)?;

        // 3. Block index — `footer.index_offset`/`index_len` are on-disk,
        //    attacker/corruption-controlled fields: `read_span` refuses
        //    before allocating if they don't fit inside the real file
        //    (SST-ALLOC).
        let index_raw = read_span(
            &mut file,
            &path,
            footer.index_offset,
            u64::from(footer.index_len),
            file_len,
            |reason| EngineError::CorruptSstBlockIndex {
                path: path.clone(),
                reason,
            },
        )?;
        let index_plain = open_section(crypto, &index_raw, id, SstSectionType::Index, 0, &path)?;
        let block_index = sst_block::decode_sst_block_index(&index_plain, &path)?;

        // 4. Bloom filter — same SST-ALLOC bound as the block index above.
        let bloom_raw = read_span(
            &mut file,
            &path,
            footer.bloom_offset,
            u64::from(footer.bloom_len),
            file_len,
            |reason| EngineError::CorruptSstBloomFilter {
                path: path.clone(),
                reason,
            },
        )?;
        let bloom_plain = open_section(crypto, &bloom_raw, id, SstSectionType::Bloom, 0, &path)?;
        let bloom = super::bloom::Bloom::from_filter(sst_block::decode_sst_bloom_filter(&bloom_plain, &path)?);

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

        // SST-INDEX-ORDER (BaseMyAI adversarial audit, 2026-07-22): `get`'s
        // and `entries_with_prefix`/`entries_with_range`'s binary searches
        // over `block_index` (below) assume it is strictly ascending and
        // non-overlapping by key — nothing previously verified that
        // assumption against the on-disk bytes. A reordered or overlapping
        // index (crafted with a recomputed section checksum/AEAD tag) could
        // silently misroute a point lookup to the wrong block, or past the
        // "not covered" boundary for a key that is actually present,
        // returning `None`/an incomplete scan for live data with no error at
        // all. Checked once here, O(block_count) over already-resident
        // metadata — every later lookup relies on this invariant already
        // holding rather than re-verifying it per call.
        for entry in &block_index {
            if entry.first_key > entry.last_key {
                return Err(EngineError::CorruptSstBlockIndex {
                    path: path.clone(),
                    reason: format!(
                        "block index entry has first_key {:?} after its own last_key {:?}",
                        entry.first_key, entry.last_key
                    ),
                });
            }
        }
        for pair in block_index.windows(2) {
            if pair[0].last_key >= pair[1].first_key {
                return Err(EngineError::CorruptSstBlockIndex {
                    path: path.clone(),
                    reason: format!(
                        "block index is not strictly ascending/non-overlapping: block last_key {:?} \
                         is not less than the next block's first_key {:?}",
                        pair[0].last_key, pair[1].first_key
                    ),
                });
            }
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
    pub(super) fn read_and_verify_block(
        &self,
        file: &mut File,
        block_no: usize,
        entry: &SstBlockIndexEntry,
    ) -> Result<Vec<(Key, Option<Value>)>> {
        // `entry.offset`/`entry.len` come from the block index — on-disk,
        // attacker/corruption-controlled — same SST-ALLOC bound as the
        // index/bloom reads in `load`, checked against `self.file_bytes`
        // (this file's real on-disk length, captured at `load` time).
        let raw = read_span(
            file,
            &self.path,
            entry.offset,
            u64::from(entry.len),
            self.file_bytes,
            |reason| EngineError::CorruptSstDataBlock {
                path: self.path.clone(),
                reason,
            },
        )?;
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

    // ── verification accessors (ADR-040, N9.2) ──────────────────────────
    //
    // Read-only views for `store::verify` — the audit path needs to walk
    // the same metadata the reader trusts (index entries, bloom membership)
    // and decode individual blocks with the same cross-checks, without
    // duplicating any decode logic that could drift from the real read path.

    /// The resident block index, in on-disk (ascending key) order.
    pub(crate) fn block_index(&self) -> &[SstBlockIndexEntry] {
        &self.block_index
    }

    /// Bloom membership for `key` — used by verification to pin the
    /// no-false-negative invariant over the file's *actual* key set.
    pub(crate) fn bloom_contains(&self, key: &[u8]) -> bool {
        self.bloom.contains(key)
    }

    /// Reads, unseals and structurally verifies one data block through the
    /// exact same path `get`/`entries` use ([`Self::read_and_verify_block`])
    /// — verification must exercise the real reader, not a parallel decoder.
    pub(crate) fn read_block(&self, file: &mut File, block_no: usize) -> Result<Vec<(Key, Option<Value>)>> {
        let entry = &self.block_index[block_no];
        self.read_and_verify_block(file, block_no, entry)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{entries, fixed_size_entries, test_crypto};
    use super::*;
    use crate::format::sst_block::SstFooter;

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

    // ── SST-ALLOC (BaseMyAI adversarial audit, 2026-07-22): a forged
    // section length must be rejected typed, before any allocation
    // proportional to the forged (not the real file's) length. ──

    /// Rewrites the plaintext footer at the end of `path` with `patch`
    /// applied, keeping every other byte identical — the minimal forgery a
    /// filesystem-write-capable actor (or bit-flip-plus-recomputed-CRC) needs
    /// to make a tiny file claim an oversized section.
    fn forge_footer(path: &Path, patch: impl FnOnce(&mut SstFooter)) {
        let mut raw = std::fs::read(path).expect("read raw sst");
        let footer_start = raw.len() - SST_FOOTER_LEN;
        let mut footer = sst_block::decode_sst_footer(&raw[footer_start..], path).expect("decode real footer");
        patch(&mut footer);
        let forged = sst_block::encode_sst_footer(&footer);
        assert_eq!(
            forged.len(),
            SST_FOOTER_LEN,
            "forged footer must keep the fixed on-disk length"
        );
        raw[footer_start..].copy_from_slice(&forged);
        std::fs::write(path, &raw).expect("write forged sst");
    }

    #[test]
    fn forged_huge_index_len_is_rejected_typed_not_by_attempting_the_allocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(5, 20);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 512, None).expect("write");
        forge_footer(&written.path, |footer| footer.index_len = u32::MAX);

        let Err(err) = BlockSstFile::load(written.path, 0, None) else {
            panic!("forged index_len must be rejected");
        };
        assert!(
            matches!(err, EngineError::CorruptSstBlockIndex { .. }),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn forged_huge_bloom_len_is_rejected_typed_not_by_attempting_the_allocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(5, 20);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 512, None).expect("write");
        forge_footer(&written.path, |footer| footer.bloom_len = u32::MAX);

        let Err(err) = BlockSstFile::load(written.path, 0, None) else {
            panic!("forged bloom_len must be rejected");
        };
        assert!(
            matches!(err, EngineError::CorruptSstBloomFilter { .. }),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn forged_index_offset_past_eof_is_rejected_typed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(5, 20);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 512, None).expect("write");
        forge_footer(&written.path, |footer| footer.index_offset = u64::MAX - 8);

        let Err(err) = BlockSstFile::load(written.path, 0, None) else {
            panic!("forged index_offset must be rejected");
        };
        assert!(
            matches!(err, EngineError::CorruptSstBlockIndex { .. }),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn forged_block_index_entry_with_huge_len_is_rejected_lazily_not_by_attempting_the_allocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = entries(5, 20);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 512, None).expect("write");

        // Forge the block index itself (a separate section from the footer):
        // decode it, inflate the one entry's `len`, recompute its section
        // wrapper. `load` never touches data blocks (N8.4, O(metadata) open),
        // so this must only surface — typed, not a huge allocation attempt —
        // once something actually reads the block.
        let mut raw = std::fs::read(&written.path).expect("read raw sst");
        let footer = sst_block::decode_sst_footer(&raw[raw.len() - SST_FOOTER_LEN..], &written.path).expect("footer");
        let index_start = footer.index_offset as usize;
        let index_end = index_start + footer.index_len as usize;
        let mut block_index =
            sst_block::decode_sst_block_index(&raw[index_start..index_end], &written.path).expect("decode index");
        assert_eq!(block_index.len(), 1, "test assumes a single data block");
        block_index[0].len = u32::MAX;
        let forged_index = sst_block::encode_sst_block_index(&block_index);
        assert_eq!(
            forged_index.len(),
            footer.index_len as usize,
            "forging entry.len must not change the index section's own on-disk length"
        );

        raw[index_start..index_end].copy_from_slice(&forged_index);
        std::fs::write(&written.path, &raw).expect("write forged sst");

        let loaded = BlockSstFile::load(written.path, 0, None).expect("load: metadata cross-checks still pass");
        let err = loaded.entries().expect_err("reading the forged block must fail");
        assert!(
            matches!(err, EngineError::CorruptSstDataBlock { .. }),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn forged_reordered_block_index_is_rejected_typed_at_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Small block_size forces at least two data blocks for a handful of
        // entries with distinct keys.
        let data: Vec<(Key, Option<Value>)> = (0..10)
            .map(|i| (Key::from(format!("k/{i:04}").as_bytes()), Some(vec![b'v'; 20])))
            .collect();
        let written = BlockSstFile::write_new(dir.path(), 0, data, 64, None).expect("write");

        let mut raw = std::fs::read(&written.path).expect("read raw sst");
        let footer = sst_block::decode_sst_footer(&raw[raw.len() - SST_FOOTER_LEN..], &written.path).expect("footer");
        let index_start = footer.index_offset as usize;
        let index_end = index_start + footer.index_len as usize;
        let mut block_index =
            sst_block::decode_sst_block_index(&raw[index_start..index_end], &written.path).expect("decode index");
        assert!(block_index.len() >= 2, "test assumes at least two data blocks");
        block_index.swap(0, 1); // reorder — first/last keys now go backwards
        let forged_index = sst_block::encode_sst_block_index(&block_index);
        assert_eq!(
            forged_index.len(),
            footer.index_len as usize,
            "swapping entries must not change the index section's own on-disk length"
        );

        raw[index_start..index_end].copy_from_slice(&forged_index);
        std::fs::write(&written.path, &raw).expect("write forged sst");

        let Err(err) = BlockSstFile::load(written.path, 0, None) else {
            panic!("reordered block index must be rejected at load, before any lookup uses it");
        };
        assert!(
            matches!(err, EngineError::CorruptSstBlockIndex { .. }),
            "unexpected error: {err}"
        );
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
}
