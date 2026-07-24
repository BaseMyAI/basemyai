// SPDX-License-Identifier: BUSL-1.1
//! Directory-fsync helper (ENG-DUR-003,
//! `docs/audits/2026-07-engine-architecture-safety-audit.md`).
//!
//! Every publication site in this crate (SST, `store.meta`,
//! `generation.meta`, `crypto.meta`) writes a tmp file, `sync_all`s it, then
//! `rename`s it into place — but POSIX gives no ordering guarantee between a
//! `rename`'s directory-entry mutation and any other file's own metadata
//! mutation (e.g. the WAL truncation that follows an SST publish) without an
//! explicit `fsync` of the containing directory. This is the exact trap
//! documented by the ALICE study (Pillai et al., OSDI'14,
//! "Crash-Consistency: All File Systems Are Not Created Equal") and the
//! reason LevelDB/RocksDB fsync the directory after renaming MANIFEST/CURRENT.
//!
//! [`sync_dir`] must be called after every publication rename, before any
//! operation whose safety depends on that rename having survived a crash
//! (WAL truncation, generation GC, old-SST removal...).

use std::path::Path;

#[cfg(unix)]
use crate::error::EngineError;
use crate::error::Result;

/// Fsyncs `dir`'s own directory-entry metadata. No-op on non-Unix platforms:
/// there is no directory-handle equivalent to fsync on Windows, and every
/// call site still calls this unconditionally rather than special-casing the
/// platform itself — it degrades safely to nothing where it isn't meaningful.
#[cfg(unix)]
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|e| EngineError::io(dir.to_path_buf(), e))
}

#[cfg(not(unix))]
pub(crate) fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}
