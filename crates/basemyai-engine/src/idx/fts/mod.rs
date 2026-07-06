//! Native full-text index — Layer 2 (N5.2, ADR-028): BM25 over a hand-rolled
//! inverted index, scoped to the narrow `match_expr` subset
//! `basemyai`'s `fts_match_expr()` actually produces — not general FTS5
//! query syntax (ADR-028 §1).
//!
//! - [`tokenizer`] — split + Unicode-lowercase + diacritic-fold (Porter
//!   stemming deliberately deferred, ADR-028 §2).
//! - [`postings`] — the `FtsPosting` wire block (`FtsPosting:1`): term
//!   frequency of one `(agent, term, vec_id)` (`agent`/`term`/`vec_id` live
//!   in the key, [`crate::key::fts_index`]).
//! - [`docterms`] — the `FtsDocTerms` wire block (`FtsDocTerms:1`): the
//!   forward index (doc -> every `(term, tf)`), needed for precise deletes
//!   and document length without depending on `idx::memory`.
//! - [`stats`] — the `FtsStats` wire block (`FtsStats:1`): per-agent BM25
//!   aggregates (`doc_count`, `total_terms` -> `avgdl`).
//! - [`persistent`] — [`PersistentFts`], composing the three into
//!   insert/delete staging (fused into the caller's atomic batch, ADR-028
//!   §4) and BM25 search.

pub mod docterms;
pub mod persistent;
pub mod postings;
pub mod stats;
pub mod tokenizer;

pub use persistent::PersistentFts;
pub use stats::FtsStats;
