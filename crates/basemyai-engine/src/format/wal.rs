// SPDX-License-Identifier: BUSL-1.1
//! On-disk WAL record layout.
//!
//! `format.lock` anchor: `WalRecord:1` — bump [`WAL_RECORD_VERSION`] and this
//! doc comment together whenever the byte layout below changes.
//!
//! Record layout (all integers little-endian):
//!
//! ```text
//! magic:    u32   = WAL_MAGIC
//! version:  u16   = WAL_RECORD_VERSION
//! op:       u8     1 = Put, 2 = Delete, 3 = Batch
//! key_len:  u32
//! val_len:  u32    0 for Delete; a Put with an empty value also has val_len
//!                  == 0 — the two are disambiguated by `op`, never by
//!                  `val_len`
//! key:      [u8; key_len]
//! value:    [u8; val_len]           (omitted entirely when op == Delete)
//! crc32:    u32    over every byte above (magic..value) in this record
//! ```
//!
//! ## Batch records (op == 3), added in version 2
//!
//! A batch (see `store::Batch`/`Engine::apply_batch`) is written as exactly
//! **one** outer record of this same framing — `key` is empty (`key_len ==
//! 0`) and `value` holds a nested, self-delimited sequence of sub-operations
//! instead of a single payload:
//!
//! ```text
//! count:      u32                     number of sub-operations
//! sub-op[0..count]:
//!   op:       u8                      1 = Put, 2 = Delete (never Batch)
//!   key_len:  u32
//!   val_len:  u32                     0 for Delete
//!   key:      [u8; key_len]
//!   value:    [u8; val_len]           (omitted when op == Delete)
//! ```
//!
//! This is deliberately *not* a sequence of independent outer records: the
//! whole batch's bytes are covered by the single outer `crc32`, and the
//! single outer `val_len` means the existing torn-tail check in
//! `store::wal::Wal::replay` (`buf.len() < total` => `Ok(None)`, stop
//! replaying) already rejects the batch **as a whole** if the process was
//! killed before the last byte of the last sub-operation was fsynced — there
//! is no way for replay to observe a prefix of a batch's sub-operations
//! without also observing the rest, because they all live inside one
//! `val_len`-delimited, checksummed span. All-or-nothing falls out of the
//! existing single-record framing for free, rather than needing a new
//! begin/commit marker pair and matching recovery logic.
//!
//! This module only does *encoding*: turning an operation (or batch of
//! operations) into bytes and back. Actual file I/O (append, fsync, replay
//! with torn-tail tolerance) lives in `crate::store::wal`.

use std::path::Path;

use super::checksum::crc32;
use crate::error::{EngineError, Result};

pub const WAL_MAGIC: u32 = 0x4241_5345; // b"BASE"
/// Bumped 1 -> 2 to add the `Batch` op (see the module doc's "Batch records"
/// section) — a deliberate wire-format extension, not a silent drift.
pub const WAL_RECORD_VERSION: u16 = 2;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`super::lock`]). Field list and order must mirror the byte layout
/// documented above exactly — update both together, never one without the
/// other.
pub fn spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "WalRecord",
        version: WAL_RECORD_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("op", "u8"),
            ("key_len", "u32"),
            ("val_len", "u32"),
            ("key", "bytes(key_len)"),
            ("value", "bytes(val_len)?"),
            ("crc32", "u32"),
        ],
    }
}

/// Fixed-size portion of a record, before the variable-length key/value.
const HEADER_LEN: usize = 4 + 2 + 1 + 4 + 4;
const CRC_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalOp {
    Put = 1,
    Delete = 2,
    /// A framed group of `Put`/`Delete` sub-operations, applied atomically.
    /// See the "Batch records" section of this module's doc comment. Never
    /// appears as the `op` of a [`BatchOp`] (only `Put`/`Delete` may nest).
    Batch = 3,
}

impl WalOp {
    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Put),
            2 => Some(Self::Delete),
            3 => Some(Self::Batch),
            _ => None,
        }
    }
}

/// A decoded WAL record, owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalRecord {
    pub op: WalOp,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

/// One `Put`/`Delete` sub-operation nested inside a `Batch` record's payload.
/// `op` is only ever `Put` or `Delete` here — nested batches are not
/// supported (there is no meaningful semantics for one, and nothing
/// constructs one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOp {
    pub op: WalOp,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

/// Encodes a batch's sub-operations into the nested payload described by
/// this module's "Batch records" doc section. The caller wraps the result as
/// the `value` of a single outer `encode(WalOp::Batch, &[], Some(payload))`
/// record — that outer framing is what actually provides atomicity (single
/// fsynced write, single checksum over the whole thing).
#[must_use]
pub fn encode_batch(ops: &[BatchOp]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&(ops.len() as u32).to_le_bytes());
    for entry in ops {
        let val_bytes: &[u8] = entry.value.as_deref().unwrap_or(&[]);
        buf.push(entry.op as u8);
        buf.extend_from_slice(&(entry.key.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(val_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&entry.key);
        buf.extend_from_slice(val_bytes);
    }
    buf
}

/// Decodes a `Batch` record's nested payload (the `value` bytes of an outer
/// record whose `op == WalOp::Batch`) back into its sub-operations.
///
/// By the time this is called, the *outer* record has already passed
/// `decode`'s length and checksum checks — the whole payload is guaranteed
/// to be exactly the bytes that were written, never a torn prefix. So any
/// structural inconsistency found here (bad sub-op tag, lengths that don't
/// add up) is genuine corruption, not a crash-mid-write artifact, and is
/// reported as [`EngineError::CorruptWal`] rather than tolerated like a torn
/// trailing record.
pub fn decode_batch(buf: &[u8], path: &Path) -> Result<Vec<BatchOp>> {
    let corrupt = |reason: &str| EngineError::CorruptWal {
        path: path.to_path_buf(),
        reason: format!("malformed batch payload: {reason}"),
    };

    if buf.len() < 4 {
        return Err(corrupt("payload shorter than the batch count field"));
    }
    let count = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes")) as usize;

    let mut offset = 4usize;
    let mut ops = Vec::with_capacity(count);
    for i in 0..count {
        if buf.len() < offset + 9 {
            return Err(corrupt(&format!("truncated sub-operation header at index {i}")));
        }
        let Some(op) = WalOp::from_tag(buf[offset]) else {
            return Err(corrupt(&format!("unrecognized sub-operation tag at index {i}")));
        };
        if matches!(op, WalOp::Batch) {
            return Err(corrupt(&format!("nested Batch op at index {i} is not supported")));
        }
        let key_len = u32::from_le_bytes(
            buf[offset + 1..offset + 5]
                .try_into()
                .expect("slice is exactly 4 bytes"),
        ) as usize;
        let val_len = u32::from_le_bytes(
            buf[offset + 5..offset + 9]
                .try_into()
                .expect("slice is exactly 4 bytes"),
        ) as usize;
        offset += 9;
        if buf.len() < offset + key_len + val_len {
            return Err(corrupt(&format!("truncated sub-operation body at index {i}")));
        }
        let key = buf[offset..offset + key_len].to_vec();
        offset += key_len;
        let value = match op {
            WalOp::Delete => None,
            WalOp::Put => Some(buf[offset..offset + val_len].to_vec()),
            WalOp::Batch => unreachable!("rejected above"),
        };
        offset += val_len;
        ops.push(BatchOp { op, key, value });
    }
    if offset != buf.len() {
        return Err(corrupt("trailing bytes after the declared sub-operation count"));
    }
    Ok(ops)
}

/// Encodes one record (header + key + value + trailing crc32).
pub fn encode(op: WalOp, key: &[u8], value: Option<&[u8]>) -> Vec<u8> {
    let val_bytes: &[u8] = value.unwrap_or(&[]);
    let mut buf = Vec::with_capacity(HEADER_LEN + key.len() + val_bytes.len() + CRC_LEN);
    buf.extend_from_slice(&WAL_MAGIC.to_le_bytes());
    buf.extend_from_slice(&WAL_RECORD_VERSION.to_le_bytes());
    buf.push(op as u8);
    buf.extend_from_slice(&(key.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(val_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(val_bytes);
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

/// Decodes exactly one record from the front of `buf`.
///
/// Returns `Ok(Some((record, consumed_len)))` on success. Returns `Ok(None)`
/// if `buf` doesn't yet contain a full, structurally valid record — the
/// replay loop (`store::wal::Wal::replay`) treats that as a torn trailing
/// write from a crash mid-append and stops silently instead of failing.
/// Returns `Err` only when a *fully-buffered* record's checksum doesn't
/// match, i.e. corruption that is not explainable as a torn tail.
pub fn decode(buf: &[u8], path: &Path) -> Result<Option<(WalRecord, usize)>> {
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != WAL_MAGIC {
        return Ok(None);
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != WAL_RECORD_VERSION {
        return Err(EngineError::UnsupportedFormatVersion {
            path: path.to_path_buf(),
            expected: WAL_RECORD_VERSION,
            found: version,
        });
    }
    let Some(op) = WalOp::from_tag(buf[6]) else {
        return Ok(None);
    };
    let key_len = u32::from_le_bytes(buf[7..11].try_into().expect("slice is exactly 4 bytes")) as usize;
    let val_len = u32::from_le_bytes(buf[11..15].try_into().expect("slice is exactly 4 bytes")) as usize;
    let total = HEADER_LEN + key_len + val_len + CRC_LEN;
    if buf.len() < total {
        return Ok(None);
    }
    let body_end = HEADER_LEN + key_len + val_len;
    let expected_crc = u32::from_le_bytes(
        buf[body_end..body_end + CRC_LEN]
            .try_into()
            .expect("slice is exactly 4 bytes"),
    );
    let actual_crc = crc32(&buf[0..body_end]);
    if actual_crc != expected_crc {
        return Err(EngineError::CorruptWal {
            path: path.to_path_buf(),
            reason: format!("checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"),
        });
    }
    let key = buf[HEADER_LEN..HEADER_LEN + key_len].to_vec();
    let value = match op {
        WalOp::Delete => None,
        // A `Batch` record's "value" is its nested sub-operation payload
        // (see this module's "Batch records" doc section) — carried the
        // same way a `Put`'s value is, just with different contents.
        WalOp::Put | WalOp::Batch => Some(buf[HEADER_LEN + key_len..body_end].to_vec()),
    };
    Ok(Some((WalRecord { op, key, value }, total)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("test.wal")
    }

    #[test]
    fn roundtrips_put() {
        let bytes = encode(WalOp::Put, b"key", Some(b"value"));
        let (record, consumed) = decode(&bytes, &path()).expect("decode ok").expect("full record");
        assert_eq!(consumed, bytes.len());
        assert_eq!(record.op, WalOp::Put);
        assert_eq!(record.key, b"key");
        assert_eq!(record.value, Some(b"value".to_vec()));
    }

    #[test]
    fn roundtrips_delete() {
        let bytes = encode(WalOp::Delete, b"key", None);
        let (record, _) = decode(&bytes, &path()).expect("decode ok").expect("full record");
        assert_eq!(record.op, WalOp::Delete);
        assert_eq!(record.value, None);
    }

    #[test]
    fn truncated_tail_is_none_not_error() {
        let bytes = encode(WalOp::Put, b"key", Some(b"value"));
        for cut in 1..bytes.len() {
            let result = decode(&bytes[..cut], &path()).expect("torn tail is not an error");
            assert!(result.is_none(), "expected None at cut={cut}");
        }
    }

    #[test]
    fn bit_flip_in_complete_record_is_corrupt_error() {
        let mut bytes = encode(WalOp::Put, b"key", Some(b"value"));
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF; // corrupt the trailing crc32 byte itself
        let err = decode(&bytes, &path()).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }

    #[test]
    fn concatenated_records_decode_in_sequence() {
        let mut buf = encode(WalOp::Put, b"a", Some(b"1"));
        buf.extend(encode(WalOp::Put, b"b", Some(b"2")));
        let (first, consumed) = decode(&buf, &path()).expect("decode ok").expect("record");
        assert_eq!(first.key, b"a");
        let (second, _) = decode(&buf[consumed..], &path()).expect("decode ok").expect("record");
        assert_eq!(second.key, b"b");
    }

    #[test]
    fn batch_payload_roundtrips_puts_and_deletes() {
        let ops = vec![
            BatchOp {
                op: WalOp::Put,
                key: b"a".to_vec(),
                value: Some(b"1".to_vec()),
            },
            BatchOp {
                op: WalOp::Delete,
                key: b"b".to_vec(),
                value: None,
            },
            BatchOp {
                op: WalOp::Put,
                key: b"c".to_vec(),
                value: Some(b"".to_vec()), // empty value, still a Put not a Delete
            },
        ];
        let payload = encode_batch(&ops);
        let decoded = decode_batch(&payload, &path()).expect("decode batch");
        assert_eq!(decoded, ops);
    }

    #[test]
    fn empty_batch_payload_roundtrips_to_empty_vec() {
        let payload = encode_batch(&[]);
        let decoded = decode_batch(&payload, &path()).expect("decode batch");
        assert!(decoded.is_empty());
    }

    #[test]
    fn batch_record_roundtrips_through_full_outer_encode_decode() {
        let ops = vec![
            BatchOp {
                op: WalOp::Put,
                key: b"k1".to_vec(),
                value: Some(b"v1".to_vec()),
            },
            BatchOp {
                op: WalOp::Delete,
                key: b"k2".to_vec(),
                value: None,
            },
        ];
        let payload = encode_batch(&ops);
        let record_bytes = encode(WalOp::Batch, &[], Some(&payload));
        let (record, consumed) = decode(&record_bytes, &path()).expect("decode ok").expect("full record");
        assert_eq!(consumed, record_bytes.len());
        assert_eq!(record.op, WalOp::Batch);
        let decoded_ops = decode_batch(&record.value.expect("batch has a payload"), &path()).expect("decode batch");
        assert_eq!(decoded_ops, ops);
    }

    #[test]
    fn batch_record_torn_tail_is_none_not_error() {
        let ops = vec![BatchOp {
            op: WalOp::Put,
            key: b"k".to_vec(),
            value: Some(b"v".to_vec()),
        }];
        let payload = encode_batch(&ops);
        let record_bytes = encode(WalOp::Batch, &[], Some(&payload));
        for cut in 1..record_bytes.len() {
            let result = decode(&record_bytes[..cut], &path()).expect("torn tail is not an error");
            assert!(result.is_none(), "expected None at cut={cut}");
        }
    }

    #[test]
    fn decode_batch_rejects_truncated_payload() {
        let ops = vec![BatchOp {
            op: WalOp::Put,
            key: b"k".to_vec(),
            value: Some(b"v".to_vec()),
        }];
        let payload = encode_batch(&ops);
        // Simulate a corrupted (not torn — this is reached only once the
        // *outer* record's checksum already validated) payload by truncating
        // the nested buffer itself.
        let err = decode_batch(&payload[..payload.len() - 1], &path()).expect_err("truncated payload is corrupt");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }

    #[test]
    fn decode_batch_rejects_nested_batch_op() {
        // Hand-craft a payload claiming one sub-op with op tag 3 (Batch) —
        // decode_batch must reject this rather than recurse or panic.
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // count = 1
        payload.push(3); // WalOp::Batch tag — invalid as a sub-op
        payload.extend_from_slice(&0u32.to_le_bytes()); // key_len
        payload.extend_from_slice(&0u32.to_le_bytes()); // val_len
        let err = decode_batch(&payload, &path()).expect_err("nested batch is rejected");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }
}
