// SPDX-License-Identifier: BUSL-1.1
//! Conteneur-level integrity operations (ADR-040 §3, N9.6): `basemyai-engine`'s
//! `verify_store`/`plan_repair`/`rebuild_indexes`/`Engine::compact_now`,
//! exposed against a `.bmai` container path + encryption key instead of an
//! already-open `Engine` — the surface `basemyai-cli` drives (`verify
//! --physical/--logical`, `repair [--dry-run]`, `rebuild-indexes`, `compact`).
//! `key` is mandatory throughout: `basemyai` containers are always encrypted
//! (ADR-007), there is no plaintext case to handle here.
//!
//! Every function here is sync engine work wrapped in `spawn_blocking`, the
//! same bridge [`super::NativeMemoryStore`] uses — never a `Mutex` held
//! across `.await`. Open/crypto failures go through [`super::native_store`]'s
//! `map_engine_error`, the same translation [`super::NativeMemoryStore`]
//! uses, so a wrong key still surfaces as the typed `WrongEncryptionKey`
//! instead of collapsing into a generic storage error.
//!
//! [`verify_container`] never opens the store for real (`Engine::open_encrypted`
//! recovers a torn WAL tail on open, which would erase the exact anomaly a
//! `Quick` audit is meant to surface) — it goes straight to
//! `basemyai_engine::verify_store`'s own read-only pass. The repair/rebuild/
//! compact functions do open the store, because applying a fix inherently
//! writes.

use std::path::Path;

use basemyai_core::{Embedder, EncryptionKey, EncryptionKeyMode};
use basemyai_engine::key::memory_index;
use basemyai_engine::{Engine, PersistentMemoryIndex, PersistentVectorIndex, VectorIndexParams};
pub use basemyai_engine::{
    EngineStats, IntegrityIssue, IssueKind, RebuildReport, RepairAction, RepairPlan, VerifyMode, VerifyReport,
    plan_repair,
};

use super::native_store::map_engine_error;
use crate::Result;
use crate::error::MemoryError;

/// Only for the `spawn_blocking` join failure itself (a `JoinError`, never a
/// `basemyai_engine::EngineError`) — those go through [`map_engine_error`].
fn interrupted(op: &str, e: impl std::fmt::Display) -> MemoryError {
    basemyai_core::CoreError::Storage(format!("{op} interrupted: {e}")).into()
}

fn open_engine(path: &Path, key: &EncryptionKey) -> Result<Engine> {
    let result = match key.mode() {
        EncryptionKeyMode::RawKey => Engine::open_encrypted(path, key.expose().as_bytes()),
        EncryptionKeyMode::Passphrase => Engine::open_with_passphrase(path, key.expose().as_bytes()),
        _ => return Err(basemyai_core::CoreError::Storage("unsupported encryption key mode".to_string()).into()),
    };
    result.map_err(map_engine_error)
}

/// Audits the `.bmai` container at `path` without modifying it (ADR-040 §2).
///
/// # Errors
/// Storage errors opening/reading the directory (missing directory, wrong
/// key) — never from an integrity finding, which lands in the returned
/// [`VerifyReport`] instead (see `verify_store`'s own contract).
pub async fn verify_container(path: &Path, key: EncryptionKey, mode: VerifyMode) -> Result<VerifyReport> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let result = match key.mode() {
            EncryptionKeyMode::RawKey => basemyai_engine::verify_store(&path, Some(key.expose().as_bytes()), mode),
            EncryptionKeyMode::Passphrase => {
                basemyai_engine::verify_store_with_passphrase(&path, Some(key.expose().as_bytes()), mode)
            }
            _ => {
                return Err(basemyai_core::CoreError::Storage("unsupported encryption key mode".to_string()).into());
            }
        };
        result.map_err(map_engine_error)
    })
    .await
    .map_err(|e| interrupted("verify", e))?
}

/// Rebuilds derived indexes (vecmap/allocator, FTS, DiskANN graph) from
/// primary memory records — never touches memory or graph records (ADR-040
/// §3). Memory records whose vector was lost land in
/// [`RebuildReport::reembedding_required`] instead of being reinvented: the
/// engine has no embedding model by design (ADR-010), only `basemyai` does.
///
/// Unconditional: unlike the CLI's `repair` (which first verifies and
/// refuses when primary data is at risk), this does not gate on a preceding
/// audit — call it once you already know (from `verify --logical`) that only
/// derived structures need resyncing.
///
/// # Errors
/// Storage errors opening the container or rebuilding an index.
pub async fn rebuild_indexes_container(path: &Path, key: EncryptionKey) -> Result<RebuildReport> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut engine = open_engine(&path, &key)?;
        let report = basemyai_engine::rebuild_indexes(&mut engine).map_err(map_engine_error)?;
        engine.close().map_err(map_engine_error)?;
        Ok(report)
    })
    .await
    .map_err(|e| interrupted("rebuild-indexes", e))?
}

/// Full compaction (`Engine::compact_now`, ADR-040/N9.4): flush, then an
/// unconditional full merge — every live key ends up in one SST, tombstones
/// purged. Safe at any size, a no-op on an empty store. Returns
/// `(stats_before, stats_after)` so the caller can report bytes reclaimed.
///
/// # Errors
/// Storage errors opening or compacting the container.
pub async fn compact_container(path: &Path, key: EncryptionKey) -> Result<(EngineStats, EngineStats)> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut engine = open_engine(&path, &key)?;
        let before = engine.stats().map_err(map_engine_error)?;
        engine.compact_now().map_err(map_engine_error)?;
        let after = engine.stats().map_err(map_engine_error)?;
        engine.close().map_err(map_engine_error)?;
        Ok((before, after))
    })
    .await
    .map_err(|e| interrupted("compact", e))?
}

/// Outcome of a reembed pass ([`reembed_missing_container`],
/// [`reembed_ids_container`], [`reembed_all_container`]).
#[derive(Debug, Default)]
pub struct ReembedReport {
    /// Memories whose vector was (re)computed and written.
    pub reembedded: u64,
    /// Requested `(agent, id)` pairs that no longer exist — the memory was
    /// forgotten between listing and reembedding. Skipped, never an error.
    pub missing: Vec<(String, String)>,
}

/// Recomputes and rewrites the vector of every `(agent, id)` in `targets`,
/// in place, at each record's existing `vec_id` — never allocates a new one,
/// never touches the record/vecmap/FTS (primary and already-consistent
/// derived data). A record whose id is currently live gets replaced (delete
/// then reinsert, the same "update" [`PersistentVectorIndex::insert`]
/// documents); a record with no live vector (e.g. [`RebuildReport::
/// reembedding_required`]) is inserted directly.
fn reembed_targets(
    engine: &mut Engine,
    embedder: &dyn Embedder,
    targets: Vec<(String, String)>,
) -> Result<ReembedReport> {
    let memory = PersistentMemoryIndex::open(engine).map_err(map_engine_error)?;
    let mut vectors =
        PersistentVectorIndex::open(engine, VectorIndexParams::with_dim(embedder.dim())).map_err(map_engine_error)?;

    let mut resolved: Vec<(u64, String)> = Vec::new();
    let mut missing = Vec::new();
    for (agent, id) in targets {
        match memory.get(engine, &agent, &id).map_err(map_engine_error)? {
            Some(rec) => resolved.push((rec.vec_id, rec.content)),
            None => missing.push((agent, id)),
        }
    }

    let contents: Vec<String> = resolved.iter().map(|(_, content)| content.clone()).collect();
    // `Embedder::embed_batch` returns `basemyai_core::CoreError` — `?`
    // converts it via `MemoryError::Core`, same as everywhere else in this
    // crate that drives an injected `Embedder`.
    let fresh_vectors = embedder.embed_batch(&contents)?;

    let reembedded = resolved.len() as u64;
    for ((vec_id, _), vector) in resolved.into_iter().zip(fresh_vectors) {
        vectors.delete(engine, vec_id).map_err(map_engine_error)?;
        vectors.insert(engine, vec_id, vector).map_err(map_engine_error)?;
    }

    Ok(ReembedReport { reembedded, missing })
}

/// Reembeds every memory the store currently knows has lost its vector
/// (runs [`basemyai_engine::rebuild_indexes`] first to get an up-to-date
/// list — cheap and idempotent, and the natural completion of `basemyai
/// rebuild-indexes`/`repair`). Store-wide: unlike [`reembed_ids_container`]/
/// [`reembed_all_container`], no agent scope, since the underlying list
/// already spans every agent.
///
/// # Errors
/// Storage errors opening the container, rebuilding indexes, or embedding.
pub async fn reembed_missing_container(
    path: &Path,
    key: EncryptionKey,
    embedder: Box<dyn Embedder>,
) -> Result<ReembedReport> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut engine = open_engine(&path, &key)?;
        let rebuilt = basemyai_engine::rebuild_indexes(&mut engine).map_err(map_engine_error)?;
        let report = reembed_targets(&mut engine, embedder.as_ref(), rebuilt.reembedding_required)?;
        engine.close().map_err(map_engine_error)?;
        Ok(report)
    })
    .await
    .map_err(|e| interrupted("reembed", e))?
}

/// Reembeds specific memories of `agent`, unconditionally — whether or not
/// they currently have a live vector (e.g. after an embedding model change).
/// An id that does not exist for `agent` lands in
/// [`ReembedReport::missing`], never an error.
///
/// # Errors
/// Storage errors opening the container or embedding.
pub async fn reembed_ids_container(
    path: &Path,
    key: EncryptionKey,
    agent: &str,
    ids: Vec<String>,
    embedder: Box<dyn Embedder>,
) -> Result<ReembedReport> {
    let path = path.to_path_buf();
    let agent = agent.to_string();
    tokio::task::spawn_blocking(move || {
        let mut engine = open_engine(&path, &key)?;
        let targets = ids.into_iter().map(|id| (agent.clone(), id)).collect();
        let report = reembed_targets(&mut engine, embedder.as_ref(), targets)?;
        engine.close().map_err(map_engine_error)?;
        Ok(report)
    })
    .await
    .map_err(|e| interrupted("reembed", e))?
}

/// Reembeds every memory of `agent`, unconditionally (e.g. after an
/// embedding model change) — the bulk counterpart of
/// [`reembed_ids_container`].
///
/// # Errors
/// Storage errors opening the container, scanning `agent`'s records, or
/// embedding.
pub async fn reembed_all_container(
    path: &Path,
    key: EncryptionKey,
    agent: &str,
    embedder: Box<dyn Embedder>,
) -> Result<ReembedReport> {
    let path = path.to_path_buf();
    let agent = agent.to_string();
    tokio::task::spawn_blocking(move || {
        let mut engine = open_engine(&path, &key)?;
        let prefix = memory_index::record_agent_prefix(&agent).map_err(map_engine_error)?;
        let mut targets = Vec::new();
        for (record_key, _) in engine.scan_prefix(&prefix).map_err(map_engine_error)? {
            if let Some(id) = memory_index::record_id(prefix.len(), record_key.as_bytes()) {
                targets.push((agent.clone(), id));
            }
        }
        let report = reembed_targets(&mut engine, embedder.as_ref(), targets)?;
        engine.close().map_err(map_engine_error)?;
        Ok(report)
    })
    .await
    .map_err(|e| interrupted("reembed", e))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::NativeMemoryStore;

    #[tokio::test]
    async fn passphrase_store_supports_verify_rebuild_and_compact() {
        let dir = tempfile::tempdir().expect("tempdir");
        drop(NativeMemoryStore::open_with_passphrase(dir.path(), "human passphrase").expect("create store"));

        let report = verify_container(
            dir.path(),
            EncryptionKey::passphrase("human passphrase"),
            VerifyMode::FullLogical,
        )
        .await
        .expect("verify passphrase store");
        assert!(report.healthy, "verify errors: {:?}", report.errors);

        rebuild_indexes_container(dir.path(), EncryptionKey::passphrase("human passphrase"))
            .await
            .expect("rebuild passphrase store");
        compact_container(dir.path(), EncryptionKey::passphrase("human passphrase"))
            .await
            .expect("compact passphrase store");
    }
}
