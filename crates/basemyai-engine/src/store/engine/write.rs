// SPDX-License-Identifier: BUSL-1.1
//! The ingestion path: `put`/`delete`/`apply_batch` (WAL append-and-fsync
//! before the memtable is touched, ADR-025), plus the auto-flush trigger
//! and the WAL-record counter bookkeeping every write op shares.

use crate::error::Result;
use crate::format::wal::{BatchOp, WalOp};
use crate::key::Key;

use super::{Batch, Engine};

impl Engine {
    /// Inserts or overwrites `key`. Durable once this returns `Ok` — the WAL
    /// record is fsynced before the memtable is updated.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let written = self.wal.append(WalOp::Put, key, Some(value))?;
        self.note_wal_record(written);
        self.memtable.put(Key::from(key), value.to_vec());
        self.maybe_flush()
    }

    /// Deletes `key` (a no-op if it wasn't present). Durable once this
    /// returns `Ok`.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        let written = self.wal.append(WalOp::Delete, key, None)?;
        self.note_wal_record(written);
        self.memtable.delete(Key::from(key));
        self.maybe_flush()
    }

    /// Applies every operation in `batch` atomically: on reopen after a
    /// crash, either all of them are visible or none are — never a partial
    /// subset. A no-op (does not touch the WAL at all) if `batch` is empty.
    ///
    /// Durability/atomicity comes entirely from the WAL framing: the whole
    /// batch is appended as a single `Batch` WAL record (one `write_all` +
    /// one `sync_all`, one checksum over every sub-operation — see
    /// `format::wal`'s "Batch records" section and `store::wal::Wal::
    /// append_batch`), so replay either finds the complete record and
    /// applies every sub-operation, or finds a torn trailing record and
    /// applies none of them — the same torn-tail tolerance the engine
    /// already relies on for single `put`/`delete` records, just covering
    /// the whole batch's bytes instead of one op's.
    pub fn apply_batch(&mut self, batch: &Batch) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let wal_ops: Vec<BatchOp> = batch
            .ops
            .iter()
            .map(|(key, value)| BatchOp {
                op: if value.is_some() { WalOp::Put } else { WalOp::Delete },
                key: key.as_bytes().to_vec(),
                value: value.clone(),
            })
            .collect();
        let written = self.wal.append_batch(&wal_ops)?;
        self.note_wal_record(written);

        for (key, value) in &batch.ops {
            match value {
                Some(v) => self.memtable.put(key.clone(), v.clone()),
                None => self.memtable.delete(key.clone()),
            }
        }
        self.maybe_flush()
    }

    pub(super) fn maybe_flush(&mut self) -> Result<()> {
        if self.memtable.len() >= self.options.memtable_flush_threshold {
            self.flush()?;
        }
        Ok(())
    }

    pub(super) fn note_wal_record(&mut self, bytes_written: u64) {
        self.counters.wal_records += 1;
        self.counters.bytes_written += bytes_written;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::engine::test_support::KEY;

    #[test]
    fn encrypted_batch_is_atomic_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(dir.path(), KEY).expect("open");
            let mut batch = Batch::new();
            batch.put(b"a", b"1");
            batch.put(b"b", b"2");
            batch.delete(b"a");
            engine.apply_batch(&batch).expect("apply");
        }
        let engine = Engine::open_encrypted(dir.path(), KEY).expect("reopen");
        assert_eq!(engine.get(b"a").expect("get"), None);
        assert_eq!(engine.get(b"b").expect("get").as_deref(), Some(&b"2"[..]));
    }
}
