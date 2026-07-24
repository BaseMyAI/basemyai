// SPDX-License-Identifier: BUSL-1.1
//! Provenance tag for graph content (ADR-045, AGENT-MEM-1).
//!
//! Mirrors `basemyai::memory::trust::TrustLevel` (ADR-036) in shape — same
//! four producers, same "no silent default toward the untrustworthy end"
//! posture — but lives here rather than being imported from `basemyai`:
//! `GraphEntity`/`GraphEdgeMeta` (the wire types this tags) live in this
//! crate, and `basemyai-core`/`basemyai-engine` never depend on `basemyai`
//! (the dependency runs the other way). `basemyai`'s callers use this same
//! type directly (re-exported at the crate root) rather than a parallel
//! duplicate — one taxonomy, not two.

use crate::error::{EngineError, Result};

/// Where a piece of graph content (an entity or an edge) came from.
///
/// `#[non_exhaustive]`: a future producer (e.g. an inference rule) adds a
/// variant without that being a breaking change for callers already
/// matching with a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GraphSource {
    /// Created directly by the user/calling agent (REST/MCP/CLI explicit
    /// write, or any caller not yet distinguishing sources — ADR-045 §2:
    /// the default for existing call sites, never a silent default for
    /// `Consolidation`/`Import`).
    User,
    /// Extracted by the LLM consolidation pipeline (`apply_extraction`,
    /// ADR-012) over a potentially adversarial episode.
    Consolidation,
    /// Reimported from a JSONL export (`import_rows`) — always this,
    /// **even if** the imported row claims another source (anti-spoof,
    /// same discipline as ADR-036 for memories).
    Import,
    /// Derived by a future internal rule without going through the LLM.
    /// Not produced anywhere yet — reserved.
    Inferred,
}

const TAG_USER: u8 = 0;
const TAG_CONSOLIDATION: u8 = 1;
const TAG_IMPORT: u8 = 2;
const TAG_INFERRED: u8 = 3;

impl GraphSource {
    /// One-byte wire tag (`GraphEntity:2`/`GraphEdge:2`, ADR-045 §4).
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::User => TAG_USER,
            Self::Consolidation => TAG_CONSOLIDATION,
            Self::Import => TAG_IMPORT,
            Self::Inferred => TAG_INFERRED,
        }
    }

    /// Decodes a wire tag. An unrecognized byte is corruption, not a
    /// forward-compat "Unknown" catch-all — unlike `TrustLevel::from_source`
    /// (a free-text SQL column with legitimate forward-compat needs), this
    /// is a fixed-width enum tag on a versioned binary format; any value
    /// outside the four defined here means the block is not a
    /// `GraphSource`-tagged record this build wrote.
    pub(crate) fn from_tag(tag: u8, corrupt: impl FnOnce(String) -> EngineError) -> Result<Self> {
        match tag {
            TAG_USER => Ok(Self::User),
            TAG_CONSOLIDATION => Ok(Self::Consolidation),
            TAG_IMPORT => Ok(Self::Import),
            TAG_INFERRED => Ok(Self::Inferred),
            other => Err(corrupt(format!("unrecognized GraphSource tag {other}"))),
        }
    }
}
