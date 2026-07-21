// SPDX-License-Identifier: BUSL-1.1
//! Failpoints (N7.4, `docs/PLAN-NATIVE-ENGINE.md` §4.4): controlled fault
//! injection at every durability boundary of the write path, compiled only
//! under `test-util`/`cfg(test)` — a release build without that feature
//! contains **zero** failpoint code (the [`fail_point!`] macro expands to
//! nothing).
//!
//! Sites (macro invocations in `store/wal.rs`, `store/sst.rs`,
//! `store/engine.rs`, `crypto.rs`):
//!
//! ```text
//! after_wal_append        after the WAL record's write_all, before fsync
//! after_wal_fsync         after the WAL record's sync_all
//! after_sst_tmp_write     after the SST tmp file's write_all, before fsync
//! after_sst_tmp_fsync     after the SST tmp file's sync_all, before rename
//! after_sst_rename        after the tmp → final rename
//! before_wal_truncate     in flush, after the SST is durable, before WAL reset
//! during_compaction       at the start of a compaction merge
//! during_compaction_sst_removal  in compact's cleanup loop, one simulated
//!                                 failed removal attempt per hit (retried;
//!                                 see tests/compaction_remove_retry.rs,
//!                                 ENG-DUR-002)
//! after_crypto_meta_write after crypto.meta's atomic replace (rotation)
//! after_full_rotation_new_dek after the next generation's fresh DEK wrap
//! after_full_rotation_sst_write after the merged SST is durable
//! before_full_rotation_publish after all content fsyncs, before pointer rename
//! after_full_rotation_publish after pointer rename, before old-generation GC
//! during_full_rotation_gc immediately before best-effort old-generation GC
//! before_manifest_publish before store.meta's tmp → final rename (N8.9,
//!                          ADR-039 §7 — the site the original plan
//!                          reserved this name for, before block-based SSTs
//!                          existed to name it after)
//! before_sst_manifest_publish before manifest.meta's tmp → final rename
//!                              (ENG-DUR-001, J2) — distinct file, distinct
//!                              site from before_manifest_publish above
//! ```
//!
//! Two configuration channels, same registry:
//! - programmatic, for in-process tests: [`set`] / [`remove`] / [`clear_all`];
//! - the `BASEMYAI_FAILPOINTS` env var, for child processes spawned by the
//!   crash harness: a comma-separated `name=action` list, e.g.
//!   `BASEMYAI_FAILPOINTS=after_sst_rename=abort,after_wal_fsync=error`.
//!   Read lazily on the first hit, then merged under programmatic changes.
//!
//! Actions: `error` returns an injected [`EngineError::Io`] from the
//! enclosing engine call (the caller sees a typed failure exactly at that
//! boundary); `abort` terminates the process without unwinding or running
//! destructors — the closest in-process stand-in for `kill -9` at an exact
//! instruction boundary, which the kill-loop harness can't time.
//!
//! The registry is **process-global**: tests that configure failpoints must
//! serialize among themselves (lock a shared test mutex) and `clear_all` on
//! exit, or they will poison unrelated tests in the same binary.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::error::{EngineError, Result};

/// What a triggered failpoint does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Return an injected [`EngineError::Io`] from the enclosing call.
    Error,
    /// `std::process::abort()` — simulates a hard kill at this exact boundary.
    Abort,
}

fn registry() -> &'static Mutex<HashMap<String, Action>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Action>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(parse_env()))
}

/// Initial registry contents from `BASEMYAI_FAILPOINTS`. Unknown action
/// names are a loud panic, not a silent skip — a harness with a typo'd
/// action would otherwise "pass" without testing anything.
fn parse_env() -> HashMap<String, Action> {
    let Ok(spec) = std::env::var("BASEMYAI_FAILPOINTS") else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Some((name, action)) = entry.split_once('=') else {
            panic!("BASEMYAI_FAILPOINTS: entry {entry:?} is not name=action");
        };
        let action = match action.trim() {
            "error" => Action::Error,
            "abort" => Action::Abort,
            other => panic!("BASEMYAI_FAILPOINTS: unknown action {other:?} (expected error|abort)"),
        };
        map.insert(name.trim().to_string(), action);
    }
    map
}

/// Arms `name` with `action` for this process.
pub fn set(name: &str, action: Action) {
    registry()
        .lock()
        .expect("failpoint registry lock poisoned")
        .insert(name.to_string(), action);
}

/// Disarms `name` (a no-op if it wasn't armed).
pub fn remove(name: &str) {
    registry()
        .lock()
        .expect("failpoint registry lock poisoned")
        .remove(name);
}

/// Disarms every failpoint (including env-configured ones).
pub fn clear_all() {
    registry().lock().expect("failpoint registry lock poisoned").clear();
}

/// Called by the [`fail_point!`](crate::fail_point) macro at each site.
/// Unarmed (the overwhelmingly common case in tests): one mutex lock and a
/// hash lookup.
///
/// # Errors
/// The injected [`EngineError::Io`] when the site is armed with
/// [`Action::Error`].
pub fn hit(name: &str) -> Result<()> {
    let action = {
        let map = registry().lock().expect("failpoint registry lock poisoned");
        map.get(name).copied()
    };
    match action {
        None => Ok(()),
        Some(Action::Error) => Err(EngineError::Io {
            path: std::path::PathBuf::from(format!("failpoint:{name}")),
            source: std::io::Error::other(format!("injected failpoint `{name}`")),
        }),
        Some(Action::Abort) => {
            // Deliberately no unwinding, no destructors, no buffered-write
            // flush: the point is to freeze the on-disk state exactly as it
            // is at this boundary.
            eprintln!("failpoint `{name}`: aborting process (injected)");
            std::process::abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serialized against other failpoint-using tests via the shared lock in
    // `tests/failpoints.rs`? No — these unit tests only exercise the
    // registry API on names no engine site uses, so they can't perturb
    // engine behavior even when run concurrently with them.

    #[test]
    fn unarmed_hit_is_ok() {
        assert!(hit("registry_test_never_armed").is_ok());
    }

    #[test]
    fn armed_error_then_removed() {
        set("registry_test_error", Action::Error);
        let err = hit("registry_test_error").expect_err("armed site must inject");
        assert!(matches!(err, EngineError::Io { .. }));
        remove("registry_test_error");
        assert!(hit("registry_test_error").is_ok());
    }
}
