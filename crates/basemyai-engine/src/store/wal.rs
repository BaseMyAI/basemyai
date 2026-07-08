// SPDX-License-Identifier: BUSL-1.1
//! WAL file I/O: append-with-fsync, replay-on-open (torn-tail tolerant), and
//! the truncate-after-flush step of the crash-safe flush sequence.
//!
//! Encryption at rest (ADR-030 §3) plugs in *between* record encoding
//! (`format::wal`) and file I/O: with a [`CryptoContext`], every encoded
//! record (batches included — a batch is already one outer record) is
//! sealed into one `WalEnvelope` before hitting the file, and replay peels
//! envelopes back off with the exact same torn-tail contract as plaintext.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use crate::crypto::CryptoContext;
use crate::error::{EngineError, Result};
use crate::format::crypto as envelope;
use crate::format::wal::{self, BatchOp, WalOp, WalRecord};

pub(crate) struct Wal {
    path: PathBuf,
    file: File,
    /// `Some` = every record is sealed into a `WalEnvelope` (ADR-030 §3).
    crypto: Option<CryptoContext>,
}

impl Wal {
    /// Opens (creating if absent) the WAL file for appending — encrypted
    /// when `crypto` is `Some` (the mode is decided once per store by
    /// `Engine::open*` from `crypto.meta`'s presence, never per file).
    ///
    /// Deliberately does *not* use `OpenOptions::append(true)`: on Windows
    /// that grants only `FILE_APPEND_DATA`, not `FILE_WRITE_DATA` — which
    /// makes `set_len` (the truncate step of [`Wal::reset`]) fail with
    /// access-denied on the very same handle. Opening for plain read+write
    /// and seeking to end-of-file before each append keeps one handle usable
    /// for both operations on every platform.
    pub(crate) fn open_for_append(path: impl Into<PathBuf>, crypto: Option<CryptoContext>) -> Result<Self> {
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| EngineError::io(path.clone(), e))?;
        Ok(Self { path, file, crypto })
    }

    /// Reads every fully-formed, checksum-valid record from the start of the
    /// file, expanding any `Batch` record into its individual `Put`/`Delete`
    /// sub-operations in order. Stops silently (does not error) at the first
    /// *outer* record that isn't fully present — the expected shape of a
    /// torn trailing write left by a crash mid-append. Because a batch is
    /// always written as one outer record (see the "Batch records" section
    /// of `format::wal`), a torn batch is dropped in its entirety here: its
    /// outer record never decodes as `Some`, so none of its sub-operations
    /// are ever pushed — all-or-nothing falls out of this loop for free,
    /// with no special-casing needed. A fully-buffered record with a bad
    /// checksum, or a fully-buffered `Batch` record with a structurally
    /// malformed nested payload, is a genuine error (see [`wal::decode`] /
    /// [`wal::decode_batch`]).
    pub(crate) fn replay(&self) -> Result<Vec<WalRecord>> {
        let mut buf = Vec::new();
        let mut file = File::open(&self.path).map_err(|e| EngineError::io(self.path.clone(), e))?;
        file.read_to_end(&mut buf)
            .map_err(|e| EngineError::io(self.path.clone(), e))?;

        let mut records = Vec::new();
        let mut offset = 0usize;
        while offset < buf.len() {
            match self.decode_next(&buf[offset..])? {
                Some((record, consumed)) => {
                    offset += consumed;
                    match record.op {
                        WalOp::Batch => {
                            let payload = record.value.unwrap_or_default();
                            let batch_ops = wal::decode_batch(&payload, &self.path)?;
                            records.extend(batch_ops.into_iter().map(|entry| WalRecord {
                                op: entry.op,
                                key: entry.key,
                                value: entry.value,
                            }));
                        }
                        WalOp::Put | WalOp::Delete => records.push(record),
                    }
                }
                None => {
                    if offset != buf.len() {
                        tracing::warn!(
                            path = %self.path.display(),
                            remaining = buf.len() - offset,
                            "WAL replay stopped at a torn trailing record (expected after a crash mid-append)"
                        );
                    }
                    break;
                }
            }
        }
        Ok(records)
    }

    /// Decodes the next record from `buf` in this WAL's mode. Plaintext:
    /// straight [`wal::decode`]. Encrypted: peel one `WalEnvelope`
    /// (incomplete envelope = torn tail, `Ok(None)`), open the seal (a
    /// failure here is corruption — the key was already verified against
    /// `crypto.meta` at open), then decode the recovered plaintext record,
    /// which must be complete and consume its buffer exactly (the envelope
    /// sealed exactly one record's bytes).
    fn decode_next(&self, buf: &[u8]) -> Result<Option<(WalRecord, usize)>> {
        let Some(crypto) = &self.crypto else {
            return wal::decode(buf, &self.path);
        };
        let Some((nonce, ciphertext, consumed)) = envelope::decode_wal_envelope(buf, &self.path)? else {
            return Ok(None);
        };
        let plaintext = crypto
            .open(&nonce, ciphertext, &envelope::wal_envelope_aad())
            .ok_or_else(|| EngineError::CorruptWal {
                path: self.path.clone(),
                reason: "envelope failed AEAD authentication (tampered or corrupt)".to_string(),
            })?;
        match wal::decode(&plaintext, &self.path)? {
            Some((record, inner_consumed)) if inner_consumed == plaintext.len() => Ok(Some((record, consumed))),
            _ => Err(EngineError::CorruptWal {
                path: self.path.clone(),
                reason: "authenticated envelope does not contain exactly one WAL record".to_string(),
            }),
        }
    }

    /// Appends one record and fsyncs before returning — the operation is
    /// durable once this call succeeds. Explicitly seeks to end-of-file
    /// first (see [`Wal::open_for_append`] for why this isn't `O_APPEND`).
    pub(crate) fn append(&mut self, op: WalOp, key: &[u8], value: Option<&[u8]>) -> Result<()> {
        let record = wal::encode(op, key, value);
        self.write_record(&record)
    }

    /// Appends a whole batch as a single outer `Batch` record and fsyncs
    /// before returning. Durability here is all-or-nothing for the *whole*
    /// batch: exactly one `write_all` + one `sync_all`, covered by one
    /// checksum (see the "Batch records" section of `format::wal`), so a
    /// crash can only ever leave either every sub-operation absent (record
    /// never made it) or every sub-operation present (record fully synced)
    /// — never a partial subset.
    pub(crate) fn append_batch(&mut self, ops: &[BatchOp]) -> Result<()> {
        let payload = wal::encode_batch(ops);
        let record = wal::encode(WalOp::Batch, &[], Some(&payload));
        self.write_record(&record)
    }

    fn write_record(&mut self, record: &[u8]) -> Result<()> {
        let sealed;
        let on_disk: &[u8] = match &self.crypto {
            Some(crypto) => {
                let (nonce, ciphertext) = crypto.seal(record, &envelope::wal_envelope_aad())?;
                sealed = envelope::encode_wal_envelope(&nonce, &ciphertext);
                &sealed
            }
            None => record,
        };
        self.file
            .seek(SeekFrom::End(0))
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.file
            .write_all(on_disk)
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.file
            .sync_all()
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        Ok(())
    }

    /// Truncates the WAL to empty and fsyncs. Callers must only invoke this
    /// *after* the corresponding SST has been fsynced and durably renamed
    /// into place — never before (ADR-025 ordering rule).
    pub(crate) fn reset(&mut self) -> Result<()> {
        self.file
            .set_len(0)
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.file
            .sync_all()
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_crypto(dir: &std::path::Path) -> CryptoContext {
        crate::crypto::create_meta(dir, b"wal test key").expect("create crypto meta")
    }

    #[test]
    fn append_then_replay_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open_for_append(&path, None).expect("open");
        wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        wal.append(WalOp::Delete, b"a", None).expect("append");

        let records = wal.replay().expect("replay");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].op, WalOp::Put);
        assert_eq!(records[1].op, WalOp::Delete);
        assert_eq!(wal.path(), path.as_path());
    }

    #[test]
    fn reset_truncates_to_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open_for_append(&path, None).expect("open");
        wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        wal.reset().expect("reset");
        assert!(wal.replay().expect("replay").is_empty());
    }

    #[test]
    fn replay_tolerates_torn_trailing_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        {
            let mut wal = Wal::open_for_append(&path, None).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        }
        // Simulate a crash mid-append: append a few extra garbage bytes that
        // don't form a complete record.
        {
            let mut file = OpenOptions::new().write(true).open(&path).expect("reopen");
            file.seek(SeekFrom::End(0)).expect("seek to end");
            file.write_all(&[0xAA, 0xBB, 0xCC]).expect("write garbage");
        }
        let wal = Wal::open_for_append(&path, None).expect("reopen");
        let records = wal.replay().expect("replay tolerates torn tail");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, b"a");
    }

    #[test]
    fn encrypted_append_then_replay_roundtrips_and_hides_plaintext() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let crypto = test_crypto(dir.path());
        let mut wal = Wal::open_for_append(&path, Some(crypto)).expect("open");
        wal.append(WalOp::Put, b"visible-key", Some(b"secret-value"))
            .expect("append");
        wal.append_batch(&[BatchOp {
            op: WalOp::Put,
            key: b"batched-key".to_vec(),
            value: Some(b"batched-secret".to_vec()),
        }])
        .expect("append batch");

        let records = wal.replay().expect("replay");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].key, b"visible-key");
        assert_eq!(records[0].value.as_deref(), Some(&b"secret-value"[..]));
        assert_eq!(records[1].key, b"batched-key");

        // Neither keys nor values may appear anywhere in the raw file.
        let raw = std::fs::read(&path).expect("read raw wal");
        for needle in [&b"visible-key"[..], b"secret-value", b"batched-key", b"batched-secret"] {
            assert!(
                !raw.windows(needle.len()).any(|w| w == needle),
                "plaintext {needle:?} leaked into the encrypted WAL file"
            );
        }
    }

    #[test]
    fn encrypted_replay_tolerates_torn_trailing_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let crypto = test_crypto(dir.path());
        {
            let mut wal = Wal::open_for_append(&path, Some(crypto.clone())).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        }
        // Truncate mid-envelope: everything after the first envelope's
        // header start is a torn trailing write.
        let full = std::fs::read(&path).expect("read");
        std::fs::write(&path, &full[..full.len() - 3]).expect("truncate");
        {
            let mut wal = Wal::open_for_append(&path, Some(crypto)).expect("reopen");
            assert!(wal.replay().expect("torn tail tolerated").is_empty());
            // The store keeps working after the torn tail.
            wal.append(WalOp::Put, b"b", Some(b"2")).expect("append after torn");
        }
    }

    #[test]
    fn encrypted_replay_rejects_tampered_complete_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let crypto = test_crypto(dir.path());
        {
            let mut wal = Wal::open_for_append(&path, Some(crypto.clone())).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        }
        // Flip one ciphertext byte in an otherwise complete envelope: the
        // Poly1305 tag must fail loudly, never decode garbage.
        let mut raw = std::fs::read(&path).expect("read");
        let last = raw.len() - 1;
        raw[last] ^= 0xFF;
        std::fs::write(&path, &raw).expect("write tampered");
        let wal = Wal::open_for_append(&path, Some(crypto)).expect("reopen");
        let err = wal.replay().expect_err("tampered envelope must fail");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }
}
