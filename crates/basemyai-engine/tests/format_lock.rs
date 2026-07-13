//! CI gate: the committed `crates/basemyai-engine/format.lock` must match the
//! wire-format specs currently defined in
//! `src/format/{wal,sst_block,crypto,store_meta}.rs`.
//!
//! This is *the* anti-drift check described by
//! `docs/adr/ADR-025-native-engine-storage-foundation.md` and
//! `docs/PLAN-NATIVE-ENGINE.md` §3.1/§4 (modeled on SurrealDB's
//! `revision.lock`): a home-grown storage engine has no inherited decades of
//! format hardening, so any change to a persisted type's on-disk layout must
//! be a deliberate, reviewed act (bump `*_VERSION`, update the byte-layout
//! doc comment, update `spec()`, update `format.lock`) — never a silent
//! side effect of an innocent refactor.
//!
//! Invoked directly via `cargo test -p basemyai-engine --test format_lock`,
//! and wired into `cargo xtask check`/`cargo xtask ci` (see `xtask/src/main.rs`).

use std::path::PathBuf;

use basemyai_engine::format::lock::verify_file;

fn lock_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is this crate's root (crates/basemyai-engine), where
    // format.lock lives alongside Cargo.toml — the same place Cargo.lock
    // lives for the whole workspace, i.e. "next to the manifest it locks".
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("format.lock")
}

#[test]
fn format_lock_matches_current_wire_formats() {
    let path = lock_path();
    let mismatches = verify_file(&path).unwrap_or_else(|e| {
        panic!(
            "could not read format.lock at {}: {e}\n\
             This file must exist and be committed — it is the anti-drift guard \
             for basemyai-engine's on-disk formats (see docs/adr/ADR-025-native-engine-storage-foundation.md).",
            path.display()
        )
    });

    assert!(
        mismatches.is_empty(),
        "\n\nformat.lock is out of sync with the current wire-format specs:\n\n{}\n\n\
         If this drift is deliberate (you changed a wire format on purpose), bump the \
         relevant `*_VERSION` constant, update the byte-layout doc comment AND the \
         `spec()` function in src/format/{{wal,sst_block,crypto,store_meta}}.rs together, \
         then update format.lock to match. If it is NOT deliberate, revert your change.\n",
        mismatches
            .iter()
            .map(|m| format!("- {m}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
