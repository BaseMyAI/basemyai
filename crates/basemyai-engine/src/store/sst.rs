// SPDX-License-Identifier: BUSL-1.1
//! SST file I/O: crash-safe write (temp file, fsync, rename into place) and a
//! load-into-memory read path. Phase A has no block index or bloom filter —
//! see `docs/adr/ADR-025-native-engine-storage-foundation.md` and the N1
//! spike for why that's an accepted simplification here, not an oversight.
//!
//! Encryption at rest (ADR-030 §3): with a [`CryptoContext`], the whole
//! plain SST body is sealed into one `SstEnvelope` before the crash-safe
//! write, and unsealed after the whole-file read — adequate because this
//! phase reads SSTs whole (no block reads to encrypt individually). The
//! tmp/fsync/rename sequence itself is unchanged.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::crypto::CryptoContext;
use crate::error::{EngineError, Result};
use crate::format::crypto as envelope;
use crate::format::sst::{self, SstEntry};
use crate::key::Key;
use crate::store::Value;

pub(crate) struct SstFile {
    pub(crate) id: u64,
    pub(crate) path: PathBuf,
    /// Sorted ascending by key (guaranteed by whoever built the file).
    entries: Vec<(Key, Option<Value>)>,
}

impl SstFile {
    /// Writes `entries` (already sorted ascending by key) to a new SST file
    /// identified by `id` inside `dir`, following the crash-safe sequence:
    /// write to a temp path, fsync, then rename into place. The caller (the
    /// `Engine` flush path) must not truncate the WAL until this returns
    /// `Ok` (ADR-025 ordering rule).
    pub(crate) fn write_new(
        dir: &Path,
        id: u64,
        entries: Vec<(Key, Option<Value>)>,
        crypto: Option<&CryptoContext>,
    ) -> Result<Self> {
        let final_path = sst_path(dir, id);
        let tmp_path = final_path.with_extension("sst.tmp");

        let encoded_entries: Vec<SstEntry> = entries
            .iter()
            .map(|(k, v)| SstEntry {
                key: k.as_bytes().to_vec(),
                value: v.clone(),
            })
            .collect();
        let mut bytes = sst::encode(&encoded_entries);
        if let Some(crypto) = crypto {
            let sealed = crypto.seal(&bytes, &envelope::sst_envelope_aad())?;
            bytes = envelope::encode_sst_envelope(&sealed.nonce, &sealed.ciphertext);
        }

        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
            file.write_all(&bytes)
                .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
            file.sync_all().map_err(|e| EngineError::io(tmp_path.clone(), e))?;
        }
        fs::rename(&tmp_path, &final_path).map_err(|e| EngineError::io(final_path.clone(), e))?;
        // Known deviation from the textbook crash-safe rename recipe: this
        // does not additionally fsync the containing directory entry after
        // the rename (opening a directory as a `File` to fsync it isn't
        // portable on Windows, the primary dev/CI target here). This is not
        // a correctness gap for the WAL-truncate ordering rule: if the
        // rename itself doesn't survive a crash, the SST simply doesn't
        // exist on reopen and the data is replayed back out of the (not yet
        // truncated) WAL instead. It is a known gap for the *next* step —
        // the crash-consistency harness should exercise this directly
        // before it's trusted further.
        Ok(Self {
            id,
            path: final_path,
            entries,
        })
    }

    /// Loads and verifies an existing SST file from disk — unsealing its
    /// envelope first when `crypto` is `Some`. An AEAD failure on a complete
    /// file is corruption/tampering (the key was already verified against
    /// `crypto.meta` at open), reported as [`EngineError::CorruptSst`].
    pub(crate) fn load(path: PathBuf, id: u64, crypto: Option<&CryptoContext>) -> Result<Self> {
        let mut file = File::open(&path).map_err(|e| EngineError::io(path.clone(), e))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|e| EngineError::io(path.clone(), e))?;
        let plain;
        let body: &[u8] = match crypto {
            Some(crypto) => {
                let (nonce, ciphertext) = envelope::decode_sst_envelope(&buf, &path)?;
                plain = crypto
                    .open(&nonce, ciphertext, &envelope::sst_envelope_aad())
                    .ok_or_else(|| EngineError::CorruptSst {
                        path: path.clone(),
                        reason: "envelope failed AEAD authentication (tampered or corrupt)".to_string(),
                    })?;
                &plain
            }
            None => &buf,
        };
        let decoded = sst::decode(body, &path)?;
        let entries = decoded.into_iter().map(|e| (Key::from(e.key), e.value)).collect();
        Ok(Self { id, path, entries })
    }

    /// Point lookup. `Some(None)` = tombstone (definitively deleted at this
    /// layer); `Some(Some(v))` = value; `None` = absent from this SST.
    pub(crate) fn get(&self, key: &Key) -> Option<Option<&Value>> {
        self.entries
            .binary_search_by(|(k, _)| k.cmp(key))
            .ok()
            .map(|idx| self.entries[idx].1.as_ref())
    }

    pub(crate) fn entries(&self) -> &[(Key, Option<Value>)] {
        &self.entries
    }
}

pub(crate) fn sst_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id:020}.sst"))
}

/// Scans `dir` for existing `*.sst` files (ignores `*.sst.tmp` orphans left
/// by a crash between write and rename) and returns them loaded, sorted
/// ascending by id (oldest first).
pub(crate) fn scan_existing(dir: &Path, crypto: Option<&CryptoContext>) -> Result<Vec<SstFile>> {
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
        found.push(SstFile::load(path, id, crypto)?);
    }
    found.sort_by_key(|s| s.id);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_crypto(dir: &Path) -> CryptoContext {
        crate::crypto::create_meta(dir, b"sst test key").expect("create crypto meta")
    }

    #[test]
    fn write_then_load_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entries = vec![
            (Key::from(&b"a"[..]), Some(b"1".to_vec())),
            (Key::from(&b"b"[..]), None),
        ];
        let written = SstFile::write_new(dir.path(), 0, entries.clone(), None).expect("write");
        let loaded = SstFile::load(written.path, 0, None).expect("load");
        assert_eq!(loaded.entries(), entries.as_slice());
    }

    #[test]
    fn get_finds_put_and_tombstone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entries = vec![
            (Key::from(&b"a"[..]), Some(b"1".to_vec())),
            (Key::from(&b"b"[..]), None),
        ];
        let sst = SstFile::write_new(dir.path(), 0, entries, None).expect("write");
        assert_eq!(sst.get(&Key::from(&b"a"[..])), Some(Some(&b"1".to_vec())));
        assert_eq!(sst.get(&Key::from(&b"b"[..])), Some(None));
        assert_eq!(sst.get(&Key::from(&b"missing"[..])), None);
    }

    #[test]
    fn scan_existing_ignores_tmp_orphans() {
        let dir = tempfile::tempdir().expect("tempdir");
        SstFile::write_new(dir.path(), 0, vec![(Key::from(&b"a"[..]), Some(b"1".to_vec()))], None).expect("write");
        // Simulate an orphaned tmp file left by a crash between write and rename.
        std::fs::write(dir.path().join("00000000000000000001.sst.tmp"), b"garbage").expect("write orphan");

        let found = scan_existing(dir.path(), None).expect("scan");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, 0);
    }

    #[test]
    fn scan_existing_sorts_ascending_by_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        SstFile::write_new(dir.path(), 5, vec![(Key::from(&b"a"[..]), Some(b"1".to_vec()))], None).expect("write");
        SstFile::write_new(dir.path(), 2, vec![(Key::from(&b"b"[..]), Some(b"2".to_vec()))], None).expect("write");

        let found = scan_existing(dir.path(), None).expect("scan");
        let ids: Vec<_> = found.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![2, 5]);
    }

    #[test]
    fn encrypted_write_then_load_roundtrips_and_hides_plaintext() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        let entries = vec![
            (Key::from(&b"visible-key"[..]), Some(b"secret-value".to_vec())),
            (Key::from(&b"tombstoned"[..]), None),
        ];
        let written = SstFile::write_new(dir.path(), 0, entries.clone(), Some(&crypto)).expect("write");
        let raw = std::fs::read(&written.path).expect("read raw sst");
        for needle in [&b"visible-key"[..], b"secret-value", b"tombstoned"] {
            assert!(
                !raw.windows(needle.len()).any(|w| w == needle),
                "plaintext {needle:?} leaked into the encrypted SST file"
            );
        }
        let loaded = SstFile::load(written.path, 0, Some(&crypto)).expect("load");
        assert_eq!(loaded.entries(), entries.as_slice());
    }

    #[test]
    fn encrypted_load_rejects_tampered_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        let written = SstFile::write_new(
            dir.path(),
            0,
            vec![(Key::from(&b"a"[..]), Some(b"1".to_vec()))],
            Some(&crypto),
        )
        .expect("write");
        let mut raw = std::fs::read(&written.path).expect("read");
        let last = raw.len() - 1;
        raw[last] ^= 0xFF;
        std::fs::write(&written.path, &raw).expect("write tampered");
        let Err(err) = SstFile::load(written.path, 0, Some(&crypto)) else {
            panic!("tampered sst must fail")
        };
        assert!(matches!(err, EngineError::CorruptSst { .. }));
    }

    #[test]
    fn plaintext_sst_read_in_encrypted_mode_is_loud_corruption() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crypto = test_crypto(dir.path());
        let written =
            SstFile::write_new(dir.path(), 0, vec![(Key::from(&b"a"[..]), Some(b"1".to_vec()))], None).expect("write");
        let Err(err) = SstFile::load(written.path, 0, Some(&crypto)) else {
            panic!("mode mismatch must be loud")
        };
        assert!(matches!(err, EngineError::CorruptSst { .. }));
    }
}
