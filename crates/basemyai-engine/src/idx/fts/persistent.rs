// SPDX-License-Identifier: BUSL-1.1
//! KV-persisted full-text index (N5.2, ADR-028): postings, forward index and
//! per-agent BM25 stats under the reserved `idx/fts/` keyspace
//! ([`crate::key::fts_index`]). Isolation by agent is **structural**, same
//! discipline as `idx::graph`/`idx::memory`.
//!
//! No RAM cache (unlike [`crate::idx::memory::PersistentMemoryIndex`], which
//! caches the global `next_vec_id`): stats are per-agent and unbounded in
//! number, so caching all of them would need eviction or leak proportionally
//! to the agent count (ADR-028 §3, "cache RAM des stats" rejected
//! alternative). Every operation reads and writes through the [`Engine`]
//! directly — not the hot path, unlike search.
//!
//! ## Crash-critical composition lives at the caller
//!
//! [`PersistentFts::stage_insert`]/[`PersistentFts::stage_delete`] never call
//! `engine.apply_batch` themselves: they stage postings + doc-terms + stats
//! updates into the caller-supplied [`Batch`], which
//! [`crate::idx::memory::PersistentMemoryIndex::put`]/`forget` fold into the
//! **same** atomic batch as the memory record and vector node (ADR-028 §4,
//! extending the couture ADR-027 §3 established for those two). A `remember`
//! stays one WAL record end to end.

use std::collections::HashMap;

use crate::error::{EngineError, Result};
use crate::key::fts_index;
use crate::store::{Batch, Engine};

use super::docterms;
use super::postings::{self, FtsPosting};
use super::stats::{self, FtsStats};
use super::tokenizer;

/// Okapi BM25 parameters — SQLite FTS5's `bm25()` defaults (ADR-014 does not
/// override them, so this is the parity target, ADR-028 §5).
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Handle over the KV-persisted full-text index. Holds no state (see module
/// doc) — safe and cheap to construct per call site.
#[derive(Debug, Default, Clone, Copy)]
pub struct PersistentFts;

impl PersistentFts {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Stages the postings + forward-index + stats bump for indexing
    /// `content` as document `vec_id` of `agent`, into `batch`. Writes
    /// nothing (stages nothing) if `content` tokenizes to zero terms — an
    /// empty/punctuation-only memory is simply never matchable, like an
    /// FTS5 row whose tokenizer produces no tokens.
    ///
    /// Reads the agent's current stats via `engine` to compute the updated
    /// aggregate; does not itself call `engine.apply_batch` (see module doc).
    /// This read-modify-write means two documents of the **same agent**
    /// must not be staged into one shared `batch` through two *separate*
    /// calls before either is applied — the second call would read stale
    /// (pre-first-insert) stats. Staging several same-agent documents into
    /// one batch is [`Self::stage_insert_many`]'s job (it accumulates the
    /// aggregate across documents and stages the stats record once).
    pub fn stage_insert(
        &self,
        engine: &Engine,
        agent: &str,
        vec_id: u64,
        content: &str,
        batch: &mut Batch,
    ) -> Result<()> {
        self.stage_insert_many(engine, agent, &[(vec_id, content)], batch)
    }

    /// Stages the postings + forward-index entries of **several** documents
    /// of one `agent`, plus a **single** stats record carrying the whole
    /// group's aggregate, into `batch` — the batched sibling of
    /// [`Self::stage_insert`], and what makes an all-or-nothing
    /// `put_memory_batch` possible (N5.5): the per-document
    /// read-modify-write on the stats record would otherwise make later
    /// stagings in the same (not yet applied) batch read stale stats and
    /// silently lose the earlier documents' counts. Documents tokenizing to
    /// zero terms stage nothing, like the single-document path.
    pub fn stage_insert_many(
        &self,
        engine: &Engine,
        agent: &str,
        docs: &[(u64, &str)],
        batch: &mut Batch,
    ) -> Result<()> {
        let mut agent_stats = self.load_stats(engine, agent)?;
        let mut staged_any = false;
        for (vec_id, content) in docs {
            let doc = docterms::from_content(content);
            if doc.terms.is_empty() {
                continue;
            }
            for term in &doc.terms {
                let key = fts_index::postings_key(agent, &term.term, *vec_id)?;
                batch.put(key.as_bytes(), &postings::encode(&FtsPosting { tf: term.tf })?);
            }
            let docterms_key = fts_index::docterms_key(agent, *vec_id)?;
            batch.put(docterms_key.as_bytes(), &docterms::encode(&doc)?);
            agent_stats.doc_count += 1;
            agent_stats.total_terms += docterms::doc_length(&doc);
            staged_any = true;
        }
        if staged_any {
            let meta_key = fts_index::meta_key(agent)?;
            batch.put(meta_key.as_bytes(), &stats::encode(&agent_stats)?);
        }
        Ok(())
    }

    /// Stages removal of document `vec_id`'s FTS entries into `batch`.
    /// No-op (stages nothing) if the document was never indexed — empty
    /// content at insert time, or already removed by an interrupted earlier
    /// attempt — mirroring the idempotence [`crate::idx::memory::PersistentMemoryIndex::forget`]
    /// already establishes for the record + vector node.
    pub fn stage_delete(&self, engine: &Engine, agent: &str, vec_id: u64, batch: &mut Batch) -> Result<()> {
        let docterms_key = fts_index::docterms_key(agent, vec_id)?;
        let Some(bytes) = engine.get(docterms_key.as_bytes())? else {
            return Ok(());
        };
        let doc = docterms::decode(&bytes)?;

        for term in &doc.terms {
            let key = fts_index::postings_key(agent, &term.term, vec_id)?;
            batch.delete(key.as_bytes());
        }
        batch.delete(docterms_key.as_bytes());

        let mut agent_stats = self.load_stats(engine, agent)?;
        agent_stats.doc_count = agent_stats.doc_count.saturating_sub(1);
        agent_stats.total_terms = agent_stats.total_terms.saturating_sub(docterms::doc_length(&doc));
        let meta_key = fts_index::meta_key(agent)?;
        batch.put(meta_key.as_bytes(), &stats::encode(&agent_stats)?);
        Ok(())
    }

    /// Loads the BM25 stats for `agent`: `Ok(default)` for an agent with no
    /// record yet (never written — not corruption), healed from that
    /// agent's `docterms` on an actual decode failure (ADR-028 §3).
    fn load_stats(self, engine: &Engine, agent: &str) -> Result<FtsStats> {
        let key = fts_index::meta_key(agent)?;
        match engine.get(key.as_bytes())? {
            Some(bytes) => match stats::decode(&bytes) {
                Ok(loaded) => Ok(loaded),
                Err(EngineError::CorruptFtsStats { .. }) => self.heal_stats(engine, agent),
                Err(other) => Err(other),
            },
            None => Ok(FtsStats::default()),
        }
    }

    /// Recomputes `agent`'s stats from its `docterms` records — safe because
    /// `docterms` and the stats record always advance in the same atomic
    /// batch (ADR-028 §4): if `docterms` for a doc exists, that doc was
    /// already counted in a well-formed stats record, so a full rescan
    /// reproduces the same aggregate a healthy record would hold.
    fn heal_stats(self, engine: &Engine, agent: &str) -> Result<FtsStats> {
        let prefix = fts_index::docterms_agent_prefix(agent)?;
        let mut healed = FtsStats::default();
        for (_, value) in engine.scan_prefix(&prefix)? {
            let doc = docterms::decode(&value)?;
            healed.doc_count += 1;
            healed.total_terms += docterms::doc_length(&doc);
        }
        Ok(healed)
    }

    /// BM25 (Okapi, `k1`/`b` = SQLite FTS5 defaults, ADR-028 §5) search over
    /// the narrow `match_expr` subset `fts_match_expr()` produces (ADR-028
    /// §1) — parsed by [`parse_match_expr`]. Returns `(vec_id, score)`
    /// pairs, **highest score first**, truncated to `k`. A document
    /// matching none of the query's terms is never scored (OR semantics,
    /// like FTS5's `MATCH`).
    ///
    /// # Errors
    /// [`EngineError::UnsupportedMatchExpr`] if `match_expr` is not in the
    /// supported subset — a franc error, never a partial best-effort parse
    /// (ADR-028 §6). Storage errors propagate from corrupt postings/doc-terms
    /// blocks (never silently skipped — only a genuinely *absent* companion
    /// record, impossible in steady state under the atomicity guarantee of
    /// §4 but defended against anyway, is skipped).
    pub fn search_bm25(&self, engine: &Engine, agent: &str, match_expr: &str, k: usize) -> Result<Vec<(u64, f32)>> {
        let terms = parse_match_expr(match_expr)?;
        if terms.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let agent_stats = self.load_stats(engine, agent)?;
        if agent_stats.doc_count == 0 {
            return Ok(Vec::new());
        }
        let avgdl = agent_stats.avgdl();
        let n = agent_stats.doc_count as f64;

        let mut scores: HashMap<u64, f64> = HashMap::new();
        let mut lengths: HashMap<u64, u64> = HashMap::new();

        for term in &terms {
            let prefix = fts_index::postings_term_prefix(agent, term)?;
            let hits = engine.scan_prefix(&prefix)?;
            if hits.is_empty() {
                continue;
            }
            let df = hits.len() as f64;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();

            for (key, value) in hits {
                let Some(vec_id) = fts_index::postings_vec_id(prefix.len(), key.as_bytes()) else {
                    // Structurally impossible from our own writes under this
                    // exact prefix; never trust a KV read more than any
                    // other decode path.
                    continue;
                };
                let posting = postings::decode(&value)?;
                let doc_len = match lengths.get(&vec_id) {
                    Some(len) => *len,
                    None => {
                        let doc_key = fts_index::docterms_key(agent, vec_id)?;
                        let Some(len) = self.doc_length_of(engine, doc_key.as_bytes())? else {
                            // A posting with no companion doc-terms record:
                            // impossible under §4's atomicity in steady
                            // state (a leftover of neither, never one
                            // without the other) — skip defensively rather
                            // than fabricate a length.
                            continue;
                        };
                        lengths.insert(vec_id, len);
                        len
                    }
                };
                let tf = f64::from(posting.tf);
                let denom = tf + K1 * (1.0 - B + B * (doc_len as f64 / avgdl));
                let term_score = idf * (tf * (K1 + 1.0)) / denom;
                *scores.entry(vec_id).or_insert(0.0) += term_score;
            }
        }

        let mut ranked: Vec<(u64, f64)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        ranked.truncate(k);
        Ok(ranked.into_iter().map(|(id, score)| (id, score as f32)).collect())
    }

    fn doc_length_of(self, engine: &Engine, doc_key_bytes: &[u8]) -> Result<Option<u64>> {
        let Some(bytes) = engine.get(doc_key_bytes)? else {
            return Ok(None);
        };
        Ok(Some(docterms::doc_length(&docterms::decode(&bytes)?)))
    }
}

/// Parses the narrow `match_expr` subset ADR-028 §1 defines: a sequence of
/// double-quoted tokens joined by the literal separator `" OR "`, nothing
/// else. Returns the tokens **folded** through [`tokenizer::fold`] — query
/// tokens arrive already split and lowercased by `fts_match_expr()`, so only
/// diacritic folding (not re-splitting) is applied, matching the fold step
/// `tokenizer::tokenize` already applied to indexed content.
///
/// A franc [`EngineError::UnsupportedMatchExpr`] for anything outside this
/// exact shape — empty input, an unquoted or empty-quoted segment, any
/// operator other than literal ` OR ` — never a best-effort partial parse
/// (ADR-028 §1/§6).
fn parse_match_expr(match_expr: &str) -> Result<Vec<String>> {
    let unsupported = |reason: String| EngineError::UnsupportedMatchExpr {
        match_expr: match_expr.to_string(),
        reason,
    };
    if match_expr.is_empty() {
        return Err(unsupported("empty match_expr".to_string()));
    }
    let mut terms = Vec::new();
    for segment in match_expr.split(" OR ") {
        let inner = segment
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or_else(|| unsupported(format!("segment {segment:?} is not a double-quoted token")))?;
        if inner.is_empty() || inner.contains('"') {
            return Err(unsupported(format!(
                "segment {segment:?} has an empty or embedded quote"
            )));
        }
        terms.push(tokenizer::fold(inner));
    }
    Ok(terms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Engine as StoreEngine;

    fn open(dir: &std::path::Path) -> StoreEngine {
        StoreEngine::open(dir).expect("open engine")
    }

    #[test]
    fn parse_match_expr_accepts_the_fts_match_expr_shape() {
        assert_eq!(
            parse_match_expr(r#""chat" OR "chien""#).expect("parse"),
            vec!["chat".to_string(), "chien".to_string()]
        );
        assert_eq!(parse_match_expr(r#""chat""#).expect("parse"), vec!["chat".to_string()]);
    }

    #[test]
    fn parse_match_expr_folds_accents_without_resplitting() {
        assert_eq!(
            parse_match_expr(r#""ecole""#).expect("parse"),
            vec!["ecole".to_string()]
        );
        // A pre-folded query token stays intact even if it contained an
        // (unexpected but harmless) accented byte sequence.
        assert_eq!(parse_match_expr(r#""déjà""#).expect("parse"), vec!["deja".to_string()]);
    }

    #[test]
    fn parse_match_expr_rejects_anything_outside_the_subset() {
        for bad in [
            "",
            "chat",                  // unquoted
            r#""chat" AND "chien""#, // wrong operator
            r#""""#,                 // empty quoted token
            r#""chat" NEAR "chien""#,
            r#"chat OR chien"#,
        ] {
            let err = parse_match_expr(bad).expect_err(&format!("{bad:?} must be rejected"));
            assert!(matches!(err, EngineError::UnsupportedMatchExpr { .. }));
        }
    }

    #[test]
    fn stage_insert_then_search_finds_the_document() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = open(dir.path());
        let fts = PersistentFts::new();

        let mut batch = Batch::new();
        fts.stage_insert(&engine, "agent-a", 1, "le chat dort sur le tapis", &mut batch)
            .expect("stage insert");
        engine.apply_batch(&batch).expect("apply");

        let hits = fts.search_bm25(&engine, "agent-a", r#""chat""#, 10).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 1);
        assert!(hits[0].1 > 0.0);
    }

    #[test]
    fn search_is_isolated_by_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = open(dir.path());
        let fts = PersistentFts::new();

        let mut batch = Batch::new();
        fts.stage_insert(&engine, "agent-a", 1, "chat", &mut batch)
            .expect("stage");
        engine.apply_batch(&batch).expect("apply");

        assert!(
            fts.search_bm25(&engine, "agent-b", r#""chat""#, 10)
                .expect("search")
                .is_empty()
        );
    }

    #[test]
    fn search_ranks_more_relevant_documents_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = open(dir.path());
        let fts = PersistentFts::new();

        // Doc 1 mentions "chat" once in a long document (lower tf, longer
        // |D|); doc 2 mentions it three times in a short document — BM25
        // should rank doc 2 above doc 1. Each staged insert is applied
        // before the next is staged — the real production sequencing
        // (`PersistentMemoryIndex::put` applies one memory at a time via
        // `insert_with`), since `stage_insert`'s stats read-modify-write
        // only sees previously *committed* state, not another pending
        // batch's staged-but-unapplied write.
        let mut batch1 = Batch::new();
        fts.stage_insert(
            &engine,
            "agent-a",
            1,
            "chat oiseau souris jardin arbre maison voiture route ville",
            &mut batch1,
        )
        .expect("stage 1");
        engine.apply_batch(&batch1).expect("apply 1");

        let mut batch2 = Batch::new();
        fts.stage_insert(&engine, "agent-a", 2, "chat chat chat", &mut batch2)
            .expect("stage 2");
        engine.apply_batch(&batch2).expect("apply 2");

        let hits = fts.search_bm25(&engine, "agent-a", r#""chat""#, 10).expect("search");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, 2, "doc with higher tf and shorter length should rank first");
    }

    #[test]
    fn search_with_no_matching_term_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = open(dir.path());
        let fts = PersistentFts::new();

        let mut batch = Batch::new();
        fts.stage_insert(&engine, "agent-a", 1, "chat", &mut batch)
            .expect("stage");
        engine.apply_batch(&batch).expect("apply");

        assert!(
            fts.search_bm25(&engine, "agent-a", r#""dinosaure""#, 10)
                .expect("search")
                .is_empty()
        );
    }

    #[test]
    fn search_of_empty_agent_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = open(dir.path());
        let fts = PersistentFts::new();
        assert!(
            fts.search_bm25(&engine, "agent-a", r#""chat""#, 10)
                .expect("search")
                .is_empty()
        );
    }

    #[test]
    fn stage_insert_of_empty_content_indexes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = open(dir.path());
        let fts = PersistentFts::new();

        let mut batch = Batch::new();
        fts.stage_insert(&engine, "agent-a", 1, "   !!! ,,,", &mut batch)
            .expect("stage empty");
        assert!(batch.is_empty(), "no terms means nothing staged, not even stats");
        engine.apply_batch(&batch).expect("apply");

        assert_eq!(fts.load_stats(&engine, "agent-a").expect("stats"), FtsStats::default());
    }

    #[test]
    fn stage_delete_removes_postings_and_updates_stats() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = open(dir.path());
        let fts = PersistentFts::new();

        let mut insert_batch = Batch::new();
        fts.stage_insert(&engine, "agent-a", 1, "chat chien", &mut insert_batch)
            .expect("stage insert");
        engine.apply_batch(&insert_batch).expect("apply insert");
        assert_eq!(
            fts.load_stats(&engine, "agent-a").expect("stats"),
            FtsStats {
                doc_count: 1,
                total_terms: 2
            }
        );

        let mut delete_batch = Batch::new();
        fts.stage_delete(&engine, "agent-a", 1, &mut delete_batch)
            .expect("stage delete");
        engine.apply_batch(&delete_batch).expect("apply delete");

        assert_eq!(fts.load_stats(&engine, "agent-a").expect("stats"), FtsStats::default());
        assert!(
            fts.search_bm25(&engine, "agent-a", r#""chat""#, 10)
                .expect("search")
                .is_empty()
        );
    }

    #[test]
    fn stage_delete_of_never_indexed_doc_is_a_silent_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = open(dir.path());
        let fts = PersistentFts::new();

        let mut batch = Batch::new();
        fts.stage_delete(&engine, "agent-a", 999, &mut batch)
            .expect("stage delete of absent doc");
        assert!(batch.is_empty());
    }

    #[test]
    fn stats_survive_close_and_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fts = PersistentFts::new();
        {
            let mut engine = open(dir.path());
            let mut batch = Batch::new();
            fts.stage_insert(&engine, "agent-a", 1, "chat chien chat", &mut batch)
                .expect("stage");
            engine.apply_batch(&batch).expect("apply");
            engine.close().expect("close");
        }
        let engine = open(dir.path());
        assert_eq!(
            fts.load_stats(&engine, "agent-a").expect("stats"),
            FtsStats {
                doc_count: 1,
                total_terms: 3
            }
        );
        let hits = fts
            .search_bm25(&engine, "agent-a", r#""chat""#, 10)
            .expect("search after reopen");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn heal_stats_recomputes_from_docterms_when_meta_is_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = open(dir.path());
        let fts = PersistentFts::new();

        let mut batch = Batch::new();
        fts.stage_insert(&engine, "agent-a", 1, "chat chien", &mut batch)
            .expect("stage");
        engine.apply_batch(&batch).expect("apply");

        let meta_key = fts_index::meta_key("agent-a").expect("meta key");
        engine.put(meta_key.as_bytes(), b"garbage").expect("corrupt meta");

        let healed = fts.load_stats(&engine, "agent-a").expect("heal");
        assert_eq!(
            healed,
            FtsStats {
                doc_count: 1,
                total_terms: 2
            }
        );
    }
}
