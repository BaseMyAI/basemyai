// SPDX-License-Identifier: BUSL-1.1
//! Full/prefix/range scans — index-driven block pruning (ADR-039 §4):
//! [`BlockSstFile::entries`] is compaction's only legitimate full walk;
//! `entries_with_*` decode only the blocks whose `[first_key, last_key]`
//! range can overlap the query, found by binary search on the resident
//! block index (the same index [`super::read`]'s `get` uses).

use std::fs::File;

use crate::error::{EngineError, Result};
use crate::key::Key;
use crate::store::Value;

use super::BlockSstFile;

/// Result of [`BlockSstFile::entries_with_prefix`]/[`BlockSstFile::entries_with_range`]:
/// the matching entries in ascending key order, plus the number of data
/// blocks actually decoded to produce them (the pruning invariant the tests
/// pin).
pub(crate) type PrefixScan = (Vec<(Key, Option<Value>)>, u64);

/// Result of [`BlockSstFile::entries_with_range_limited`]: the matching
/// entries in ascending key order, whether the walk was truncated by the
/// limit (`true` = later blocks may still hold in-range keys past the last
/// returned one), and the number of data blocks actually decoded.
pub(crate) type LimitedRangeScan = (Vec<(Key, Option<Value>)>, bool, u64);

impl BlockSstFile {
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

    /// Every entry whose key starts with `prefix`, in ascending key order —
    /// decoding only the data blocks whose `[first_key, last_key]` range can
    /// overlap the prefix range, found by binary search on the block index
    /// (the same index [`Self::get`] uses; ADR-039 §4's planned index-driven
    /// scan). Blocks strictly before the range are skipped by
    /// `partition_point`; the walk stops at the first block whose
    /// `first_key` already sorts past every prefixed key. Boundary blocks
    /// may contain non-matching neighbors, hence the per-entry filter.
    ///
    /// Returns `(matches, blocks_read)` where `blocks_read` counts data
    /// blocks actually decoded — the pruning invariant the tests pin.
    /// Deliberately bypasses the engine's block cache, like every scan path
    /// (see `EngineStats::block_cache` docs: scans would only evict hot
    /// point-lookup blocks).
    pub(crate) fn entries_with_prefix(&self, prefix: &[u8]) -> Result<PrefixScan> {
        let start = self
            .block_index
            .partition_point(|entry| entry.last_key.as_slice() < prefix);
        if start == self.block_index.len() {
            return Ok((Vec::new(), 0));
        }
        let mut file = File::open(&self.path).map_err(|e| EngineError::io(self.path.clone(), e))?;
        let mut out = Vec::new();
        let mut blocks_read = 0u64;
        for (block_no, entry) in self.block_index.iter().enumerate().skip(start) {
            // A block whose first_key sorts after `prefix` without carrying
            // it can no longer contain a match, and neither can any later
            // block (blocks are sorted): every prefixed key sorts below
            // such a first_key.
            if entry.first_key.as_slice() > prefix && !entry.first_key.starts_with(prefix) {
                break;
            }
            let decoded = self.read_and_verify_block(&mut file, block_no, entry)?;
            blocks_read += 1;
            out.extend(decoded.into_iter().filter(|(k, _)| k.as_bytes().starts_with(prefix)));
        }
        Ok((out, blocks_read))
    }

    /// Like [`Self::entries_with_prefix`], but bounded on **both** sides by
    /// an explicit `[start, end)` range instead of a fixed prefix (ADR-041
    /// §7.2: the primitive a `valid_until <= now` range query needs — a
    /// prefix scan alone can't express an upper bound). Same two-sided
    /// block-skipping as the prefix variant: `partition_point` finds the
    /// first block that could contain `start`, and the walk stops at the
    /// first block whose `first_key` already sorts at or past `end` — no
    /// later block can contain anything below `end` either, since blocks
    /// are sorted. Boundary blocks may contain out-of-range neighbors, hence
    /// the per-entry filter.
    ///
    /// Returns `(matches, blocks_read)`, same contract as
    /// [`Self::entries_with_prefix`].
    pub(crate) fn entries_with_range(&self, start: &[u8], end: &[u8]) -> Result<PrefixScan> {
        if start >= end {
            return Ok((Vec::new(), 0));
        }
        let block_start = self
            .block_index
            .partition_point(|entry| entry.last_key.as_slice() < start);
        if block_start == self.block_index.len() {
            return Ok((Vec::new(), 0));
        }
        let mut file = File::open(&self.path).map_err(|e| EngineError::io(self.path.clone(), e))?;
        let mut out = Vec::new();
        let mut blocks_read = 0u64;
        for (block_no, entry) in self.block_index.iter().enumerate().skip(block_start) {
            if entry.first_key.as_slice() >= end {
                break;
            }
            let decoded = self.read_and_verify_block(&mut file, block_no, entry)?;
            blocks_read += 1;
            out.extend(
                decoded
                    .into_iter()
                    .filter(|(k, _)| k.as_bytes() >= start && k.as_bytes() < end),
            );
        }
        Ok((out, blocks_read))
    }

    /// Like [`Self::entries_with_range`], but stops reading blocks once at
    /// least `limit` matches have been collected (ADR-041 §7.3: the bounded
    /// building block behind [`crate::Engine::scan_range_page`] — an
    /// unbounded per-SST read would defeat the whole point of a paged scan).
    ///
    /// Returns `(matches, truncated, blocks_read)`. `truncated == true`
    /// means the walk stopped **because of `limit`** while later blocks may
    /// still hold in-range keys: the source is only complete up to the last
    /// returned key, and the caller must treat everything past it as unknown.
    /// `truncated == false` means the range was exhausted (same completeness
    /// contract as [`Self::entries_with_range`]). Block granularity means
    /// the result can overshoot `limit` by up to one block's worth of
    /// entries — bounded, never silently trimmed (trimming would break the
    /// "complete up to the last returned key" invariant mid-block).
    pub(crate) fn entries_with_range_limited(
        &self,
        start: &[u8],
        end: &[u8],
        limit: usize,
    ) -> Result<LimitedRangeScan> {
        if start >= end || limit == 0 {
            return Ok((Vec::new(), false, 0));
        }
        let block_start = self
            .block_index
            .partition_point(|entry| entry.last_key.as_slice() < start);
        if block_start == self.block_index.len() {
            return Ok((Vec::new(), false, 0));
        }
        let mut file = File::open(&self.path).map_err(|e| EngineError::io(self.path.clone(), e))?;
        let mut out = Vec::new();
        let mut blocks_read = 0u64;
        for (block_no, entry) in self.block_index.iter().enumerate().skip(block_start) {
            if entry.first_key.as_slice() >= end {
                // Range exhausted before the limit bit: not a truncation.
                return Ok((out, false, blocks_read));
            }
            if out.len() >= limit {
                return Ok((out, true, blocks_read));
            }
            let decoded = self.read_and_verify_block(&mut file, block_no, entry)?;
            blocks_read += 1;
            out.extend(
                decoded
                    .into_iter()
                    .filter(|(k, _)| k.as_bytes() >= start && k.as_bytes() < end),
            );
        }
        Ok((out, false, blocks_read))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{entries, fixed_size_entries, test_crypto};
    use super::*;

    #[test]
    fn entries_with_prefix_matches_full_scan_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Three key families across many small blocks, tombstones included.
        let mut data: Vec<(Key, Option<Value>)> = Vec::new();
        for family in ["graph", "user", "vector"] {
            for i in 0..300 {
                let key = Key::from(format!("{family}/{i:06}").as_bytes());
                let value = if i % 7 == 0 { None } else { Some(vec![b'v'; 50]) };
                data.push((key, value));
            }
        }
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 2048, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        for prefix in [&b"graph/"[..], b"user/", b"vector/", b"user/000001", b"", b"absent/"] {
            let expected: Vec<_> = data
                .iter()
                .filter(|(k, _)| k.as_bytes().starts_with(prefix))
                .cloned()
                .collect();
            let (got, _) = loaded.entries_with_prefix(prefix).expect("prefix scan");
            assert_eq!(got, expected, "prefix {:?}", String::from_utf8_lossy(prefix));
        }
    }

    #[test]
    fn entries_with_prefix_decodes_only_overlapping_blocks() {
        let dir = tempfile::tempdir().expect("tempdir");
        // "middle/" is sandwiched between two large families so most blocks
        // are outside its range.
        let mut data: Vec<(Key, Option<Value>)> = Vec::new();
        for family in ["aaa", "middle", "zzz"] {
            let count = if family == "middle" { 20 } else { 1000 };
            for i in 0..count {
                data.push((Key::from(format!("{family}/{i:06}").as_bytes()), Some(vec![b'v'; 100])));
            }
        }
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 4096, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        let total_blocks = loaded.block_index.len() as u64;
        assert!(total_blocks > 20, "test needs many blocks, got {total_blocks}");

        let (got, blocks_read) = loaded.entries_with_prefix(b"middle/").expect("prefix scan");
        assert_eq!(got.len(), 20);
        // 20 entries of ~115 wire bytes fit in one 4096-byte block; allow the
        // two boundary blocks shared with the neighboring families.
        assert!(
            blocks_read <= 3,
            "narrow prefix must decode only its overlapping blocks, read {blocks_read} of {total_blocks}"
        );

        // A prefix sorting past every key touches nothing at all.
        let (got, blocks_read) = loaded.entries_with_prefix(b"zzzz/").expect("prefix scan");
        assert!(got.is_empty());
        assert_eq!(blocks_read, 0);

        // A prefix sorting before every key stops at the first block.
        let (got, blocks_read) = loaded.entries_with_prefix(b"AAA/").expect("prefix scan");
        assert!(got.is_empty());
        assert!(blocks_read <= 1);

        // The empty prefix legitimately decodes everything.
        let (got, blocks_read) = loaded.entries_with_prefix(b"").expect("prefix scan");
        assert_eq!(got.len(), data.len());
        assert_eq!(blocks_read, total_blocks);
    }

    #[test]
    fn entries_with_range_matches_full_scan_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut data: Vec<(Key, Option<Value>)> = Vec::new();
        for family in ["graph", "user", "vector"] {
            for i in 0..300 {
                let key = Key::from(format!("{family}/{i:06}").as_bytes());
                let value = if i % 7 == 0 { None } else { Some(vec![b'v'; 50]) };
                data.push((key, value));
            }
        }
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 2048, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        let cases: [(&[u8], &[u8]); 5] = [
            (b"graph/", b"user/"),
            (b"user/000050", b"user/000150"),
            (b"", b"aaa"),
            (b"zzzz", b"zzzz0"),
            (b"vector/", b"vector0"),
        ];
        for (start, end) in cases {
            let expected: Vec<_> = data
                .iter()
                .filter(|(k, _)| k.as_bytes() >= start && k.as_bytes() < end)
                .cloned()
                .collect();
            let (got, _) = loaded.entries_with_range(start, end).expect("range scan");
            assert_eq!(
                got,
                expected,
                "range [{:?}, {:?})",
                String::from_utf8_lossy(start),
                String::from_utf8_lossy(end)
            );
        }
        // An empty (or inverted) range never touches anything.
        let (got, blocks_read) = loaded.entries_with_range(b"user/", b"user/").expect("range scan");
        assert!(got.is_empty());
        assert_eq!(blocks_read, 0);
        let (got, blocks_read) = loaded.entries_with_range(b"zzz", b"aaa").expect("range scan");
        assert!(got.is_empty());
        assert_eq!(blocks_read, 0);
    }

    #[test]
    fn entries_with_range_decodes_only_overlapping_blocks() {
        let dir = tempfile::tempdir().expect("tempdir");
        // "middle/" is sandwiched between two large families so most blocks
        // are outside the queried range.
        let mut data: Vec<(Key, Option<Value>)> = Vec::new();
        for family in ["aaa", "middle", "zzz"] {
            let count = if family == "middle" { 20 } else { 1000 };
            for i in 0..count {
                data.push((Key::from(format!("{family}/{i:06}").as_bytes()), Some(vec![b'v'; 100])));
            }
        }
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 4096, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        let total_blocks = loaded.block_index.len() as u64;
        assert!(total_blocks > 20, "test needs many blocks, got {total_blocks}");

        let (got, blocks_read) = loaded.entries_with_range(b"middle/", b"middle0").expect("range scan");
        assert_eq!(got.len(), 20);
        assert!(
            blocks_read <= 3,
            "narrow range must decode only its overlapping blocks, read {blocks_read} of {total_blocks}"
        );

        // A range sorting past every key touches nothing at all.
        let (got, blocks_read) = loaded.entries_with_range(b"zzzz/", b"zzzz0").expect("range scan");
        assert!(got.is_empty());
        assert_eq!(blocks_read, 0);

        // The full-keyspace range legitimately decodes everything.
        let (got, blocks_read) = loaded.entries_with_range(b"", &[0xff]).expect("range scan");
        assert_eq!(got.len(), data.len());
        assert_eq!(blocks_read, total_blocks);
    }

    #[test]
    fn entries_with_range_limited_pages_reassemble_the_full_range() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Tombstones included: the limited walk must count them as matches
        // (they are entries a merge layer above needs to see).
        let data = entries(400, 60);
        let written = BlockSstFile::write_new(dir.path(), 0, data.clone(), 1024, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        assert!(loaded.block_index.len() > 10, "test needs many blocks");

        let (full, _) = loaded.entries_with_range(b"k/", b"k0").expect("range scan");
        assert_eq!(full.len(), data.len());

        // Chain limited pages: resume from the successor of the last key.
        let mut start: Vec<u8> = b"k/".to_vec();
        let mut reassembled: Vec<(Key, Option<Value>)> = Vec::new();
        loop {
            let (page, truncated, _) = loaded
                .entries_with_range_limited(&start, b"k0", 37)
                .expect("limited scan");
            if truncated {
                assert!(
                    page.len() >= 37,
                    "a truncated page stopped early, so it must have gathered at least `limit` matches"
                );
            }
            let last = page.last().map(|(k, _)| k.as_bytes().to_vec());
            reassembled.extend(page);
            if !truncated {
                break;
            }
            let mut next = last.expect("truncated implies a non-empty page");
            next.push(0x00);
            start = next;
        }
        assert_eq!(
            reassembled, full,
            "chained limited pages must reassemble the full range"
        );
    }

    #[test]
    fn entries_with_range_limited_stops_reading_blocks_early() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = fixed_size_entries(2000, 100);
        let written = BlockSstFile::write_new(dir.path(), 0, data, 4096, None).expect("write");
        let loaded = BlockSstFile::load(written.path, 0, None).expect("load");
        let total_blocks = loaded.block_index.len() as u64;
        assert!(total_blocks > 20, "test needs many blocks, got {total_blocks}");

        let (page, truncated, blocks_read) = loaded
            .entries_with_range_limited(b"k/", b"k0", 10)
            .expect("limited scan");
        assert!(truncated);
        assert!(page.len() >= 10);
        assert!(
            blocks_read <= 2,
            "a small limit must stop after a couple of blocks, read {blocks_read} of {total_blocks}"
        );

        // Exhausting the range before the limit bites is not a truncation.
        let (page, truncated, _) = loaded
            .entries_with_range_limited(b"k/", b"k0", usize::MAX)
            .expect("limited scan");
        assert!(!truncated);
        assert_eq!(page.len(), 2000);

        // Degenerate inputs: empty range, zero limit.
        let (page, truncated, blocks_read) = loaded
            .entries_with_range_limited(b"k0", b"k/", 10)
            .expect("limited scan");
        assert!(page.is_empty() && !truncated && blocks_read == 0);
        let (page, truncated, blocks_read) = loaded
            .entries_with_range_limited(b"k/", b"k0", 0)
            .expect("limited scan");
        assert!(page.is_empty() && !truncated && blocks_read == 0);
    }

    #[test]
    fn entries_with_prefix_roundtrips_encrypted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        let data = entries(500, 100);
        let written = BlockSstFile::write_new(dir.path(), 3, data.clone(), 2048, Some(&crypto)).expect("write");
        let loaded = BlockSstFile::load(written.path, 3, Some(&crypto)).expect("load");
        let prefix = b"k/0001";
        let expected: Vec<_> = data
            .iter()
            .filter(|(k, _)| k.as_bytes().starts_with(prefix))
            .cloned()
            .collect();
        let (got, blocks_read) = loaded.entries_with_prefix(prefix).expect("prefix scan");
        assert_eq!(got, expected);
        assert!(
            blocks_read < loaded.block_index.len() as u64,
            "narrow prefix must not decode the whole encrypted SST"
        );
    }
}
