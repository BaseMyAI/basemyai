// SPDX-License-Identifier: BUSL-1.1
//! `format.lock` mechanism — the anti-drift guard rail for Layer 1's on-disk
//! formats, modeled on SurrealDB's `revision.lock`
//! (`docs/PLAN-NATIVE-ENGINE.md` §3.1/§4, `docs/adr/ADR-025-native-engine-storage-foundation.md`).
//!
//! ## Why this exists
//!
//! A home-grown storage engine inherits none of the decades of format
//! hardening a project like SQLite has. The only thing standing between an
//! "innocent refactor" and silent on-disk corruption for existing users is
//! *someone noticing* that a wire format changed. Humans are bad at noticing
//! that from a diff of struct fields. This module makes it a build-time,
//! CI-gated fact instead: every persisted type has a canonical
//! [`FormatSpec`] (field name + wire-type tag, in on-disk order) computed
//! from the same source that encodes/decodes it, hashed, and compared
//! against a hash committed in `format.lock`. Any divergence is a hard
//! failure — never a warning.
//!
//! ## What gets hashed, and why
//!
//! The hash is **not** taken over the prose doc comment (comment wording
//! fixes, typo corrections, or rewording would spuriously break the lock —
//! noise that trains developers to stop trusting, and eventually to ignore,
//! the check). It is **not** derived automatically from the Rust struct via
//! e.g. a `#[derive]` over field types either: `WalRecord`/`SstEntry` are
//! *decoded, in-memory* representations (`Vec<u8>`, `Option<Vec<u8>>`) that
//! deliberately do not mirror the wire layout 1:1 (there is no `magic` or
//! `crc32` field on the struct — those are transport, not payload). Hashing
//! the Rust struct would miss changes to the framing entirely.
//!
//! Instead each format module hand-maintains a small [`FormatSpec`] value —
//! the *wire* field list in on-disk order, immediately next to the
//! `encode`/`decode` functions it describes, so a reviewer sees spec and
//! codec change together in a diff. It is exactly as easy to keep in sync as
//! the byte-layout doc comment already sitting above it (same discipline,
//! now machine-checked) and exactly as hard to game by accident as any other
//! change that requires touching the same few lines a wire-format change
//! already requires touching.
//!
//! The hash function itself is this crate's own CRC32 ([`super::checksum`])
//! — already present for WAL/SST corruption detection, zero new dependency,
//! and adequate here: this is a drift *detector*, not a security boundary,
//! so collision-resistance against an adversary is not the threat model —
//! catching an accidental one-field change is.

use std::fmt;
use std::fs;
use std::path::Path;

use super::checksum::crc32;

/// One field in a persisted type's on-disk layout, in wire order.
///
/// `wire_type` is a short canonical tag describing width/shape on disk —
/// `"u8"`, `"u16"`, `"u32"`, `"u64"`, `"bytes(key_len)"`,
/// `"bytes(val_len)?"` (the trailing `?` marks a field omitted for some
/// variants, e.g. a WAL delete record's value) — never a Rust type name, so
/// a purely in-memory refactor (`Vec<u8>` -> `Box<[u8]>`, renaming the
/// decoded struct) does not perturb the hash, while a real layout change
/// (new field, reordered field, widened integer, newly-optional field) does.
pub type Field = (&'static str, &'static str);

/// Canonical wire-format description of one persisted type, hashed into
/// `format.lock`.
#[derive(Debug, Clone, Copy)]
pub struct FormatSpec {
    /// Matches the `format.lock anchor:` name used in the type's byte-layout
    /// doc comment (e.g. `"WalRecord"`, `"SstFile"`).
    pub name: &'static str,
    /// The `*_VERSION` constant guarding this format (`WAL_RECORD_VERSION`,
    /// `SST_FORMAT_VERSION`, ...).
    pub version: u16,
    /// Fields in on-disk order, including framing (magic/length/checksum),
    /// not just the payload.
    pub fields: &'static [Field],
}

impl FormatSpec {
    /// Deterministic hash of `(name, version, fields)`. Any change to any of
    /// the three — including field order — changes the hash.
    pub fn hash_hex(&self) -> String {
        let mut buf = Vec::new();
        buf.extend_from_slice(self.name.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&self.version.to_le_bytes());
        for (field_name, wire_type) in self.fields {
            buf.extend_from_slice(field_name.as_bytes());
            buf.push(b':');
            buf.extend_from_slice(wire_type.as_bytes());
            buf.push(0);
        }
        format!("{:08x}", crc32(&buf))
    }

    /// The exact `TypeName:version(hash)` line this spec should have in
    /// `format.lock`.
    pub fn lock_line(&self) -> String {
        format!("{}:{}({})", self.name, self.version, self.hash_hex())
    }
}

impl fmt::Display for FormatSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.lock_line())
    }
}

/// Every persisted type this crate currently defines. Adding a new persisted
/// type (a new module under `format/`, or an index block format under
/// `idx/` — e.g. `idx::vector::node`) means adding its `spec()` here *and*
/// a corresponding line in `format.lock` — the verifier below fails loudly
/// if the two ever disagree, in either direction (missing entry or extra
/// entry).
pub fn all_specs() -> Vec<FormatSpec> {
    vec![
        super::wal::spec(),
        super::crypto::crypto_meta_spec(),
        super::crypto::wal_envelope_spec(),
        super::crypto::encrypted_sst_block_spec(),
        super::sst_block::sst_header_spec(),
        super::sst_block::sst_data_block_spec(),
        super::sst_block::sst_block_index_spec(),
        super::sst_block::sst_bloom_filter_spec(),
        super::sst_block::sst_footer_spec(),
        super::store_meta::spec(),
        crate::idx::vector::node::spec(),
        crate::idx::vector::meta::spec(),
        crate::idx::graph::entity::spec(),
        crate::idx::graph::edge::spec(),
        crate::idx::memory::record::spec(),
        crate::idx::memory::vecmap::spec(),
        crate::idx::memory::meta::spec(),
        crate::idx::fts::postings::spec(),
        crate::idx::fts::docterms::spec(),
        crate::idx::fts::stats::spec(),
    ]
}

/// One discrepancy between the current source and the committed
/// `format.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// `format.lock` has no line for a type `all_specs()` knows about.
    MissingFromLock { name: &'static str, current_line: String },
    /// `format.lock` has a line for a type that no longer exists in source.
    StaleInLock { line: String },
    /// The committed hash doesn't match the hash recomputed from source —
    /// the wire format drifted without a deliberate version bump + lock
    /// update.
    HashDiverged {
        name: &'static str,
        locked_line: String,
        current_line: String,
    },
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFromLock { name, current_line } => write!(
                f,
                "`{name}` is a persisted type in source but has no entry in format.lock. \
                 Add this line to format.lock: `{current_line}`."
            ),
            Self::StaleInLock { line } => write!(
                f,
                "format.lock has an entry with no matching persisted type in source: `{line}`. \
                 Remove it if the type was deliberately deleted, or check `all_specs()` if it \
                 was renamed."
            ),
            Self::HashDiverged {
                name,
                locked_line,
                current_line,
            } => write!(
                f,
                "`{name}`'s on-disk wire format no longer matches format.lock.\n  \
                 locked:  {locked_line}\n  current: {current_line}\n\
                 This means one of two things:\n  \
                 (a) you changed the wire format on purpose — bump the `*_VERSION` constant, \
                 update the byte-layout doc comment AND the `spec()` field list together, then \
                 update format.lock to `{current_line}`.\n  \
                 (b) you didn't mean to change it — revert your change to \
                 `format/{{wal,sst_block,crypto,store_meta}}.rs` or `idx/vector/node.rs`."
            ),
        }
    }
}

/// Parses `format.lock`'s `TypeName:version(hash)` lines, ignoring blank
/// lines and `#`-prefixed comments. Returns `(name, version, hash)` triples
/// in file order.
fn parse_lock(contents: &str) -> Vec<(String, u16, String)> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let (version_str, hash_paren) = rest.split_once('(')?;
            let hash = hash_paren.strip_suffix(')')?;
            let version: u16 = version_str.parse().ok()?;
            Some((name.to_string(), version, hash.to_string()))
        })
        .collect()
}

/// Verifies every [`all_specs`] entry against the `format.lock` file at
/// `lock_path`. Returns every discrepancy found (empty `Vec` == clean).
pub fn verify_file(lock_path: &Path) -> Result<Vec<Mismatch>, std::io::Error> {
    let contents = fs::read_to_string(lock_path)?;
    let locked = parse_lock(&contents);
    let specs = all_specs();

    let mut mismatches = Vec::new();

    for spec in &specs {
        let current_line = spec.lock_line();
        match locked.iter().find(|(name, _, _)| name == spec.name) {
            None => mismatches.push(Mismatch::MissingFromLock {
                name: spec.name,
                current_line,
            }),
            Some((_, locked_version, locked_hash)) => {
                let locked_line = format!("{}:{}({})", spec.name, locked_version, locked_hash);
                if *locked_version != spec.version || *locked_hash != spec.hash_hex() {
                    mismatches.push(Mismatch::HashDiverged {
                        name: spec.name,
                        locked_line,
                        current_line,
                    });
                }
            }
        }
    }

    for (name, version, hash) in &locked {
        if !specs.iter().any(|s| s.name == name) {
            mismatches.push(Mismatch::StaleInLock {
                line: format!("{name}:{version}({hash})"),
            });
        }
    }

    Ok(mismatches)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> FormatSpec {
        FormatSpec {
            name: "Sample",
            version: 1,
            fields: &[("a", "u32"), ("b", "u8")],
        }
    }

    #[test]
    fn hash_is_deterministic() {
        let spec = sample_spec();
        assert_eq!(spec.hash_hex(), spec.hash_hex());
    }

    #[test]
    fn reordering_fields_changes_hash() {
        let original = sample_spec();
        let reordered = FormatSpec {
            fields: &[("b", "u8"), ("a", "u32")],
            ..original
        };
        assert_ne!(original.hash_hex(), reordered.hash_hex());
    }

    #[test]
    fn widening_a_field_changes_hash() {
        let original = sample_spec();
        let widened = FormatSpec {
            fields: &[("a", "u64"), ("b", "u8")],
            ..original
        };
        assert_ne!(original.hash_hex(), widened.hash_hex());
    }

    #[test]
    fn bumping_version_changes_hash_even_with_same_fields() {
        let original = sample_spec();
        let bumped = FormatSpec { version: 2, ..original };
        assert_ne!(original.hash_hex(), bumped.hash_hex());
    }

    #[test]
    fn parse_lock_ignores_comments_and_blank_lines() {
        let parsed = parse_lock("# comment\n\nWalRecord:1(deadbeef)\n  \nSstFile:1(cafef00d)\n");
        assert_eq!(
            parsed,
            vec![
                ("WalRecord".to_string(), 1, "deadbeef".to_string()),
                ("SstFile".to_string(), 1, "cafef00d".to_string()),
            ]
        );
    }

    #[test]
    fn verify_file_clean_lock_has_no_mismatches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("format.lock");
        let lines: Vec<String> = all_specs().iter().map(FormatSpec::lock_line).collect();
        fs::write(&path, lines.join("\n")).expect("write lock");
        let mismatches = verify_file(&path).expect("verify");
        assert!(mismatches.is_empty(), "expected clean lock, got: {mismatches:?}");
    }

    #[test]
    fn verify_file_detects_diverged_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("format.lock");
        // Every current spec's name/version, paired with a hash that cannot
        // match any real spec — every entry must diverge.
        let specs = all_specs();
        let lines: Vec<String> = specs
            .iter()
            .map(|spec| format!("{}:{}(00000000)", spec.name, spec.version))
            .collect();
        fs::write(&path, lines.join("\n")).expect("write lock");
        let mismatches = verify_file(&path).expect("verify");
        assert_eq!(mismatches.len(), specs.len());
        assert!(mismatches.iter().all(|m| matches!(m, Mismatch::HashDiverged { .. })));
    }

    #[test]
    fn verify_file_detects_missing_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("format.lock");
        fs::write(&path, "SstFile:1(00000000)\n").expect("write lock");
        let mismatches = verify_file(&path).expect("verify");
        assert!(
            mismatches
                .iter()
                .any(|m| matches!(m, Mismatch::MissingFromLock { name: "WalRecord", .. }))
        );
    }

    #[test]
    fn verify_file_detects_stale_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("format.lock");
        let mut lines: Vec<String> = all_specs().iter().map(FormatSpec::lock_line).collect();
        lines.push("Ghost:1(00000000)".to_string());
        fs::write(&path, lines.join("\n")).expect("write lock");
        let mismatches = verify_file(&path).expect("verify");
        assert!(mismatches.iter().any(|m| matches!(m, Mismatch::StaleInLock { .. })));
    }
}
