// SPDX-License-Identifier: BUSL-1.1
//! Shared test fixtures for `engine`'s split submodules.

use super::EngineOptions;

pub(super) const KEY: &[u8] = b"test user key";

/// Options that force flush + compaction quickly, so tests exercise sealed
/// SST sections and compaction, not just the WAL. Small `block_size` too, so
/// these small stores still span more than one data block per SST.
pub(super) fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 4,
        compaction_sst_threshold: 2,
        block_size: 256,
        ..EngineOptions::default()
    }
}
