// SPDX-License-Identifier: BUSL-1.1
//! Block-based SST writer + optimized reader (ADR-039, N8.3/N8.4/N8.5/N8.8):
//! the sole SST implementation [`crate::store::Engine`] uses — the whole-file
//! `SstFile` this replaced (ADR-025 Phase A) is deleted, per ADR-039 §5.3's
//! "no dual-format transition, no migration" policy.
//!
//! Layout assembled here from the `format::sst_block` codecs: header, data
//! blocks, block index, bloom filter, footer — see that module's doc for the
//! exact wire layout of each section. Split by responsibility: [`bloom`] the
//! filter, [`write`] the writer (`write_new`), [`read`] the lazy open plus
//! the block-decode path every reader shares, [`scan`] the full/prefix/range
//! walks.
//!
//! ## Lazy open (N8.4)
//!
//! [`BlockSstFile::load`] reads **only** the header, footer, block index and
//! bloom filter — never a data block. That is O(metadata), independent of
//! how much payload the file holds (ADR-039 §8.1's exit criterion: RSS/open
//! cost bounded by metadata + cache, never proportional to data). Every data
//! block is read on demand, through exactly three paths:
//!
//! - [`BlockSstFile::get`] — a point lookup: bloom filter (zero I/O on a
//!   miss) -> binary search the block index by `last_key` -> read the
//!   *single* candidate block -> binary search within it. Structurally
//!   cannot touch more than one data block; see `store::engine::Engine::get`
//!   for how this feeds the `point_lookup_full_sst_read` invariant counter
//!   (ADR-039 §4/§5.5).
//! - [`BlockSstFile::entries_with_prefix`] — a prefix scan: binary search
//!   the block index for the range of blocks overlapping the prefix, decode
//!   only those (ADR-039 §4's index-driven scan; N8.11).
//! - [`BlockSstFile::entries`] — a full scan, every block in order. The
//!   *only* legitimate caller of a full walk: compaction, which by nature
//!   must see every key. `get` never calls this.
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

use std::fs;
use std::path::{Path, PathBuf};

use crate::crypto::CryptoContext;
use crate::error::{EngineError, Result};
use crate::format::sst_block::SstBlockIndexEntry;

mod bloom;
mod read;
mod scan;
#[cfg(test)]
mod test_support;
mod write;

use bloom::Bloom;

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

/// Shared by [`write::flush_block`]/`write_new` (computed once per new file)
/// and [`read`]'s `load` (recomputed from the resident index at open) — the
/// same sum, needed on both sides of the writer/reader boundary.
fn sum_tombstones(block_index: &[SstBlockIndexEntry]) -> u64 {
    block_index.iter().map(|e| u64::from(e.tombstone_count)).sum()
}

pub(crate) fn sst_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id:020}.sst"))
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
    use super::test_support::entries;
    use super::*;
    use crate::key::Key;

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
