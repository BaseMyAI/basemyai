// SPDX-License-Identifier: BUSL-1.1
//! Vector-index node block layout (LM-DiskANN style, ADR-026).
//!
//! `format.lock` anchor: `VectorNode:2` — bump [`VECTOR_NODE_VERSION`] and
//! this doc comment together whenever the byte layout below changes.
//!
//! One node = one self-contained block (ADR-026 §Décision 1/2): the vector
//! itself, its out-neighbor id list, and its tombstone flag, stored as a
//! single KV value in the Layer-1 store.
//!
//! The tombstone marker lives *inside the block* (v2: `flags` bit 0) rather
//! than under a separate tombstone key, deliberately: the block stays
//! autonomous (the LM-DiskANN point — one node = one self-describing
//! record), a logical delete is one local block rewrite inside one atomic
//! `apply_batch` (block + metadata, ADR-026 §3), and the rebuild path sees
//! the marker for free while scanning the very data it already reads — a
//! separate tombstone keyspace would force joining two scans and could
//! desynchronize from the block under crash. v1→v2 is a hard cut with no
//! migration: this crate is unpublished and not wired into any consumer, so
//! no v1 data exists outside this repo's own tests.
//!
//! Block layout (all integers and floats little-endian):
//!
//! ```text
//! magic:          u32  = VECTOR_NODE_MAGIC
//! version:        u16  = VECTOR_NODE_VERSION
//! flags:          u8    bit 0 = deleted (tombstone, ADR-026 §4: excluded
//!                       from search results but still traversable as a
//!                       routing point until consolidation purges the
//!                       block); bits 1-7 reserved, must be zero — the
//!                       decoder rejects unknown bits instead of silently
//!                       dropping future semantics
//! dim:            u16   number of f32 components in `vector`
//! neighbor_count: u16   number of u64 ids in `neighbors` (bounded by the
//!                       index's max degree R in practice, but the decoder
//!                       only trusts the buffer length, never the field)
//! vector:         [f32 LE; dim]
//! neighbors:      [u64 LE; neighbor_count]
//! crc32:          u32   over every byte above (magic..neighbors)
//! ```
//!
//! Like `format::{wal,sst_block}`, this module only does *encoding*: turning
//! a node into bytes and back. Reading/writing blocks through `Engine` is
//! `idx::vector::persistent`'s job.

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

pub const VECTOR_NODE_MAGIC: u32 = 0x564E_4F44; // b"VNOD" (LE bytes "DONV")
pub const VECTOR_NODE_VERSION: u16 = 2;

/// Bit 0 of the `flags` byte: the node is a tombstone (logically deleted).
const FLAG_DELETED: u8 = 0b0000_0001;
/// Every currently-defined flag bit; anything outside is rejected on decode.
const FLAG_MASK: u8 = FLAG_DELETED;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented above exactly — update both together, never one
/// without the other.
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "VectorNode",
        version: VECTOR_NODE_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("flags", "u8"),
            ("dim", "u16"),
            ("neighbor_count", "u16"),
            ("vector", "f32[dim]"),
            ("neighbors", "u64[neighbor_count]"),
            ("crc32", "u32"),
        ],
    }
}

/// Fixed-size portion of a block, before the variable-length arrays.
const HEADER_LEN: usize = 4 + 2 + 1 + 2 + 2;
const CRC_LEN: usize = 4;
const F32_LEN: usize = 4;
const U64_LEN: usize = 8;

/// A decoded vector-index node, owned. `vector.len()` is the dimension;
/// `neighbors` holds out-edge ids in graph order (the encoder preserves
/// order, it carries no meaning on the wire); `deleted` is the tombstone
/// flag (wire `flags` bit 0) — a deleted node keeps its vector and neighbor
/// list so it can keep serving as a routing point until consolidation.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorNode {
    pub vector: Vec<f32>,
    pub neighbors: Vec<u64>,
    pub deleted: bool,
}

impl VectorNode {
    /// A live (non-tombstoned) node — the overwhelmingly common constructor.
    #[must_use]
    pub fn live(vector: Vec<f32>, neighbors: Vec<u64>) -> Self {
        Self {
            vector,
            neighbors,
            deleted: false,
        }
    }
}

/// Encodes one node block (header + vector + neighbors + trailing crc32).
///
/// Returns `Err(EngineError::CorruptVectorNode)` if `vector.len()` or
/// `neighbors.len()` exceeds `u16::MAX` — the wire fields are `u16` and a
/// silent `as` truncation would produce a block that decodes to different
/// content than what was encoded.
pub fn encode(node: &VectorNode) -> Result<Vec<u8>> {
    let dim = u16::try_from(node.vector.len()).map_err(|_| EngineError::CorruptVectorNode {
        reason: format!("dimension {} exceeds the u16 wire field", node.vector.len()),
    })?;
    let neighbor_count = u16::try_from(node.neighbors.len()).map_err(|_| EngineError::CorruptVectorNode {
        reason: format!("neighbor count {} exceeds the u16 wire field", node.neighbors.len()),
    })?;
    let flags: u8 = if node.deleted { FLAG_DELETED } else { 0 };

    let mut buf =
        Vec::with_capacity(HEADER_LEN + node.vector.len() * F32_LEN + node.neighbors.len() * U64_LEN + CRC_LEN);
    buf.extend_from_slice(&VECTOR_NODE_MAGIC.to_le_bytes());
    buf.extend_from_slice(&VECTOR_NODE_VERSION.to_le_bytes());
    buf.push(flags);
    buf.extend_from_slice(&dim.to_le_bytes());
    buf.extend_from_slice(&neighbor_count.to_le_bytes());
    for component in &node.vector {
        buf.extend_from_slice(&component.to_le_bytes());
    }
    for neighbor in &node.neighbors {
        buf.extend_from_slice(&neighbor.to_le_bytes());
    }
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Decodes a node block previously produced by [`encode`].
///
/// N2 fuzzing lesson (see `docs/TODO-NATIVE-ENGINE.md`, `format::sst_block`): every
/// count field read from the wire is bounded against the *actual* buffer
/// length before any allocation — here the exact-length equation
/// `header + dim·4 + neighbor_count·8 + crc == buf.len()` is checked before
/// the vectors are materialized, so a lying `dim`/`neighbor_count` yields
/// `CorruptVectorNode`, never a panic or oversized allocation. (`dim` and
/// `neighbor_count` are `u16`, so the length arithmetic below cannot
/// overflow `usize`.)
pub fn decode(buf: &[u8]) -> Result<VectorNode> {
    let corrupt = |reason: String| EngineError::CorruptVectorNode { reason };

    if buf.len() < HEADER_LEN + CRC_LEN {
        return Err(corrupt("block shorter than fixed header + trailing crc32".to_string()));
    }
    let crc_at = buf.len() - CRC_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != VECTOR_NODE_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != VECTOR_NODE_VERSION {
        return Err(EngineError::UnsupportedVectorNodeVersion {
            expected: VECTOR_NODE_VERSION,
            found: version,
        });
    }
    let flags = buf[6];
    if flags & !FLAG_MASK != 0 {
        return Err(corrupt(format!(
            "unknown flag bits {:#010b} (this build understands {FLAG_MASK:#010b})",
            flags
        )));
    }
    let dim = u16::from_le_bytes(buf[7..9].try_into().expect("slice is exactly 2 bytes")) as usize;
    let neighbor_count = u16::from_le_bytes(buf[9..11].try_into().expect("slice is exactly 2 bytes")) as usize;

    let expected_len = HEADER_LEN + dim * F32_LEN + neighbor_count * U64_LEN + CRC_LEN;
    if buf.len() != expected_len {
        return Err(corrupt(format!(
            "declared dim {dim} + neighbor_count {neighbor_count} imply a {expected_len}-byte \
             block, got {} bytes",
            buf.len()
        )));
    }

    let mut pos = HEADER_LEN;
    let mut vector = Vec::with_capacity(dim);
    for _ in 0..dim {
        vector.push(f32::from_le_bytes(
            buf[pos..pos + F32_LEN].try_into().expect("slice is exactly 4 bytes"),
        ));
        pos += F32_LEN;
    }
    let mut neighbors = Vec::with_capacity(neighbor_count);
    for _ in 0..neighbor_count {
        neighbors.push(u64::from_le_bytes(
            buf[pos..pos + U64_LEN].try_into().expect("slice is exactly 8 bytes"),
        ));
        pos += U64_LEN;
    }
    Ok(VectorNode {
        vector,
        neighbors,
        deleted: flags & FLAG_DELETED != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> VectorNode {
        VectorNode::live(vec![0.25, -1.5, 3.0, 0.0], vec![7, 42, u64::MAX])
    }

    #[test]
    fn roundtrips_a_node() {
        let node = sample();
        let bytes = encode(&node).expect("encode ok");
        let decoded = decode(&bytes).expect("decode ok");
        assert_eq!(decoded, node);
    }

    #[test]
    fn roundtrips_a_tombstoned_node() {
        let node = VectorNode {
            deleted: true,
            ..sample()
        };
        let bytes = encode(&node).expect("encode ok");
        let decoded = decode(&bytes).expect("decode ok");
        assert!(decoded.deleted);
        assert_eq!(decoded, node);
    }

    #[test]
    fn roundtrips_empty_vector_and_neighbors() {
        let node = VectorNode::live(Vec::new(), Vec::new());
        let bytes = encode(&node).expect("encode ok");
        let decoded = decode(&bytes).expect("decode ok");
        assert_eq!(decoded, node);
    }

    #[test]
    fn roundtrips_384d_no_neighbors() {
        let node = VectorNode::live((0..384).map(|i| i as f32 * 0.5 - 96.0).collect(), Vec::new());
        let bytes = encode(&node).expect("encode ok");
        let decoded = decode(&bytes).expect("decode ok");
        assert_eq!(decoded, node);
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&sample()).expect("encode ok");
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated block must not decode");
            assert!(
                matches!(err, EngineError::CorruptVectorNode { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[HEADER_LEN] ^= 0xFF; // flip a byte of the vector payload
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptVectorNode { .. }));
    }

    #[test]
    fn unknown_version_is_unsupported_error() {
        let mut bytes = encode(&sample()).expect("encode ok");
        // Bump the version field, then fix the crc so only the version check trips.
        bytes[4] = 0xFF;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("unknown version must be rejected");
        assert!(matches!(
            err,
            EngineError::UnsupportedVectorNodeVersion { found: 0x00FF, .. }
        ));
    }

    /// A v1 block (the pre-tombstone layout) must be rejected as an
    /// unsupported version, never misparsed: v1→v2 is a deliberate hard cut
    /// (see the module doc), and the version gate is what enforces it.
    #[test]
    fn v1_block_is_rejected_as_unsupported_version() {
        // Hand-build a well-formed v1 block: magic, version=1, dim=1,
        // neighbor_count=0, one f32, crc32.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&VECTOR_NODE_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // dim
        bytes.extend_from_slice(&0u16.to_le_bytes()); // neighbor_count
        bytes.extend_from_slice(&1.0f32.to_le_bytes());
        let crc = crc32(&bytes);
        bytes.extend_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("v1 block must be rejected");
        assert!(matches!(
            err,
            EngineError::UnsupportedVectorNodeVersion { found: 1, .. }
        ));
    }

    /// Reserved flag bits must be rejected (crc32 recomputed so only the
    /// flags check trips) — silently ignoring them would drop future
    /// semantics on the floor.
    #[test]
    fn unknown_flag_bits_are_rejected() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[6] = 0b0000_0010;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("unknown flag bits must be rejected");
        assert!(matches!(err, EngineError::CorruptVectorNode { .. }));
    }

    /// A `neighbor_count` that lies about the payload size (with a
    /// recomputed crc32, so the checksum gate does not short-circuit first)
    /// must be caught by the exact-length equation, not by a panic or an
    /// oversized allocation — the exact N2 fuzzing lesson from `format::sst_block`.
    #[test]
    fn lying_neighbor_count_is_rejected_not_panicking() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[9..11].copy_from_slice(&u16::MAX.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("lying neighbor_count must be rejected");
        assert!(matches!(err, EngineError::CorruptVectorNode { .. }));
    }

    #[test]
    fn lying_dim_is_rejected_not_panicking() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[7..9].copy_from_slice(&u16::MAX.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("lying dim must be rejected");
        assert!(matches!(err, EngineError::CorruptVectorNode { .. }));
    }

    #[test]
    fn oversized_inputs_are_rejected_at_encode_time() {
        let node = VectorNode::live(vec![0.0; usize::from(u16::MAX) + 1], Vec::new());
        let err = encode(&node).expect_err("dim beyond u16 must not silently truncate");
        assert!(matches!(err, EngineError::CorruptVectorNode { .. }));
    }
}
