// SPDX-License-Identifier: BUSL-1.1
//! Proves the fix for ENG-DUR-004 (P1,
//! `docs/audits/2026-07-engine-architecture-safety-audit.md`
//! §"ENG-DUR-004 — Fenêtre de rotation complète : la perte du pointeur de
//! génération détruit la génération courante à l'ouverture suivante").
//!
//! Formerly `generation_pointer_loss_causes_silent_total_data_loss_on_reopen`
//! (`generation_pointer_loss_destroys_active_generation.rs`), which pinned
//! the bug: `resolve_active_generation` (`store/engine.rs`) used to treat a
//! missing `generation.meta` as an unconditional "logical generation zero,
//! root directory" without checking whether a `gen-N` directory sat right
//! next to it — `open_inner` would then stamp a brand-new, empty
//! `crypto.meta` in the root, and the unconditional post-open
//! `gc_inactive_generations` would delete the real `gen-N`, silently
//! destroying every record the store held.
//!
//! `resolve_active_generation` now checks `any_generation_dir_present`
//! before accepting "no pointer" as generation 0 (I10,
//! `docs/architecture/ENGINE-TARGET-ARCHITECTURE.md` §3): a missing pointer
//! next to a live `gen-N` is refused as a typed
//! [`EngineError::CorruptGenerationMeta`] instead. This test proves the fix
//! end to end rather than by code reading — do not delete it going forward,
//! it is the regression test for I10.

use std::path::Path;

use basemyai_engine::format::generation_meta;
use basemyai_engine::{Engine, EngineError, EngineOptions};

const KEY_A: &[u8] = b"generation pointer loss key A";
const KEY_B: &[u8] = b"generation pointer loss key B";

/// Forces a flush to a real SST quickly, so the seeded record lives in a
/// sealed, on-disk data file inside `gen-1/` — not only in the in-memory
/// memtable/WAL tail — before the pointer is dropped.
fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 100,
        block_size: 256,
        ..EngineOptions::default()
    }
}

fn generation_meta_path(root: &Path) -> std::path::PathBuf {
    root.join(generation_meta::GENERATION_META_FILENAME)
}

/// Reproduces ENG-DUR-004's exact scenario and asserts the fixed behavior:
/// a lost generation pointer next to an otherwise intact `gen-N` directory
/// is refused with a typed error, and the real `gen-N` directory — and
/// every record inside it — survives untouched.
///
/// Steps, matching the audit's exact scenario:
/// 1. Open an encrypted store under key A, `put` one recognizable record,
///    and `close()` it so the record is flushed into a sealed SST inside
///    what is, at that point, the root/generation-0 directory.
/// 2. `rotate_key_full` to key B. This builds `gen-1/` (fresh `crypto.meta`,
///    the merged data re-sealed under the new DEK, an empty fsynced WAL),
///    publishes `generation.meta` pointing at generation 1, and — as part
///    of the very same call — sweeps the old generation-0 artifacts
///    (`wal.log`, `crypto.meta`, `*.sst`) out of the root directory, since
///    the old generation *was* the root. Confirmed on disk: `gen-1/` exists.
/// 3. Drop the engine (clean shutdown, not a crash — the bug did not need
///    an unclean shutdown to reproduce, and the fix must not need one
///    either to trigger).
/// 4. Delete `generation.meta` from the root, simulating the pointer loss.
/// 5. Reopen with key B (the current, correct key) and observe the refusal.
#[test]
fn generation_pointer_loss_is_rejected_and_gen1_survives() {
    let root = tempfile::tempdir().expect("tempdir");

    // Step 1: seed one recognizable, durably flushed record under key A.
    {
        let mut engine =
            Engine::open_encrypted_with_options(root.path(), KEY_A, small_options()).expect("open with key A");
        engine
            .put(b"canary", b"this record must survive rotation and pointer loss")
            .expect("put canary record");
        engine.close().expect("close flushes the canary into a sealed SST");
    }

    // Step 2: full rotation to key B publishes gen-1/ and generation.meta.
    {
        let mut engine =
            Engine::open_encrypted_with_options(root.path(), KEY_A, small_options()).expect("reopen with key A");
        engine.rotate_key_full(KEY_B).expect("full rotation to key B");
        // `rotate_key_full` leaves the instance already on the new
        // generation; drop it (clean shutdown) rather than reusing it, to
        // match the "reopen from scratch" shape of the real crash scenario.
    }
    let gen1_dir = root.path().join("gen-1");
    assert!(
        gen1_dir.is_dir(),
        "full rotation must have published gen-1/ holding the re-sealed data"
    );
    assert!(
        generation_meta_path(root.path()).exists(),
        "full rotation must have published generation.meta pointing at generation 1"
    );

    // Step 4: simulate the lost pointer. gen-1/ itself is untouched.
    std::fs::remove_file(generation_meta_path(root.path())).expect("simulate pointer loss");

    // Step 5: reopen with the current (post-rotation) key B — must be
    // refused, not silently accepted against a phantom empty generation 0.
    let reopen_result = Engine::open_encrypted_with_options(root.path(), KEY_B, small_options());
    match reopen_result {
        Err(EngineError::CorruptGenerationMeta { .. }) => {}
        Err(other) => panic!("expected CorruptGenerationMeta, got a different typed error: {other}"),
        Ok(_) => panic!(
            "I10 violation: reopen succeeded against a phantom fresh generation-0 store instead \
             of refusing typed"
        ),
    }

    // The refusal must be non-destructive: gen-1/ — and every real record
    // inside it — must still be exactly as it was before this reopen
    // attempt, unlike the old behavior where the post-open GC deleted it.
    assert!(
        gen1_dir.is_dir(),
        "gen-1/ must survive a refused open — the fix must never GC on this error path"
    );

    // Prove the data is genuinely recoverable, not merely physically present:
    // restore the pointer (the real-world recovery step after diagnosing a
    // lost generation.meta) and reopen through the normal root path. `gen-N`
    // directories have no `store.meta` of their own — they are only ever
    // meant to be reached via the root pointer, not opened standalone — so
    // this is the correct way to verify survival, not an incidental detail.
    // Test-only plain write (not the crash-safe tmp+fsync+rename the engine
    // itself uses to publish it) — good enough to restore the pointer here.
    std::fs::write(
        generation_meta_path(root.path()),
        generation_meta::encode(&generation_meta::GenerationMeta { current_generation: 1 }),
    )
    .expect("restore the lost pointer");
    let recovered = Engine::open_encrypted_with_options(root.path(), KEY_B, small_options())
        .expect("reopen after restoring the pointer must succeed cleanly");
    assert_eq!(
        recovered.get(b"canary").expect("get must not error"),
        Some(b"this record must survive rotation and pointer loss".to_vec()),
        "the canary record must still be intact once the pointer is restored"
    );
}
