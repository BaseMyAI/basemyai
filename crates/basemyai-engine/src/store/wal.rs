//! WAL file I/O: append-with-fsync, replay-on-open (torn-tail tolerant), and
//! the truncate-after-flush step of the crash-safe flush sequence.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use crate::error::{EngineError, Result};
use crate::format::wal::{self, BatchOp, WalOp, WalRecord};

pub(crate) struct Wal {
    path: PathBuf,
    file: File,
}

impl Wal {
    /// Opens (creating if absent) the WAL file for appending.
    ///
    /// Deliberately does *not* use `OpenOptions::append(true)`: on Windows
    /// that grants only `FILE_APPEND_DATA`, not `FILE_WRITE_DATA` — which
    /// makes `set_len` (the truncate step of [`Wal::reset`]) fail with
    /// access-denied on the very same handle. Opening for plain read+write
    /// and seeking to end-of-file before each append keeps one handle usable
    /// for both operations on every platform.
    pub(crate) fn open_for_append(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| EngineError::io(path.clone(), e))?;
        Ok(Self { path, file })
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
            match wal::decode(&buf[offset..], &self.path)? {
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
        self.file
            .seek(SeekFrom::End(0))
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.file
            .write_all(record)
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

    #[test]
    fn append_then_replay_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open_for_append(&path).expect("open");
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
        let mut wal = Wal::open_for_append(&path).expect("open");
        wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        wal.reset().expect("reset");
        assert!(wal.replay().expect("replay").is_empty());
    }

    #[test]
    fn replay_tolerates_torn_trailing_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        {
            let mut wal = Wal::open_for_append(&path).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        }
        // Simulate a crash mid-append: append a few extra garbage bytes that
        // don't form a complete record.
        {
            let mut file = OpenOptions::new().write(true).open(&path).expect("reopen");
            file.seek(SeekFrom::End(0)).expect("seek to end");
            file.write_all(&[0xAA, 0xBB, 0xCC]).expect("write garbage");
        }
        let wal = Wal::open_for_append(&path).expect("reopen");
        let records = wal.replay().expect("replay tolerates torn tail");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, b"a");
    }
}
