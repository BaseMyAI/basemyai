// SPDX-License-Identifier: BUSL-1.1
//! GC-RETRY-P2 (BaseMyAI adversarial audit, 2026-07-22): `gc_old_generation`
//! used to make exactly one attempt to remove the old generation directory
//! after a full key/passphrase rotation, silently discarding the error
//! (`let _ = fs::remove_dir_all(old_dir)`) — unlike the per-SST removal path
//! (`store::version::remove_old_sst_with_retries`, ENG-DUR-002), which
//! already retries and counts. It now retries the same way and counts
//! persistent failures via
//! [`basemyai_engine::EngineStats::generation_remove_failures`].
//!
//! Same failpoint/lock idiom as `tests/compaction_remove_retry.rs`.

use std::sync::{Mutex, MutexGuard, OnceLock};

use basemyai_engine::Engine;
use basemyai_engine::failpoint::{self, Action};
use basemyai_engine::harness::CRYPTO_KEY;

fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

struct ClearOnDrop;
impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        failpoint::clear_all();
    }
}

/// A persistently failing removal of the old generation directory (every
/// attempt forced to fail — simulating a held file handle, the Windows
/// antivirus/indexer/backup scenario this mirrors from ENG-DUR-002) must be
/// retried, then counted — never silently discarded. The rotation itself
/// must still succeed and the new generation must be fully usable: the old
/// generation being un-removable is a disk-space/confidentiality-window
/// concern, never a correctness one — old generations are never considered
/// active regardless of whether their physical removal ever succeeds.
#[test]
fn persistent_generation_remove_failure_is_retried_then_counted_and_rotation_still_succeeds() {
    let _serial = lock();
    let _clear = ClearOnDrop;
    failpoint::clear_all();

    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_encrypted(dir.path(), CRYPTO_KEY).expect("open encrypted");
    engine.put(b"key", b"value").expect("seed a record before rotating");
    engine
        .flush()
        .expect("flush so the old generation has real content to (fail to) remove");

    failpoint::set("during_generation_gc_removal", Action::Error);
    engine
        .rotate_key_full(ROTATED_KEY)
        .expect("rotate_key_full must still succeed despite every old-generation removal attempt failing");
    failpoint::clear_all();

    let stats = engine.stats().expect("stats");
    assert!(
        stats.generation_remove_failures >= 1,
        "the old generation directory failed every removal retry and must be counted, not silently discarded"
    );

    // The new generation is fully usable, unaffected by the old
    // generation's failed cleanup.
    assert_eq!(engine.get(b"key").expect("get"), Some(b"value".to_vec()));
    engine
        .put(b"after-rotation", b"still works")
        .expect("put after rotation");
    drop(engine);

    // Reopen must select the NEW generation and must never resurrect the
    // old, undeleted-but-inactive generation as live.
    let reopened = Engine::open_encrypted(dir.path(), ROTATED_KEY).expect("reopen with the rotated key");
    assert_eq!(reopened.get(b"key").expect("get"), Some(b"value".to_vec()));
    assert_eq!(
        reopened.get(b"after-rotation").expect("get"),
        Some(b"still works".to_vec())
    );
    // The old key must not open the store — proves the active generation
    // really is the new one, not a resurrected old one left behind by the
    // failed removal.
    drop(reopened);
    assert!(Engine::open_encrypted(dir.path(), CRYPTO_KEY).is_err());
}

const ROTATED_KEY: &[u8] = b"generation-gc-retry-rotated-key";
