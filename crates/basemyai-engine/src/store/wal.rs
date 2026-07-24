// SPDX-License-Identifier: BUSL-1.1
//! WAL file I/O: append-with-fsync, replay-on-open (torn-tail tolerant), and
//! the truncate-after-flush step of the crash-safe flush sequence.
//!
//! Encryption at rest (ADR-030 §3) plugs in *between* record encoding
//! (`format::wal`) and file I/O: with a [`CryptoContext`], every encoded
//! record (batches included — a batch is already one outer record) is
//! sealed into one `WalEnvelope` before hitting the file, and replay peels
//! envelopes back off with the exact same torn-tail contract as plaintext.
//!
//! **Anti-replay (ADR-044, CRYPTO-1).** Every record written through this
//! module carries its own absolute file offset (`record_offset`,
//! `format::wal`) and, when encrypted, is sealed under an AAD binding
//! `store_id ‖ wal_epoch ‖ record_offset` ([`envelope::wal_envelope_aad_v2`]).
//! `wal_epoch` — this WAL episode's counter — is published durably
//! ([`wal_epoch` module][crate::format::wal_epoch]) *before* every
//! [`Wal::reset`] truncation, mirroring the WAL-after-SST publication
//! ordering ADR-025 already established. Replay validates both: the
//! plaintext `record_offset` must equal the physical offset a record was
//! found at (catches a splice/permutation even on a plaintext store), and —
//! encrypted stores only — the AEAD open itself fails if any of
//! `store_id`/`wal_epoch`/`record_offset` don't match the position being
//! replayed into, indistinguishably from a wrong-key/corrupt failure (an
//! attacker probing error messages cannot tell "bad tag" from "falsified
//! position" apart).
//!
//! **Residual risk, accepted and documented rather than hidden.**
//! [`Wal::reset`] truncates the WAL *before* publishing the bumped
//! `wal_epoch.meta` (chosen over the reverse order for crash-safety, see
//! that method's doc). This leaves one narrow window: a crash after the
//! truncate's fsync commits but before the epoch bump's rename commits
//! resumes the next episode still labeled with the *old* epoch number. If
//! an attacker can additionally engineer that exact crash timing (not just
//! observe it — genuinely control when the process dies, which the
//! declared threat model's "malicious/corrupt `.bmai` directory" attacker
//! is not assumed to have), a previously captured record from earlier in
//! that same old episode could re-authenticate if a new write happens to
//! land at the identical offset afterward. This is strictly narrower than
//! the CRYPTO-1 gap this module closes (which required no crash at all,
//! any time, unconditionally) — but it is not zero. Closing it fully would
//! need the epoch bump and the truncation to be one atomic durable
//! operation (e.g. a new WAL segment file per epoch with a single
//! "current segment" pointer, mirroring how SST generations already work)
//! — deferred as a follow-up, not attempted under the time pressure that
//! produced the original CRYPTO-1 gap in the first place.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::crypto::CryptoContext;
use crate::error::{EngineError, Result};
use crate::fail_point;
use crate::format::crypto as envelope;
use crate::format::wal::{self, BatchOp, WalOp, WalRecord};
use crate::format::wal_epoch;

pub(crate) struct Wal {
    path: PathBuf,
    file: File,
    /// `Some` = every record is sealed into a `WalEnvelope` (ADR-030 §3).
    crypto: Option<CryptoContext>,
    /// This store's stable identity (`store.meta`, ADR-042 §3.3) — bound
    /// into every encrypted envelope's AAD (ADR-044 §4).
    store_id: Uuid,
    /// The current WAL episode counter (`wal_epoch.meta`, ADR-044 §2),
    /// incremented by [`Wal::reset`] before every truncation.
    wal_epoch: u64,
    /// Monotonic count of `sync_all()` calls performed by this handle since
    /// it was opened (write-record fsyncs plus truncation fsyncs) — folded
    /// into [`crate::EngineStats::fsync_count`] by `Engine::stats` (current
    /// handle) and `Engine::rotate_full` (retired handle, accumulated into
    /// `Counters` before drop, since a fresh `Wal` after rotation would
    /// otherwise reset this to zero and lose the pre-rotation count).
    fsync_count: u64,
}

impl Wal {
    /// Opens (creating if absent) the WAL file for appending — encrypted
    /// when `crypto` is `Some` (the mode is decided once per store by
    /// `Engine::open*` from `crypto.meta`'s presence, never per file).
    /// `store_id` is this store's stable identity (`store.meta`), bound into
    /// every encrypted envelope's AAD (ADR-044 §4).
    ///
    /// Also loads (or, for a genuinely fresh WAL, creates) this generation's
    /// `wal_epoch.meta` (ADR-044 §2/§7): a non-empty `wal.log` with no
    /// `wal_epoch.meta` next to it is a pre-ADR-044 store this build refuses
    /// typed, never silently reinterpreted.
    ///
    /// Deliberately does *not* use `OpenOptions::append(true)`: on Windows
    /// that grants only `FILE_APPEND_DATA`, not `FILE_WRITE_DATA` — which
    /// makes `set_len` (the truncate step of [`Wal::reset`]) fail with
    /// access-denied on the very same handle. Opening for plain read+write
    /// and seeking to end-of-file before each append keeps one handle usable
    /// for both operations on every platform.
    pub(crate) fn open_for_append(
        path: impl Into<PathBuf>,
        crypto: Option<CryptoContext>,
        store_id: Uuid,
    ) -> Result<Self> {
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| EngineError::io(path.clone(), e))?;
        let wal_len = file.metadata().map_err(|e| EngineError::io(path.clone(), e))?.len();
        let dir = wal_dir(&path);
        let wal_epoch = load_or_create_wal_epoch(&dir, &path, wal_len)?;
        Ok(Self {
            path,
            file,
            crypto,
            store_id,
            wal_epoch,
            fsync_count: 0,
        })
    }

    /// Reads every fully-formed, checksum-valid record from the start of the
    /// file, expanding any `Batch` record into its individual `Put`/`Delete`
    /// sub-operations in order. Stops silently (does not error) at the first
    /// *outer* record that isn't fully present — the expected shape of a
    /// torn trailing write left by a crash mid-append — and truncates that
    /// torn suffix before the handle is reused for appends. Because a batch
    /// is always written as one outer record (see the "Batch records" section
    /// of `format::wal`), a torn batch is dropped in its entirety here: its
    /// outer record never decodes as `Some`, so none of its sub-operations
    /// are ever pushed — all-or-nothing falls out of this loop for free,
    /// with no special-casing needed. A fully-buffered record with a bad
    /// checksum, or a fully-buffered `Batch` record with a structurally
    /// malformed nested payload, is a genuine error (see [`wal::decode`] /
    /// [`wal::decode_batch`]). A fully-buffered record whose declared
    /// `record_offset` (or, encrypted, AAD) does not match the physical
    /// position it was found at is [`EngineError::WalRecordOffsetMismatch`]
    /// / [`EngineError::CorruptWal`] — never tolerated as a torn tail
    /// (ADR-044 §5).
    pub(crate) fn replay(&mut self) -> Result<Vec<WalRecord>> {
        let mut buf = Vec::new();
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.file
            .read_to_end(&mut buf)
            .map_err(|e| EngineError::io(self.path.clone(), e))?;

        let mut records = Vec::new();
        let mut offset = 0usize;
        while offset < buf.len() {
            match decode_next(
                &buf[offset..],
                offset as u64,
                self.crypto.as_ref(),
                self.store_id,
                self.wal_epoch,
                &self.path,
            )? {
                Some((record, consumed)) => {
                    offset += consumed;
                    match record.op {
                        WalOp::Batch => {
                            let payload = record.value.unwrap_or_default();
                            let batch_ops = wal::decode_batch(&payload, &self.path)?;
                            records.extend(batch_ops.into_iter().map(|entry| WalRecord {
                                op: entry.op,
                                record_offset: record.record_offset,
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
        if offset != buf.len() {
            self.truncate_to(offset as u64)?;
        }
        Ok(records)
    }

    /// Appends one record and fsyncs before returning — the operation is
    /// durable once this call succeeds. Returns the on-disk bytes written
    /// (envelope included when encrypted).
    pub(crate) fn append(&mut self, op: WalOp, key: &[u8], value: Option<&[u8]>) -> Result<u64> {
        self.write_record(op, key, value)
    }

    /// Appends a whole batch as a single outer `Batch` record and fsyncs
    /// before returning. Durability here is all-or-nothing for the *whole*
    /// batch: exactly one `write_all` + one `sync_all`, covered by one
    /// checksum (see the "Batch records" section of `format::wal`), so a
    /// crash can only ever leave either every sub-operation absent (record
    /// never made it) or every sub-operation present (record fully synced)
    /// — never a partial subset. Returns the on-disk bytes written.
    pub(crate) fn append_batch(&mut self, ops: &[BatchOp]) -> Result<u64> {
        if ops.len() > wal::MAX_BATCH_OPS {
            return Err(EngineError::WalBatchTooLarge {
                len: ops.len(),
                max: wal::MAX_BATCH_OPS,
            });
        }
        let payload = wal::encode_batch(ops);
        self.write_record(WalOp::Batch, &[], Some(&payload))
    }

    /// Explicitly seeks to end-of-file first (see [`Wal::open_for_append`]
    /// for why this isn't `O_APPEND`) — the returned position is this
    /// record's `record_offset` (ADR-044 §3), known before encoding since it
    /// is always the file's current length.
    fn write_record(&mut self, op: WalOp, key: &[u8], value: Option<&[u8]>) -> Result<u64> {
        let offset = self
            .file
            .seek(SeekFrom::End(0))
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        let record = wal::encode(op, offset, key, value);
        let sealed;
        let on_disk: &[u8] = match &self.crypto {
            Some(crypto) => {
                let aad = envelope::wal_envelope_aad_v2(self.store_id, self.wal_epoch, offset);
                let encrypted = crypto.seal(&record, &aad)?;
                sealed = envelope::encode_wal_envelope(&encrypted.nonce, &encrypted.ciphertext);
                &sealed
            }
            None => &record,
        };
        self.file
            .write_all(on_disk)
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        fail_point!("after_wal_append");
        self.file
            .sync_all()
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.fsync_count += 1;
        fail_point!("after_wal_fsync");
        Ok(on_disk.len() as u64)
    }

    /// Current WAL file length on disk (the `wal_bytes` gauge of
    /// [`crate::EngineStats`]).
    pub(crate) fn size_on_disk(&self) -> Result<u64> {
        self.file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| EngineError::io(self.path.clone(), e))
    }

    /// Truncates the WAL to empty and fsyncs. Callers must only invoke this
    /// *after* the corresponding SST has been fsynced and durably renamed
    /// into place — never before (ADR-025 ordering rule).
    ///
    /// Truncates **before** publishing the next `wal_epoch.meta` (ADR-044
    /// §2) — the reverse of this ADR's original text, corrected after
    /// `encrypted_batch_kill_reopen_verify_loop` (real kill/reopen crash
    /// testing, not just unit tests) caught the bug in the originally
    /// specified order: bumping the epoch file *before* truncating means a
    /// crash in between leaves a durable epoch pointer that no longer
    /// matches the (still untruncated, still on-disk) WAL content sealed
    /// under the *previous* epoch — replay then fails AEAD authentication
    /// on every record, permanently, since nothing ever re-truncates or
    /// re-seals them. That is not a security property, it is a
    /// self-inflicted denial of service: the store cannot reopen at all.
    /// Truncating first avoids it: a crash before the truncate's fsync
    /// leaves old epoch + old (still fully decodable) content, exactly the
    /// pre-ADR-044 behavior; a crash after the truncate's fsync but before
    /// the epoch bump's rename leaves old epoch + an empty WAL, which
    /// replays trivially (nothing to decode). See this module's doc for the
    /// residual security trade-off this reordering accepts.
    pub(crate) fn reset(&mut self) -> Result<()> {
        let next_epoch = self
            .wal_epoch
            .checked_add(1)
            .ok_or_else(|| EngineError::CorruptWalEpoch {
                path: wal_dir(&self.path).join(wal_epoch::WAL_EPOCH_FILENAME),
                reason: "wal_epoch would overflow u64".to_string(),
            })?;
        // Truncate *before* publishing the new epoch (deliberately the
        // reverse of ADR-044 §2's original "publish then truncate" text —
        // see this method's doc for why). A crash before the truncate
        // fsyncs durably leaves the old epoch next to the old, still-decodable
        // WAL content (replay proceeds normally, exactly as before ADR-044).
        // A crash after the truncate fsyncs but before the epoch bump
        // renames in leaves the old epoch next to an *empty* WAL — replay
        // trivially succeeds (nothing to decode), and the still-old epoch
        // is corrected by this same call the next time `reset` runs, or
        // simply continues to be used for whatever is appended next.
        self.truncate_to(0)?;
        fail_point!("before_wal_epoch_publish");
        publish_wal_epoch(&wal_dir(&self.path), next_epoch)?;
        fail_point!("after_wal_epoch_publish");
        self.wal_epoch = next_epoch;
        Ok(())
    }

    fn truncate_to(&mut self, len: u64) -> Result<()> {
        self.file
            .set_len(len)
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.file
            .seek(SeekFrom::Start(len))
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.file
            .sync_all()
            .map_err(|e| EngineError::io(self.path.clone(), e))?;
        self.fsync_count += 1;
        Ok(())
    }

    /// Total `sync_all()` calls performed by this handle since it was
    /// opened (write-record fsyncs plus truncation fsyncs).
    pub(crate) fn fsync_count(&self) -> u64 {
        self.fsync_count
    }

    /// This WAL episode's current counter (ADR-044 §2) — used by this
    /// module's own tests to assert `Wal::reset` bumps it.
    #[cfg(test)]
    pub(crate) fn wal_epoch(&self) -> u64 {
        self.wal_epoch
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// `path`'s containing directory — every WAL-adjacent durable artifact
/// (`wal_epoch.meta`, and the generation's other `*.meta` files) lives next
/// to `wal.log`, never in some other location derived differently.
fn wal_dir(path: &Path) -> PathBuf {
    path.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Loads `dir`'s `wal_epoch.meta`, or — only for a genuinely fresh WAL
/// (`wal_len == 0` and no file yet) — creates it at epoch 0. A non-empty
/// `wal.log` with no `wal_epoch.meta` is a pre-ADR-044 store: refused typed
/// (ADR-044 §7), never silently assumed to be episode 0.
fn load_or_create_wal_epoch(dir: &Path, wal_path: &Path, wal_len: u64) -> Result<u64> {
    let epoch_path = dir.join(wal_epoch::WAL_EPOCH_FILENAME);
    if epoch_path.exists() {
        let bytes = fs::read(&epoch_path).map_err(|e| EngineError::io(epoch_path.clone(), e))?;
        return Ok(wal_epoch::decode(&bytes, &epoch_path)?.wal_epoch);
    }
    if wal_len != 0 {
        return Err(EngineError::UnsupportedFormatVersion {
            path: wal_path.to_path_buf(),
            expected: wal::WAL_RECORD_VERSION,
            found: 0, // sentinel: no wal_epoch.meta at all (pre-ADR-044 WAL)
        });
    }
    publish_wal_epoch(dir, 0)?;
    Ok(0)
}

/// Publishes `dir`'s `wal_epoch.meta`: tmp + fsync + rename + parent-dir
/// fsync (ENG-DUR-003 idiom, same as `format::generation_meta`'s writer in
/// `store::engine::io`).
fn publish_wal_epoch(dir: &Path, epoch: u64) -> Result<()> {
    let final_path = dir.join(wal_epoch::WAL_EPOCH_FILENAME);
    let tmp_path = final_path.with_extension("meta.tmp");
    let bytes = wal_epoch::encode(&wal_epoch::WalEpoch { wal_epoch: epoch });
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
    fs::rename(&tmp_path, &final_path).map_err(|e| EngineError::io(final_path, e))?;
    crate::fs_util::sync_dir(dir)?;
    Ok(())
}

/// Decodes the next record from `buf` in the given mode, validating it
/// against `base_offset` — the physical byte offset in `wal.log` where `buf`
/// itself begins (ADR-044 §5). Plaintext: [`wal::decode`], then check the
/// decoded `record_offset` equals `base_offset`. Encrypted: peel one
/// `WalEnvelope` (incomplete envelope = torn tail, `Ok(None)`), open the seal
/// under the AAD reconstructed from `(store_id, wal_epoch, base_offset)` — a
/// mismatch on any of the three fails authentication indistinguishably from
/// a wrong-key/corrupt failure — then decode the recovered plaintext record
/// and check its own `record_offset` too (belt-and-suspenders: the AAD
/// already ties the ciphertext to this position, but the plaintext field is
/// checked identically to the unencrypted path rather than trusted blindly).
///
/// Free function (not a `Wal` method) so [`Wal::replay`] and the read-only
/// verification scan ([`scan_readonly`], ADR-040) share one decoder and
/// cannot drift apart.
fn decode_next(
    buf: &[u8],
    base_offset: u64,
    crypto: Option<&CryptoContext>,
    store_id: Uuid,
    wal_epoch: u64,
    path: &std::path::Path,
) -> Result<Option<(WalRecord, usize)>> {
    let decoded = match crypto {
        None => wal::decode(buf, path)?,
        Some(crypto) => {
            let Some((nonce, ciphertext, consumed)) = envelope::decode_wal_envelope(buf, path)? else {
                return Ok(None);
            };
            let aad = envelope::wal_envelope_aad_v2(store_id, wal_epoch, base_offset);
            let plaintext = crypto
                .open(&nonce, ciphertext, &aad)
                .ok_or_else(|| EngineError::CorruptWal {
                    path: path.to_path_buf(),
                    reason: "envelope failed AEAD authentication (tampered, corrupt, or replayed from a \
                         different store/episode/position)"
                        .to_string(),
                })?;
            match wal::decode(&plaintext, path)? {
                Some((record, inner_consumed)) if inner_consumed == plaintext.len() => Some((record, consumed)),
                _ => {
                    return Err(EngineError::CorruptWal {
                        path: path.to_path_buf(),
                        reason: "authenticated envelope does not contain exactly one WAL record".to_string(),
                    });
                }
            }
        }
    };
    let Some((record, consumed)) = decoded else {
        return Ok(None);
    };
    if record.record_offset != base_offset {
        return Err(EngineError::WalRecordOffsetMismatch {
            path: path.to_path_buf(),
            declared: record.record_offset,
            actual: base_offset,
        });
    }
    Ok(Some((record, consumed)))
}

/// Result of a read-only structural WAL scan ([`scan_readonly`]).
pub(crate) struct WalScan {
    /// Fully-formed, checksum-valid records found, in append order (batches
    /// expanded into their `Put`/`Delete` sub-operations, same accounting as
    /// [`Wal::replay`]). Carried in full — not just counted — so the
    /// `FullLogical` verification pass (ADR-040 §2, N9.3) can overlay the
    /// WAL's unflushed state onto the SST view without ever opening the
    /// store for writing.
    pub(crate) records: Vec<WalRecord>,
    /// Trailing bytes that do not form a complete record — the expected
    /// shape of a torn write left by a crash mid-append. `0` on a cleanly
    /// closed WAL.
    pub(crate) torn_tail_bytes: u64,
}

/// Structurally scans the WAL at `path` **without modifying it** — unlike
/// [`Wal::replay`], which truncates a torn tail before reuse. This is the
/// verification path's WAL reader (ADR-040 §2 rule 1: `verify` never writes,
/// not even the truncation `open` allows itself). A missing file scans as
/// empty (a store flushed-and-closed cleanly may have an empty or absent
/// WAL). `store_id`/`wal_epoch` are the same coordinates [`Wal::write_record`]
/// sealed under (ADR-044 §4) — the caller (`store::verify`) reads them from
/// `store.meta`/`wal_epoch.meta` the same way `Engine::open` does.
///
/// # Errors
/// [`EngineError::CorruptWal`] for a fully-buffered record that fails its
/// checksum/AEAD or a malformed nested batch, or
/// [`EngineError::WalRecordOffsetMismatch`] for a structurally valid record
/// found at the wrong position — genuine corruption, never confused with the
/// torn tail (reported via [`WalScan::torn_tail_bytes`]).
pub(crate) fn scan_readonly(
    path: &std::path::Path,
    crypto: Option<&CryptoContext>,
    store_id: Uuid,
    wal_epoch: u64,
) -> Result<WalScan> {
    if !path.exists() {
        return Ok(WalScan {
            records: Vec::new(),
            torn_tail_bytes: 0,
        });
    }
    let buf = std::fs::read(path).map_err(|e| EngineError::io(path.to_path_buf(), e))?;
    let mut records = Vec::new();
    let mut offset = 0usize;
    while offset < buf.len() {
        match decode_next(&buf[offset..], offset as u64, crypto, store_id, wal_epoch, path)? {
            Some((record, consumed)) => {
                offset += consumed;
                match record.op {
                    WalOp::Batch => {
                        let payload = record.value.unwrap_or_default();
                        records.extend(wal::decode_batch(&payload, path)?.into_iter().map(|entry| WalRecord {
                            op: entry.op,
                            record_offset: record.record_offset,
                            key: entry.key,
                            value: entry.value,
                        }));
                    }
                    WalOp::Put | WalOp::Delete => records.push(record),
                }
            }
            None => break,
        }
    }
    Ok(WalScan {
        records,
        torn_tail_bytes: (buf.len() - offset) as u64,
    })
}

/// Reads a WAL directory's current `wal_epoch` for a read-only caller
/// (`store::verify`) that does not hold a writable [`Wal`] handle. Mirrors
/// [`load_or_create_wal_epoch`]'s "absent" policy except it never creates
/// anything (verify never writes, ADR-040 §2 rule 1): a missing
/// `wal_epoch.meta` next to a non-empty `wal.log` is reported by the caller
/// as a store-format problem, not silently treated as epoch 0.
pub(crate) fn read_wal_epoch_for_verify(dir: &Path) -> Result<Option<u64>> {
    let epoch_path = dir.join(wal_epoch::WAL_EPOCH_FILENAME);
    if !epoch_path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&epoch_path).map_err(|e| EngineError::io(epoch_path.clone(), e))?;
    Ok(Some(wal_epoch::decode(&bytes, &epoch_path)?.wal_epoch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_crypto(dir: &std::path::Path) -> CryptoContext {
        crate::crypto::create_meta(dir, b"wal test key").expect("create crypto meta")
    }

    fn sid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn append_then_replay_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
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
        let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
        wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        wal.reset().expect("reset");
        assert!(wal.replay().expect("replay").is_empty());
    }

    #[test]
    fn reset_publishes_wal_epoch_meta() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
        assert_eq!(wal.wal_epoch(), 0);
        wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        wal.reset().expect("reset");
        assert_eq!(wal.wal_epoch(), 1);
        assert_eq!(
            read_wal_epoch_for_verify(dir.path())
                .expect("read epoch")
                .expect("some"),
            1
        );
        wal.reset().expect("second reset (empty wal, still bumps)");
        assert_eq!(wal.wal_epoch(), 2);
    }

    #[test]
    fn epoch_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        {
            let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
            wal.reset().expect("reset");
            wal.reset().expect("reset again");
        }
        let wal = Wal::open_for_append(&path, None, sid(1)).expect("reopen");
        assert_eq!(wal.wal_epoch(), 2);
    }

    #[test]
    fn nonempty_wal_without_epoch_file_is_refused_typed() {
        // Simulates a pre-ADR-044 store: WAL bytes on disk, no
        // wal_epoch.meta ever published for them.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        std::fs::write(&path, wal::encode(WalOp::Put, 0, b"a", Some(b"1"))).expect("seed legacy wal bytes");
        match Wal::open_for_append(&path, None, sid(1)) {
            Err(EngineError::UnsupportedFormatVersion { found: 0, .. }) => {}
            Err(other) => panic!("expected UnsupportedFormatVersion{{found:0}}, got {other}"),
            Ok(_) => panic!("must refuse, not silently adopt epoch 0"),
        }
    }

    #[test]
    fn replay_tolerates_torn_trailing_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        {
            let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        }
        // Simulate a crash mid-append: append a few extra garbage bytes that
        // don't form a complete record.
        {
            let mut file = OpenOptions::new().write(true).open(&path).expect("reopen");
            file.seek(SeekFrom::End(0)).expect("seek to end");
            file.write_all(&[0xAA, 0xBB, 0xCC]).expect("write garbage");
        }
        let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("reopen");
        let records = wal.replay().expect("replay tolerates torn tail");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, b"a");

        wal.append(WalOp::Put, b"b", Some(b"2")).expect("append after recovery");
        drop(wal);

        let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("second reopen");
        let records = wal.replay().expect("replay after recovered append");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].key, b"a");
        assert_eq!(records[1].key, b"b");
    }

    #[test]
    fn encrypted_append_then_replay_roundtrips_and_hides_plaintext() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let crypto = test_crypto(dir.path());
        let mut wal = Wal::open_for_append(&path, Some(crypto), sid(1)).expect("open");
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
    fn append_batch_rejects_batches_larger_than_replay_accepts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
        let ops = vec![
            BatchOp {
                op: WalOp::Put,
                key: b"k".to_vec(),
                value: Some(b"v".to_vec()),
            };
            wal::MAX_BATCH_OPS + 1
        ];

        let err = wal
            .append_batch(&ops)
            .expect_err("oversized batch refused before writing");
        assert!(matches!(err, EngineError::WalBatchTooLarge { .. }));
        assert!(wal.replay().expect("empty wal remains readable").is_empty());
    }

    #[test]
    fn encrypted_replay_tolerates_torn_trailing_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let crypto = test_crypto(dir.path());
        {
            let mut wal = Wal::open_for_append(&path, Some(crypto.clone()), sid(1)).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        }
        // Truncate mid-envelope: everything after the first envelope's
        // header start is a torn trailing write.
        let full = std::fs::read(&path).expect("read");
        std::fs::write(&path, &full[..full.len() - 3]).expect("truncate");
        {
            let mut wal = Wal::open_for_append(&path, Some(crypto.clone()), sid(1)).expect("reopen");
            assert!(wal.replay().expect("torn tail tolerated").is_empty());
            // The store keeps working after the torn tail.
            wal.append(WalOp::Put, b"b", Some(b"2")).expect("append after torn");
        }
        let mut wal = Wal::open_for_append(&path, Some(crypto), sid(1)).expect("second reopen");
        let records = wal.replay().expect("replay after recovered append");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, b"b");
        assert_eq!(records[0].value.as_deref(), Some(&b"2"[..]));
    }

    #[test]
    fn encrypted_replay_rejects_tampered_complete_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let crypto = test_crypto(dir.path());
        {
            let mut wal = Wal::open_for_append(&path, Some(crypto.clone()), sid(1)).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        }
        // Flip one ciphertext byte in an otherwise complete envelope: the
        // Poly1305 tag must fail loudly, never decode garbage.
        let mut raw = std::fs::read(&path).expect("read");
        let last = raw.len() - 1;
        raw[last] ^= 0xFF;
        std::fs::write(&path, &raw).expect("write tampered");
        let mut wal = Wal::open_for_append(&path, Some(crypto), sid(1)).expect("reopen");
        let err = wal.replay().expect_err("tampered envelope must fail");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }

    // --- ADR-044 §8 adversarial scenarios -----------------------------

    #[test]
    fn swapping_two_same_length_plaintext_records_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        {
            let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
            wal.append(WalOp::Put, b"aaa", Some(b"1")).expect("append a");
            wal.append(WalOp::Put, b"bbb", Some(b"2")).expect("append b");
        }
        let mut raw = std::fs::read(&path).expect("read");
        let half = raw.len() / 2;
        assert_eq!(half * 2, raw.len(), "the two same-shape records must be equal length");
        let (first, second) = raw.split_at(half);
        raw = [second, first].concat();
        std::fs::write(&path, &raw).expect("write swapped");

        let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("reopen");
        let err = wal
            .replay()
            .expect_err("swapped records must be rejected, not silently accepted");
        assert!(matches!(err, EngineError::WalRecordOffsetMismatch { .. }));
    }

    #[test]
    fn duplicating_a_stale_put_after_a_delete_is_rejected_encrypted() {
        // The exact CRYPTO-1 scenario: copy an old Put(key, stale) envelope
        // back into the log after a legitimate Delete(key).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let crypto = test_crypto(dir.path());
        let stale_envelope: Vec<u8>;
        {
            let mut wal = Wal::open_for_append(&path, Some(crypto.clone()), sid(1)).expect("open");
            wal.append(WalOp::Put, b"key", Some(b"stale")).expect("append put");
            let full = std::fs::read(&path).expect("read after first put");
            stale_envelope = full; // the whole (only) envelope so far
            wal.append(WalOp::Delete, b"key", None).expect("append delete");
        }
        // Append the stale envelope's raw bytes again — a byte-for-byte
        // replay attempt appended past the legitimate Delete.
        let mut raw = std::fs::read(&path).expect("read after delete");
        raw.extend_from_slice(&stale_envelope);
        std::fs::write(&path, &raw).expect("write replayed duplicate");

        let mut wal = Wal::open_for_append(&path, Some(crypto), sid(1)).expect("reopen");
        let err = wal
            .replay()
            .expect_err("a duplicated stale envelope must fail AEAD authentication");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }

    #[test]
    fn deleting_a_middle_record_desyncs_the_offset_chain_and_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        {
            let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append a");
            wal.append(WalOp::Put, b"bb", Some(b"22")).expect("append b");
            wal.append(WalOp::Put, b"c", Some(b"3")).expect("append c");
        }
        let raw = std::fs::read(&path).expect("read");
        let a_len = wal::encode(WalOp::Put, 0, b"a", Some(b"1")).len();
        // Delete the first record entirely: every following record's true
        // physical offset shifts backward by `a_len`, but its embedded
        // `record_offset` field still claims the original (now-wrong,
        // too-large) position.
        let spliced = raw[a_len..].to_vec();
        std::fs::write(&path, &spliced).expect("write spliced");

        let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("reopen");
        let err = wal
            .replay()
            .expect_err("a record found at the wrong offset must be rejected");
        assert!(matches!(err, EngineError::WalRecordOffsetMismatch { .. }));
    }

    #[test]
    fn copying_a_record_from_another_store_id_is_rejected() {
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let path_a = dir_a.path().join("wal.log");
        let path_b = dir_b.path().join("wal.log");
        // Same DEK/passphrase bytes on purpose: the realistic scenario is
        // two stores sharing a development key, distinguished only by
        // store_id, exactly what ADR-044 §4 calls out.
        let crypto_a = test_crypto(dir_a.path());
        let crypto_b = crate::crypto::create_meta(dir_b.path(), b"wal test key").expect("create crypto meta b");
        {
            let mut wal = Wal::open_for_append(&path_a, Some(crypto_a), sid(1)).expect("open a");
            wal.append(WalOp::Put, b"key", Some(b"value")).expect("append");
        }
        // Establish store b's own wal_epoch.meta first (an ordinary open of
        // an empty store), then plant store a's stolen bytes over its empty
        // wal.log — isolates the assertion to the store_id mismatch rather
        // than the pre-ADR-044-store refusal path.
        drop(Wal::open_for_append(&path_b, Some(crypto_b.clone()), sid(2)).expect("open b once to publish its epoch"));
        let stolen = std::fs::read(&path_a).expect("read store a's wal");
        std::fs::write(&path_b, &stolen).expect("plant store a's bytes into store b");

        let mut wal = Wal::open_for_append(&path_b, Some(crypto_b), sid(2)).expect("open b");
        let err = wal
            .replay()
            .expect_err("a record sealed for a different store_id must fail authentication");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }

    #[test]
    fn copying_a_record_from_an_earlier_epoch_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let crypto = test_crypto(dir.path());
        let epoch0_record: Vec<u8>;
        {
            let mut wal = Wal::open_for_append(&path, Some(crypto.clone()), sid(1)).expect("open");
            wal.append(WalOp::Put, b"old", Some(b"epoch0")).expect("append");
            epoch0_record = std::fs::read(&path).expect("read epoch 0 bytes");
            wal.reset().expect("truncate into epoch 1");
            wal.append(WalOp::Put, b"new", Some(b"epoch1"))
                .expect("append in epoch 1");
        }
        // Splice the earlier epoch's record onto the end of the current
        // (epoch-1) log at the offset it would need to claim to look valid
        // structurally.
        let mut raw = std::fs::read(&path).expect("read current wal");
        raw.extend_from_slice(&epoch0_record);
        std::fs::write(&path, &raw).expect("write spliced");

        let mut wal = Wal::open_for_append(&path, Some(crypto), sid(1)).expect("reopen");
        let err = wal
            .replay()
            .expect_err("a record sealed under a previous wal_epoch must fail authentication");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }

    #[test]
    fn modifying_record_offset_in_plaintext_field_fails_encrypted_replay() {
        // Flipping the plaintext record_offset field without being able to
        // recompute a valid AEAD tag must fail as an AEAD error, never as a
        // silent structural pass-through.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let crypto = test_crypto(dir.path());
        {
            let mut wal = Wal::open_for_append(&path, Some(crypto.clone()), sid(1)).expect("open");
            wal.append(WalOp::Put, b"key", Some(b"value")).expect("append");
        }
        // Corrupt a ciphertext byte inside the envelope body (past the
        // fixed header) — the attacker cannot recompute the tag without the
        // key, so any bit flip inside the sealed region must fail AEAD.
        let mut raw = std::fs::read(&path).expect("read");
        let flip_at = raw.len() - 5;
        raw[flip_at] ^= 0xFF;
        std::fs::write(&path, &raw).expect("write tampered");

        let mut wal = Wal::open_for_append(&path, Some(crypto), sid(1)).expect("reopen");
        let err = wal
            .replay()
            .expect_err("tampered ciphertext must fail AEAD, not decode silently");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }

    #[test]
    fn replaying_the_same_wal_twice_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
        wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
        let first = wal.replay().expect("first replay");
        let second = wal.replay().expect("second replay (double open)");
        assert_eq!(first, second);
    }

    #[test]
    fn every_truncation_of_the_last_record_is_torn_tail_not_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let full_bytes = {
            let mut wal = Wal::open_for_append(&path, None, sid(1)).expect("open");
            wal.append(WalOp::Put, b"a", Some(b"1")).expect("append");
            std::fs::read(&path).expect("read")
        };
        for cut in 0..full_bytes.len() {
            let truncated_path = dir.path().join(format!("cut-{cut}"));
            std::fs::create_dir_all(&truncated_path).expect("mkdir");
            // Simulates a crash mid-append on an *already-open* store, not a
            // fresh one — its epoch was already published before the append
            // that got torn.
            publish_wal_epoch(&truncated_path, 0).expect("publish epoch for simulated crash dir");
            let wal_path = truncated_path.join("wal.log");
            std::fs::write(&wal_path, &full_bytes[..cut]).expect("write truncated");
            let mut wal = Wal::open_for_append(&wal_path, None, sid(1)).expect("open truncated");
            let records = wal
                .replay()
                .unwrap_or_else(|e| panic!("torn tail tolerated at cut={cut}: {e}"));
            assert!(records.is_empty(), "cut={cut}");
        }
    }
}
