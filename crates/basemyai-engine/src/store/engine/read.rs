// SPDX-License-Identifier: BUSL-1.1
//! The read path: point lookups ([`Engine::get`]) and scans
//! ([`Engine::scan_prefix`]/[`Engine::scan_range`]/[`Engine::scan_range_page`]).
//! Every SST consulted resolves through its own bloom-filter -> block-index
//! -> block-read path — never a full SST read for a point lookup, never a
//! full-file decode for a scan.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use crate::error::Result;
use crate::key::Key;
use crate::store::Value;

use super::{Engine, ScanPage};

impl Engine {
    /// Point lookup: memtable first, then SSTs newest to oldest — the first
    /// hit (value or tombstone) wins. Each SST consulted resolves through
    /// its own bloom-filter -> block-index -> single-block-read path — never
    /// a full SST read. Feeds the `point_lookup_full_sst_read` invariant
    /// counter surfaced by [`Self::stats`] (ADR-039 §4/§5.5).
    pub fn get(&self, key: &[u8]) -> Result<Option<Value>> {
        let key = Key::from(key);
        if let Some(hit) = self.memtable.get(&key) {
            return Ok(hit.cloned());
        }
        for h in self.current.ssts().iter().rev() {
            let (hit, blocks_read) = h.file.get(&key, &self.block_cache)?;
            if blocks_read > 1 {
                self.point_lookup_full_sst_read.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(value) = hit {
                return Ok(value);
            }
        }
        Ok(None)
    }

    /// Range scan: every live key starting with `prefix`, with its current
    /// value, in ascending key order. Tombstoned keys are omitted.
    ///
    /// Same layering rule as [`Engine::get`], expressed as a merge: SSTs
    /// oldest to newest, then the memtable, later layers overwriting earlier
    /// ones — the newest state per key wins, then tombstones are dropped.
    ///
    /// Materializes the matching set in memory (no streaming iterator yet) —
    /// fine for its current caller, the vector-index rebuild path
    /// (`idx::vector::persistent`), which needs every node block anyway;
    /// a streaming scan is deliberately deferred until something needs it.
    ///
    /// Per SST, only the data blocks overlapping the prefix range are
    /// decoded, via binary search on the block index
    /// ([`crate::store::sst_block::BlockSstFile::entries_with_prefix`]) —
    /// never a full-file decode.
    pub fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Key, Value)>> {
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        for h in self.current.ssts() {
            let (matches, _blocks_read) = h.file.entries_with_prefix(prefix)?;
            for (k, v) in matches {
                merged.insert(k, v);
            }
        }
        for (k, v) in self.memtable.iter() {
            if k.as_bytes().starts_with(prefix) {
                merged.insert(k.clone(), v.clone());
            }
        }
        Ok(merged
            .into_iter()
            .filter_map(|(k, v)| v.map(|value| (k, value)))
            .collect())
    }

    /// Every live entry with a key in `[start, end)` — the genuine
    /// range-query counterpart to [`Self::scan_prefix`] (ADR-041 §7.2):
    /// unlike a prefix scan, `end` bounds the query on both sides, so SST
    /// blocks entirely below `start` or at/past `end` are skipped without
    /// being decoded ([`crate::store::sst_block::BlockSstFile::entries_with_range`]),
    /// not just filtered after a full read. `start >= end` is an empty
    /// range, not an error.
    pub fn scan_range(&self, start: &[u8], end: &[u8]) -> Result<Vec<(Key, Value)>> {
        if start >= end {
            return Ok(Vec::new());
        }
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        for h in self.current.ssts() {
            let (matches, _blocks_read) = h.file.entries_with_range(start, end)?;
            for (k, v) in matches {
                merged.insert(k, v);
            }
        }
        for (k, v) in self.memtable.iter() {
            if k.as_bytes() >= start && k.as_bytes() < end {
                merged.insert(k.clone(), v.clone());
            }
        }
        Ok(merged
            .into_iter()
            .filter_map(|(k, v)| v.map(|value| (k, value)))
            .collect())
    }

    /// One bounded page of [`Self::scan_range`] (ADR-041 §7.3): at most
    /// ~`limit` live entries from `[start, end)`, in ascending key order,
    /// with memory bounded by `O(sources × limit)` instead of the full
    /// matching set — the primitive a paged full-population scan needs
    /// (`scan_range` materializes everything, which is exactly what a
    /// bounded-memory maintenance pass must avoid).
    ///
    /// Paging protocol: re-invoke with `start = next_start` until
    /// `next_start` is `None`. **An empty `entries` with a `Some(next_start)`
    /// means progress, not exhaustion** — a stretch of keys whose newest
    /// layer is a tombstone yields no live entries yet still advances the
    /// cursor. Loop on `next_start`, never on `entries.is_empty()`.
    ///
    /// How the bound stays correct under LSM layering: each source (every
    /// SST, plus the memtable) is read up to at most `limit` in-range
    /// entries. A source that got truncated is only complete up to its last
    /// returned key, so the page's *frontier* is the smallest such key
    /// across truncated sources — every key `<= frontier` has been seen by
    /// every source (each one returned all its keys at least that far), so
    /// last-write-wins merging is definitive there. Merged keys past the
    /// frontier are discarded (a not-yet-read older layer can't change them,
    /// but a not-yet-read *newer* one could) and re-read by the next page.
    pub fn scan_range_page(&self, start: &[u8], end: &[u8], limit: usize) -> Result<ScanPage> {
        if start >= end || limit == 0 {
            return Ok(ScanPage {
                entries: Vec::new(),
                next_start: None,
            });
        }
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        // Frontier = min over truncated sources of "the last key that source
        // returned". `None` until some source truncates.
        let mut frontier: Option<Vec<u8>> = None;
        let clip = |candidate: Option<Vec<u8>>, current: Option<Vec<u8>>| match (candidate, current) {
            (Some(c), Some(f)) => Some(c.min(f)),
            (Some(c), None) => Some(c),
            (None, f) => f,
        };
        for h in self.current.ssts() {
            let (matches, truncated, _blocks_read) = h.file.entries_with_range_limited(start, end, limit)?;
            if truncated {
                let last = matches.last().map(|(k, _)| k.as_bytes().to_vec());
                frontier = clip(last, frontier);
            }
            for (k, v) in matches {
                merged.insert(k, v);
            }
        }
        let mut taken = 0usize;
        let mut last_taken: Option<Vec<u8>> = None;
        for (k, v) in self.memtable.iter() {
            if k.as_bytes() < start || k.as_bytes() >= end {
                continue;
            }
            if taken == limit {
                // The memtable is complete only up to the last key actually
                // taken — the key we stopped at may shadow (overwrite or
                // tombstone) a same-key entry an older SST already merged,
                // so it must fall past the frontier and into the next page.
                frontier = clip(last_taken.take(), frontier);
                break;
            }
            last_taken = Some(k.as_bytes().to_vec());
            merged.insert(k.clone(), v.clone());
            taken += 1;
        }
        match frontier {
            None => Ok(ScanPage {
                entries: merged
                    .into_iter()
                    .filter_map(|(k, v)| v.map(|value| (k, value)))
                    .collect(),
                next_start: None,
            }),
            Some(f) => {
                let entries = merged
                    .into_iter()
                    .take_while(|(k, _)| k.as_bytes() <= f.as_slice())
                    .filter_map(|(k, v)| v.map(|value| (k, value)))
                    .collect();
                let mut next = f;
                next.push(0x00);
                Ok(ScanPage {
                    entries,
                    next_start: Some(next),
                })
            }
        }
    }
}
