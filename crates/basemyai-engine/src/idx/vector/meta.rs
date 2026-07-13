// SPDX-License-Identifier: BUSL-1.1
//! Vector-index parameters and persisted index metadata (ADR-026).
//!
//! Two things live here:
//! - [`VectorIndexParams`] — the knobs of the Vamana/LM-DiskANN graph,
//!   exposed as one struct so no magic number hides inside `graph.rs`.
//! - [`VectorIndexMeta`] + its wire codec — the single per-index metadata
//!   record persisted in the KV store under `key::vector_index::meta_key()`
//!   (params, entry point, epoch, count), versioned in `format.lock` like
//!   every other persisted type.
//!
//! `format.lock` anchor: `VectorIndexMeta:1` — bump [`VECTOR_INDEX_META_VERSION`]
//! and this layout together whenever the byte layout below changes.
//!
//! Record layout (all integers and floats little-endian):
//!
//! ```text
//! magic:       u32  = VECTOR_INDEX_META_MAGIC
//! version:     u16  = VECTOR_INDEX_META_VERSION
//! dim:         u16   vector dimension of the index
//! max_degree:  u16   R — maximum out-degree
//! beam_width:  u16   L — greedy-search beam width
//! alpha:       f32   α — robust-prune slack factor
//! epoch:       u64   build generation; bumped by every rebuild, never by
//!                    incremental inserts — a reopened index whose meta is
//!                    absent/corrupt/inconsistent gets rebuilt from the
//!                    vectors and comes back with epoch + 1 (ADR-026 §3)
//! count:       u64   number of nodes in the index
//! entry_point: u64   fixed navigation start of every search; only
//!                    meaningful when count > 0 (encoded as 0 otherwise)
//! crc32:       u32   over every byte above (magic..entry_point)
//! ```

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

/// Product-default embedding dimension (`all-MiniLM-L6-v2`, 384d — see
/// `EMBEDDING_DIM` in `crates/basemyai/src/memory/schema.rs`). The index is
/// dimension-parametric; this is only the default.
pub const DEFAULT_DIM: usize = 384;

/// Default maximum out-degree `R` (classic DiskANN setting).
pub const DEFAULT_MAX_DEGREE: usize = 32;

/// Default beam width `L` for greedy search (used for both build and
/// query; `search` widens the beam to `k` if asked for more results than
/// `L`).
///
/// 128, not the lighter L=64 sometimes quoted: measured on the recall
/// harness (`tests/vector_recall.rs`, seeded random 384d vectors — a
/// deliberately hard, near-orthogonal workload), L=64 lands at
/// recall@10 = 0.856, under the ADR-026 §6 gate of 0.9, while L=128
/// reaches 0.988 for ~12 % extra insert cost. The DiskANN reference
/// setups use L_build ≈ 100–125 for the same reason.
pub const DEFAULT_BEAM_WIDTH: usize = 128;

/// Default robust-prune slack `α` (classic DiskANN setting; `α > 1`
/// keeps some longer edges for graph navigability).
pub const DEFAULT_ALPHA: f32 = 1.2;

pub const VECTOR_INDEX_META_MAGIC: u32 = 0x564D_4554; // b"VMET" (LE bytes "TEMV")
pub const VECTOR_INDEX_META_VERSION: u16 = 1;

/// Tunable parameters of a vector index (RAM [`super::graph::VectorIndex`]
/// or persistent [`super::persistent::PersistentVectorIndex`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorIndexParams {
    /// Vector dimension every inserted/queried vector must match.
    pub dim: usize,
    /// `R` — maximum out-degree of a graph node.
    pub max_degree: usize,
    /// `L` — beam width of the greedy search (candidate list size).
    pub beam_width: usize,
    /// `α` — robust-prune slack factor.
    pub alpha: f32,
}

impl Default for VectorIndexParams {
    fn default() -> Self {
        Self {
            dim: DEFAULT_DIM,
            max_degree: DEFAULT_MAX_DEGREE,
            beam_width: DEFAULT_BEAM_WIDTH,
            alpha: DEFAULT_ALPHA,
        }
    }
}

impl VectorIndexParams {
    /// Defaults with a non-default dimension (tests and future V2
    /// multi-model setups).
    #[must_use]
    pub fn with_dim(dim: usize) -> Self {
        Self { dim, ..Self::default() }
    }
}

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented in the module doc exactly — update both together,
/// never one without the other.
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "VectorIndexMeta",
        version: VECTOR_INDEX_META_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("dim", "u16"),
            ("max_degree", "u16"),
            ("beam_width", "u16"),
            ("alpha", "f32"),
            ("epoch", "u64"),
            ("count", "u64"),
            ("entry_point", "u64"),
            ("crc32", "u32"),
        ],
    }
}

/// Total encoded size: fixed-size record (no variable-length payload).
const META_LEN: usize = 4 + 2 + 2 + 2 + 2 + 4 + 8 + 8 + 8 + 4;
const CRC_LEN: usize = 4;

/// The decoded per-index metadata record persisted under
/// `key::vector_index::meta_key()`.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorIndexMeta {
    pub params: VectorIndexParams,
    /// Build generation (see the module doc's `epoch` field description).
    pub epoch: u64,
    /// Number of nodes in the index.
    pub count: u64,
    /// Entry point of every greedy search; `None` iff `count == 0`.
    pub entry_point: Option<u64>,
}

/// Encodes the metadata record.
///
/// Returns `Err(EngineError::CorruptVectorIndexMeta)` if `dim`/`max_degree`/
/// `beam_width` exceed their `u16` wire fields — a silent `as` truncation
/// would persist parameters different from the ones actually in use.
pub fn encode(meta: &VectorIndexMeta) -> Result<Vec<u8>> {
    let field_u16 = |value: usize, what: &str| {
        u16::try_from(value).map_err(|_| EngineError::CorruptVectorIndexMeta {
            reason: format!("{what} {value} exceeds the u16 wire field"),
        })
    };
    let dim = field_u16(meta.params.dim, "dimension")?;
    let max_degree = field_u16(meta.params.max_degree, "max degree")?;
    let beam_width = field_u16(meta.params.beam_width, "beam width")?;

    let mut buf = Vec::with_capacity(META_LEN);
    buf.extend_from_slice(&VECTOR_INDEX_META_MAGIC.to_le_bytes());
    buf.extend_from_slice(&VECTOR_INDEX_META_VERSION.to_le_bytes());
    buf.extend_from_slice(&dim.to_le_bytes());
    buf.extend_from_slice(&max_degree.to_le_bytes());
    buf.extend_from_slice(&beam_width.to_le_bytes());
    buf.extend_from_slice(&meta.params.alpha.to_le_bytes());
    buf.extend_from_slice(&meta.epoch.to_le_bytes());
    buf.extend_from_slice(&meta.count.to_le_bytes());
    buf.extend_from_slice(&meta.entry_point.unwrap_or(0).to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Decodes a metadata record previously produced by [`encode`].
///
/// Fixed-length record: an exact-length check runs before any field is
/// read, so no wire field can drive an allocation or out-of-bounds read
/// (N2 fuzzing lesson, same discipline as `format::sst_block` / `node::decode`).
pub fn decode(buf: &[u8]) -> Result<VectorIndexMeta> {
    let corrupt = |reason: String| EngineError::CorruptVectorIndexMeta { reason };

    if buf.len() != META_LEN {
        return Err(corrupt(format!(
            "metadata record must be exactly {META_LEN} bytes, got {}",
            buf.len()
        )));
    }
    let crc_at = META_LEN - CRC_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != VECTOR_INDEX_META_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != VECTOR_INDEX_META_VERSION {
        return Err(EngineError::UnsupportedVectorIndexMetaVersion {
            expected: VECTOR_INDEX_META_VERSION,
            found: version,
        });
    }
    let dim = u16::from_le_bytes(buf[6..8].try_into().expect("slice is exactly 2 bytes"));
    let max_degree = u16::from_le_bytes(buf[8..10].try_into().expect("slice is exactly 2 bytes"));
    let beam_width = u16::from_le_bytes(buf[10..12].try_into().expect("slice is exactly 2 bytes"));
    let alpha = f32::from_le_bytes(buf[12..16].try_into().expect("slice is exactly 4 bytes"));
    let epoch = u64::from_le_bytes(buf[16..24].try_into().expect("slice is exactly 8 bytes"));
    let count = u64::from_le_bytes(buf[24..32].try_into().expect("slice is exactly 8 bytes"));
    let entry_point_raw = u64::from_le_bytes(buf[32..40].try_into().expect("slice is exactly 8 bytes"));

    Ok(VectorIndexMeta {
        params: VectorIndexParams {
            dim: usize::from(dim),
            max_degree: usize::from(max_degree),
            beam_width: usize::from(beam_width),
            alpha,
        },
        epoch,
        count,
        entry_point: (count > 0).then_some(entry_point_raw),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_values() {
        let params = VectorIndexParams::default();
        assert_eq!(params.dim, 384);
        assert_eq!(params.max_degree, 32);
        assert_eq!(params.beam_width, 128);
        assert!((params.alpha - 1.2).abs() < 1e-6);
    }

    #[test]
    fn with_dim_only_changes_the_dimension() {
        let params = VectorIndexParams::with_dim(8);
        assert_eq!(params.dim, 8);
        assert_eq!(params.max_degree, DEFAULT_MAX_DEGREE);
    }

    fn sample() -> VectorIndexMeta {
        VectorIndexMeta {
            params: VectorIndexParams::default(),
            epoch: 3,
            count: 42,
            entry_point: Some(7),
        }
    }

    #[test]
    fn roundtrips_a_meta_record() {
        let meta = sample();
        let bytes = encode(&meta).expect("encode ok");
        assert_eq!(bytes.len(), META_LEN);
        assert_eq!(decode(&bytes).expect("decode ok"), meta);
    }

    #[test]
    fn roundtrips_an_empty_index_meta() {
        let meta = VectorIndexMeta {
            params: VectorIndexParams::with_dim(16),
            epoch: 0,
            count: 0,
            entry_point: None,
        };
        let bytes = encode(&meta).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), meta);
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&sample()).expect("encode ok");
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated record must not decode");
            assert!(
                matches!(err, EngineError::CorruptVectorIndexMeta { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[20] ^= 0xFF; // flip a byte of the epoch field
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptVectorIndexMeta { .. }));
    }

    #[test]
    fn unknown_version_is_unsupported_error() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[4] = 0xFF;
        let crc_at = META_LEN - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("unknown version must be rejected");
        assert!(matches!(
            err,
            EngineError::UnsupportedVectorIndexMetaVersion { found: 0x00FF, .. }
        ));
    }

    #[test]
    fn oversized_params_are_rejected_at_encode_time() {
        let meta = VectorIndexMeta {
            params: VectorIndexParams {
                dim: usize::from(u16::MAX) + 1,
                ..VectorIndexParams::default()
            },
            epoch: 0,
            count: 0,
            entry_point: None,
        };
        let err = encode(&meta).expect_err("dim beyond u16 must not silently truncate");
        assert!(matches!(err, EngineError::CorruptVectorIndexMeta { .. }));
    }
}
