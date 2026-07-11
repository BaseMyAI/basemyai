// SPDX-License-Identifier: BUSL-1.1
//! Bounded, byte-capacity LRU cache of decoded SST data blocks (N8.7,
//! ADR-039 §5.6). Sits between [`super::sst_block::BlockSstFile::get`] and
//! disk: a hit skips the `pread` + decode + (if encrypted) unseal entirely.
//!
//! Keyed by `(sst_id, block_no)` and shared across every SST an [`Engine`]
//! (`crate::store::Engine`) holds — one engine-wide byte budget, not a
//! per-SST allowance, per ADR-039 §5.6.
//!
//! v1 policy is a plain LRU (ADR-039 §5.6: "CLOCK/SLRU seulement si LRU
//! montre un défaut mesuré — pas de sophistication spéculative"), tracked
//! with a monotonic per-entry `last_used` tick and an O(n) eviction scan —
//! deliberately simple: at 32 MiB / ~16 KiB blocks that's on the order of
//! a couple thousand entries at most, and the alternative (an intrusive
//! doubly-linked-list LRU) buys O(1) eviction at a real correctness-risk
//! cost this milestone's brief doesn't ask for.
//!
//! **No lock is ever held across I/O.** [`BlockCache::get`] and
//! [`BlockCache::insert`] each take the internal [`Mutex`] only for the
//! in-memory map operation; every disk read/decrypt/decode happens in the
//! caller (`BlockSstFile::get`), entirely outside this cache's lock. A
//! concurrent miss on the same block from two threads is possible (both
//! read/decode/insert independently) — accepted, not engineered around,
//! per the same ADR guidance.
//!
//! **Threat model**: the cache holds *decoded, decrypted* block entries in
//! process RAM for as long as they stay cache-resident (not just the
//! duration of one read) — see `docs/security/encryption-model.md`'s
//! "Modèle de menace du cache/RAM" note (ADR-030's posture: the covered
//! threat is disk-at-rest, not process RAM).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::key::Key;
use crate::store::Value;

type CachedBlock = Vec<(Key, Option<Value>)>;

/// Rough resident-bytes estimate for one cached block: key + value bytes,
/// same accounting granularity the writer's block-boundary sizing uses
/// (`store::sst_block::entry_wire_size`) — good enough for a capacity
/// budget, not meant to match the process's actual heap accounting exactly.
fn block_bytes(entries: &CachedBlock) -> usize {
    entries
        .iter()
        .map(|(k, v)| k.as_bytes().len() + v.as_ref().map_or(0, Vec::len))
        .sum()
}

struct Entry {
    block: CachedBlock,
    bytes: usize,
    last_used: u64,
}

struct Inner {
    entries: HashMap<(u64, u32), Entry>,
    resident_bytes: usize,
    tick: u64,
    hits: u64,
    misses: u64,
}

pub(crate) struct BlockCache {
    inner: Mutex<Inner>,
    capacity_bytes: usize,
}

impl BlockCache {
    pub(crate) fn new(capacity_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
                resident_bytes: 0,
                tick: 0,
                hits: 0,
                misses: 0,
            }),
            capacity_bytes,
        }
    }

    /// Looks up `(sst_id, block_no)`. A clone of the cached block on hit
    /// (cheap: `Key`/`Value` are small, and this is strictly cheaper than
    /// the disk read + decode + decrypt it replaces); `None` on miss —
    /// caller reads the block from disk and calls [`Self::insert`].
    pub(crate) fn get(&self, sst_id: u64, block_no: u32) -> Option<CachedBlock> {
        let Ok(mut inner) = self.inner.lock() else {
            return None;
        };
        inner.tick += 1;
        let tick = inner.tick;
        // The `.map()` closure's borrow of `inner.entries` (via `entry`)
        // ends when it returns — `hit` is a fully owned `CachedBlock`, so
        // touching `inner.hits`/`inner.misses` right after does not
        // conflict with it (unlike holding `entry` alive across those
        // statements, which the borrow checker rejects through a
        // `MutexGuard`'s `DerefMut`).
        let hit = inner.entries.get_mut(&(sst_id, block_no)).map(|entry| {
            entry.last_used = tick;
            entry.block.clone()
        });
        if hit.is_some() {
            inner.hits += 1;
        } else {
            inner.misses += 1;
        }
        hit
    }

    /// Inserts a freshly-read block, evicting least-recently-used entries
    /// (oldest `last_used` tick first) until the resident-bytes budget is
    /// satisfied. A single block larger than the whole budget is still
    /// inserted (the budget is a target for steady-state residency, not a
    /// hard per-insert reject — refusing to cache it would only turn every
    /// lookup of that one oversized block into a guaranteed miss forever).
    pub(crate) fn insert(&self, sst_id: u64, block_no: u32, block: CachedBlock) {
        let bytes = block_bytes(&block);
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.entries.contains_key(&(sst_id, block_no)) {
            return; // Already resident (a concurrent miss raced us here) — no-op, not a re-insert.
        }
        inner.tick += 1;
        let tick = inner.tick;
        while inner.resident_bytes + bytes > self.capacity_bytes {
            let Some(&victim) = inner.entries.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| k) else {
                break; // Cache is already empty; the new block alone exceeds capacity — insert anyway.
            };
            if let Some(evicted) = inner.entries.remove(&victim) {
                inner.resident_bytes -= evicted.bytes;
            }
        }
        inner.resident_bytes += bytes;
        inner.entries.insert(
            (sst_id, block_no),
            Entry {
                block,
                bytes,
                last_used: tick,
            },
        );
    }

    /// Drops every entry belonging to `sst_id` — called when that SST is
    /// deleted (compaction folds it into a new SST and removes the old
    /// file), so a stale block never survives past the file it came from.
    pub(crate) fn invalidate_sst(&self, sst_id: u64) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        // Split into disjoint field borrows first — `entries.retain`'s
        // closure needs to touch `resident_bytes` too, and doing that
        // through the `MutexGuard`'s autoderef inline (`inner.entries.retain
        // (|.., e| { inner.resident_bytes -= ... })`) does not borrow-check.
        let Inner {
            entries,
            resident_bytes,
            ..
        } = &mut *inner;
        entries.retain(|&(id, _), entry| {
            if id == sst_id {
                *resident_bytes -= entry.bytes;
                false
            } else {
                true
            }
        });
    }

    pub(crate) fn hits(&self) -> u64 {
        self.inner.lock().map(|i| i.hits).unwrap_or(0)
    }

    pub(crate) fn misses(&self) -> u64 {
        self.inner.lock().map(|i| i.misses).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(n: usize, val_len: usize) -> CachedBlock {
        (0..n)
            .map(|i| (Key::from(format!("k{i}").as_bytes()), Some(vec![b'v'; val_len])))
            .collect()
    }

    #[test]
    fn miss_then_insert_then_hit() {
        let cache = BlockCache::new(1024 * 1024);
        assert!(cache.get(0, 0).is_none());
        assert_eq!(cache.misses(), 1);
        cache.insert(0, 0, block(5, 10));
        let hit = cache.get(0, 0).expect("hit after insert");
        assert_eq!(hit, block(5, 10));
        assert_eq!(cache.hits(), 1);
    }

    #[test]
    fn distinct_keys_do_not_collide() {
        let cache = BlockCache::new(1024 * 1024);
        cache.insert(0, 0, block(1, 10));
        cache.insert(0, 1, block(2, 10));
        cache.insert(1, 0, block(3, 10));
        assert_eq!(cache.get(0, 0).expect("hit").len(), 1);
        assert_eq!(cache.get(0, 1).expect("hit").len(), 2);
        assert_eq!(cache.get(1, 0).expect("hit").len(), 3);
    }

    #[test]
    fn eviction_stays_within_capacity_and_drops_least_recently_used() {
        // `block(10, 10)` is exactly 120 bytes: 10 entries, each key "k0"..
        // "k9" (2 bytes) + a 10-byte value = 12 bytes/entry. Capacity 250
        // fits exactly two such blocks (240 bytes) but not three (360).
        let cache = BlockCache::new(250);
        cache.insert(0, 0, block(10, 10)); // resident: 120
        cache.insert(0, 1, block(10, 10)); // resident: 240 — still within budget
        // Touch block 0 so block 1 becomes the least-recently-used.
        assert!(cache.get(0, 0).is_some());
        // Inserting a third block would push resident bytes to 360 > 250 —
        // the least-recently-used entry (block 1, untouched) must be
        // evicted to make room, never the just-touched block 0.
        cache.insert(0, 2, block(10, 10));
        assert!(
            cache.get(0, 0).is_some(),
            "recently-touched block must survive eviction"
        );
        assert!(
            cache.get(0, 1).is_none(),
            "untouched least-recently-used block must be evicted"
        );
        assert!(cache.get(0, 2).is_some(), "just-inserted block must be resident");
    }

    #[test]
    fn invalidate_sst_drops_only_that_sst() {
        let cache = BlockCache::new(1024 * 1024);
        cache.insert(0, 0, block(1, 10));
        cache.insert(1, 0, block(1, 10));
        cache.invalidate_sst(0);
        assert!(cache.get(0, 0).is_none());
        assert!(cache.get(1, 0).is_some());
    }

    #[test]
    fn oversized_single_block_is_still_cached() {
        let cache = BlockCache::new(10);
        cache.insert(0, 0, block(50, 50));
        assert!(
            cache.get(0, 0).is_some(),
            "an oversized block should still be cached, not silently dropped"
        );
    }
}
