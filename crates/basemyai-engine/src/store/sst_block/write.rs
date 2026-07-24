// SPDX-License-Identifier: BUSL-1.1
//! The block-based SST writer (ADR-039, N8.3): splits `entries` into data
//! blocks once the accumulated wire size reaches `block_size`, seals each
//! section under per-block AEAD when encrypted (N8.8), and commits the
//! whole file via the crate's standard temp-file + fsync + rename sequence.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use crate::crypto::CryptoContext;
use crate::error::{EngineError, Result};
use crate::fail_point;
use crate::format::crypto::{self as envelope, SstSectionType};
use crate::format::sst_block::{self, SST_HEADER_TOTAL_LEN, SstBlockIndexEntry, SstEntry, SstFooter, SstHeader};
use crate::key::Key;
use crate::store::Value;

use super::bloom::Bloom;
use super::{BlockSstFile, sst_path, sum_tombstones};

/// Seals `plain` as one section if `crypto` is `Some`, else passes it
/// through unchanged. `section`/`section_no` become part of the AAD
/// (ADR-039 §3) — the caller must pass the same coordinates it will read
/// back with, or `open_section` (the reader's inverse, in [`super::read`])
/// will (correctly) fail authentication.
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
        // ENG-DUR-003: fsync the containing directory so this rename's
        // directory-entry mutation is itself durable before the caller
        // truncates the WAL on the strength of it (`Engine::flush`) — see
        // `crate::fs_util` for why the rename alone is not enough.
        crate::fs_util::sync_dir(dir)?;

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
}

#[cfg(test)]
mod tests {
    use super::super::test_support::entries;
    use super::*;

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
}
