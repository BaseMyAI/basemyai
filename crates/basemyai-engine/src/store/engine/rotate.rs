// SPDX-License-Identifier: BUSL-1.1
//! Key/passphrase rotation: in-place (O(1), re-wraps the DEK, ADR-030 §4)
//! and full (O(store size), a fresh generation under a fresh DEK, ADR-042).
//! `rotate_full` is the most tentacular function in the engine — it
//! reimplements a generation-creation bootstrap, a full compaction-style
//! merge, and manifest/WAL publication all in one atomic operation — but it
//! stays a single function rather than being split further: it is
//! intrinsically one all-or-nothing "new generation" unit, not a sequence
//! of independently reusable phases.

use std::fs::{self, OpenOptions};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::crypto::{self, CryptoContext};
use crate::error::{EngineError, Result};
use crate::fail_point;
use crate::format::generation_meta;
use crate::key::Key;
use crate::store::Value;
use crate::store::sst_block::BlockSstFile;
use crate::store::version::{SstHandle, Version};
use crate::store::wal::Wal;

use super::Engine;
use super::io::{gc_old_generation, generation_dir, publish_generation, publish_sst_manifest};

impl Engine {
    /// Rotates the user key **in place** (ADR-030 §4): the store's DEK is
    /// re-wrapped under a KEK derived from `new_key` (fresh salt) and
    /// `crypto.meta` is atomically replaced (tmp + fsync + rename). O(1) —
    /// no data file is rewritten — and crash-safe by construction: after a
    /// crash, `crypto.meta` is either the old wrap (old key opens) or the
    /// new one (new key opens), never a mixed state.
    ///
    /// Unlike libSQL's `Store::rotate_key`, **this instance stays fully
    /// usable after the call** (the DEK itself never changes). The assumed,
    /// documented deviation: an attacker holding the old key *and* a copy
    /// of the old `crypto.meta` can still unwrap the DEK — see ADR-030 §4
    /// for the threat-model discussion and the deferred full-re-encryption
    /// follow-up.
    ///
    /// # Errors
    /// [`EngineError::NotEncrypted`] if this store was opened without
    /// encryption (nothing to rotate — parity with ADR-007's posture);
    /// otherwise I/O errors from the atomic replace.
    pub fn rotate_key(&mut self, new_key: &[u8]) -> Result<()> {
        self.rotate_key_with_mode(new_key, crypto::KeyMode::RawKey, crypto::Argon2idProfile::Default)
    }

    /// Passphrase counterpart to [`Self::rotate_key`]. The existing DEK is
    /// re-wrapped under an Argon2id-derived KEK without rewriting data files.
    pub fn rotate_passphrase(&mut self, new_passphrase: &[u8]) -> Result<()> {
        self.rotate_passphrase_with_profile(new_passphrase, crypto::Argon2idProfile::Default)
    }

    /// Re-wraps the existing DEK with a passphrase under an explicit Argon2id
    /// profile. The profile must be repeated at each rotation that should keep
    /// using the constrained-hardware parameters (ADR-042).
    pub fn rotate_passphrase_with_profile(
        &mut self,
        new_passphrase: &[u8],
        profile: crypto::Argon2idProfile,
    ) -> Result<()> {
        self.rotate_key_with_mode(new_passphrase, crypto::KeyMode::Passphrase, profile)
    }

    fn rotate_key_with_mode(
        &mut self,
        new_key: &[u8],
        mode: crypto::KeyMode,
        profile: crypto::Argon2idProfile,
    ) -> Result<()> {
        let Some(crypto) = &self.crypto else {
            return Err(EngineError::NotEncrypted { path: self.dir.clone() });
        };
        crypto::write_meta_with_mode_for_generation_and_profile(
            &self.dir,
            new_key,
            crypto,
            mode,
            profile,
            self.generation_id,
        )
    }

    /// Re-encrypts every live record under a fresh DEK and atomically makes
    /// the resulting generation current (ADR-042). Unlike [`Self::rotate_key`],
    /// this is an O(store size) full merge and removes tombstones and shadowed
    /// records from the active generation.
    ///
    /// # Errors
    /// [`EngineError::NotEncrypted`] for a plaintext store, plus I/O or
    /// corruption errors encountered while reading or publishing the new
    /// generation. Before pointer publication an error leaves this instance
    /// and the active generation unchanged.
    pub fn rotate_key_full(&mut self, new_key: &[u8]) -> Result<()> {
        self.rotate_full(new_key, crypto::KeyMode::RawKey, crypto::Argon2idProfile::Default)
    }

    /// Passphrase counterpart to [`Self::rotate_key_full`]. The fresh DEK is
    /// wrapped by an Argon2id-derived KEK persisted in `CryptoMeta:2`.
    pub fn rotate_passphrase_full(&mut self, new_passphrase: &[u8]) -> Result<()> {
        self.rotate_passphrase_full_with_profile(new_passphrase, crypto::Argon2idProfile::Default)
    }

    /// Full-DEK counterpart to [`Self::rotate_passphrase_with_profile`].
    pub fn rotate_passphrase_full_with_profile(
        &mut self,
        new_passphrase: &[u8],
        profile: crypto::Argon2idProfile,
    ) -> Result<()> {
        self.rotate_full(new_passphrase, crypto::KeyMode::Passphrase, profile)
    }

    fn rotate_full(&mut self, new_key: &[u8], mode: crypto::KeyMode, profile: crypto::Argon2idProfile) -> Result<()> {
        if self.crypto.is_none() {
            return Err(EngineError::NotEncrypted { path: self.dir.clone() });
        }

        let next_generation = self
            .generation_id
            .checked_add(1)
            .ok_or_else(|| EngineError::CorruptGenerationMeta {
                path: self.root_dir.join(generation_meta::GENERATION_META_FILENAME),
                reason: "active generation id cannot be incremented".to_string(),
            })?;
        let next_dir = generation_dir(&self.root_dir, next_generation);

        // A pre-publication crash may leave this exact directory behind.
        // Never reuse its crypto.meta/DEK: remove it completely and create a
        // fresh generation from scratch.
        if next_dir.exists() {
            fs::remove_dir_all(&next_dir).map_err(|e| EngineError::io(next_dir.clone(), e))?;
        }
        fs::create_dir(&next_dir).map_err(|e| EngineError::io(next_dir.clone(), e))?;

        let build = (|| -> Result<(CryptoContext, Option<BlockSstFile>, Wal)> {
            let new_crypto =
                crypto::create_meta_for_generation_with_profile(&next_dir, new_key, mode, profile, next_generation)?;
            fail_point!("after_full_rotation_new_dek");

            // Same precedence as reads/compaction: old SSTs first, newest
            // layers overwrite them, and the WAL-replayed memtable wins last.
            // This directly folds the unflushed tail into the output, so no
            // intermediate old-DEK SST and no WAL re-sealing pass is needed.
            let mut merged: std::collections::BTreeMap<Key, Option<Value>> = std::collections::BTreeMap::new();
            for h in self.current.ssts() {
                for (key, value) in h.file.entries()? {
                    merged.insert(key, value);
                }
            }
            for (key, value) in self.memtable.iter() {
                merged.insert(key.clone(), value.clone());
            }
            let entries: Vec<(Key, Option<Value>)> = merged.into_iter().filter(|(_, value)| value.is_some()).collect();
            let new_sst = if entries.is_empty() {
                None
            } else {
                let sst = BlockSstFile::write_new(&next_dir, 0, entries, self.options.block_size, Some(&new_crypto))?;
                fail_point!("after_full_rotation_sst_write");
                Some(sst)
            };

            // ENG-DUR-001: `next_dir` gets its own manifest, listing its
            // (at most one) merged SST — part of the same all-or-nothing
            // build as the rest of this closure. If anything below fails,
            // the existing `remove_dir_all(&next_dir)` error path (below)
            // discards this manifest along with everything else half-built.
            publish_sst_manifest(&next_dir, 0, &new_sst.iter().map(|s| s.id).collect::<Vec<_>>())?;

            // The new generation is published only after even its empty WAL
            // exists durably. Keep this handle ready for the live-state swap.
            let wal_path = next_dir.join("wal.log");
            let wal_file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&wal_path)
                .map_err(|e| EngineError::io(wal_path.clone(), e))?;
            wal_file.sync_all().map_err(|e| EngineError::io(wal_path.clone(), e))?;
            drop(wal_file);
            let new_wal = Wal::open_for_append(wal_path, Some(new_crypto.clone()))?;
            fail_point!("before_full_rotation_publish");
            Ok((new_crypto, new_sst, new_wal))
        })();

        let (new_crypto, new_sst, new_wal) = match build {
            Ok(build) => build,
            Err(error) => {
                let _ = fs::remove_dir_all(&next_dir);
                return Err(error);
            }
        };

        publish_generation(&self.root_dir, next_generation)?;

        // From this point forward the in-memory writer must follow the
        // published pointer. Every operation below is infallible; notably,
        // the new WAL handle was opened before publication.
        let old_dir = std::mem::replace(&mut self.dir, next_dir);
        self.generation_id = next_generation;
        let old_wal = std::mem::replace(&mut self.wal, new_wal);
        // The retired `Wal` never counts its fsyncs anywhere else — fold
        // them into `Counters` now, or they vanish once `self.wal` (fresh,
        // starting from zero) replaces it as the source `Engine::stats`
        // reads from.
        self.counters.fsync_count += old_wal.fsync_count();
        drop(old_wal); // mandatory before remove_dir_all on Windows
        // `next_dir`'s manifest was already published inside `build` above
        // (generation 0, listing this same 0-or-1 merged SST) — the new
        // `Version` mirrors it so `Engine::stats`/future flushes agree with
        // what's on disk. The old generation's handles are deliberately
        // *not* retired: their whole directory is GC'd wholesale below
        // (`gc_old_generation`), never file by file — a `Snapshot` taken
        // before a full rotation does not survive it (typed I/O error on
        // its next read, per ADR-043 §2 amended).
        let new_version = Arc::new(Version::build(
            0,
            new_sst
                .into_iter()
                .map(|file| SstHandle::new(file, Arc::clone(&self.sst_remove_failures)))
                .collect(),
        ));
        let old_version = std::mem::replace(&mut self.current, new_version);
        let input_bytes = old_version.ssts().iter().map(|h| h.file.file_bytes).sum::<u64>();
        let output_bytes = self.current.ssts().iter().map(|h| h.file.file_bytes).sum::<u64>();
        for old in old_version.ssts() {
            self.block_cache.invalidate_sst(old.file.id);
        }
        drop(old_version);
        self.memtable.clear();
        // Fresh generation, fresh id space (ids from the retired generation
        // belong to a directory this instance no longer writes to) — safe to
        // reset regardless of what `next_sst_id` held before (INV-VS-8 is
        // scoped per generation, never across one).
        self.next_sst_id = Arc::new(AtomicU64::new(u64::from(!self.current.ssts().is_empty())));
        self.crypto = Some(new_crypto);
        self.counters.compaction_count += 1;
        self.counters.compaction_input_bytes += input_bytes;
        self.counters.compaction_output_bytes += output_bytes;
        self.counters.bytes_written += output_bytes;

        fail_point!("after_full_rotation_publish");
        gc_old_generation(
            &self.root_dir,
            &old_dir,
            next_generation,
            &self.generation_remove_failures,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::engine::test_support::{KEY, small_options};

    #[test]
    fn full_rotation_publishes_fresh_generation_and_keeps_live_engine_usable() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted_with_options(root.path(), KEY, small_options()).expect("open");
            engine.put(b"kept", b"current").expect("put current");
            engine.put(b"deleted", b"old").expect("put deleted");
            engine.flush().expect("flush old generation");
            engine.delete(b"deleted").expect("delete");
            fs::write(root.path().join("crypto.meta.tmp"), b"old wrap").expect("seed crypto tmp");
            fs::write(root.path().join("999.sst.tmp"), b"old ciphertext").expect("seed sst tmp");

            engine.rotate_key_full(b"fresh key").expect("full rotate");
            assert_eq!(engine.get(b"kept").expect("get kept").as_deref(), Some(&b"current"[..]));
            assert_eq!(engine.get(b"deleted").expect("get deleted"), None);
            engine.put(b"after", b"publish").expect("write after publish");
        }

        assert!(!root.path().join("crypto.meta.tmp").exists());
        assert!(!root.path().join("999.sst.tmp").exists());

        let Err(old) = Engine::open_encrypted(root.path(), KEY) else {
            panic!("old key must not open current generation");
        };
        assert!(matches!(old, EngineError::WrongEncryptionKey { .. }));
        let reopened = Engine::open_encrypted(root.path(), b"fresh key").expect("new key opens");
        assert_eq!(
            reopened.get(b"kept").expect("get kept").as_deref(),
            Some(&b"current"[..])
        );
        assert_eq!(reopened.get(b"deleted").expect("get deleted"), None);
        assert_eq!(
            reopened.get(b"after").expect("get after").as_deref(),
            Some(&b"publish"[..])
        );
    }

    /// ADR-042 §5 exit criterion: the old key **combined with a genuine copy
    /// of the pre-rotation `crypto.meta`** — exactly the gap ADR-030 §4
    /// documented as uncovered for `--full` — must not decrypt a single byte
    /// of the new generation, neither WAL nor SST. `generation_pointer_
    /// requires_matching_crypto_meta_generation` already proves the
    /// generation-id self-check in `crypto.meta` rejects this; this test
    /// goes one level lower and proves the AEAD itself (DEK binding, not
    /// just the generation-id field) rejects it, by attempting a raw
    /// `CryptoContext::open` against real ciphertext from the new
    /// generation using a context loaded from the *old* generation's
    /// `crypto.meta` while it still existed, pre-rotation.
    #[test]
    fn old_crypto_meta_copied_beside_a_new_generation_cannot_open_its_wal_or_sst() {
        let root = tempfile::tempdir().expect("tempdir");
        let old_ctx = {
            let mut engine = Engine::open_encrypted_with_options(root.path(), KEY, small_options()).expect("open");
            engine.put(b"kept", b"current").expect("put current");
            engine.flush().expect("seed a real sealed SST under generation 0");
            let ctx = crypto::load_meta(root.path(), KEY).expect("load pre-rotation context");
            engine.rotate_key_full(b"fresh key").expect("full rotate");
            engine
                .put(b"after", b"publish")
                .expect("write into the new generation's WAL");
            ctx
        };

        let pointer_bytes =
            fs::read(root.path().join(generation_meta::GENERATION_META_FILENAME)).expect("read generation pointer");
        let pointer = generation_meta::decode(
            &pointer_bytes,
            &root.path().join(generation_meta::GENERATION_META_FILENAME),
        )
        .expect("decode generation pointer");
        let new_gen_dir = generation_dir(root.path(), pointer.current_generation);

        let wal_path = new_gen_dir.join("wal.log");
        let wal_bytes = fs::read(&wal_path).expect("read new generation wal");
        let (nonce, ciphertext, _consumed) = crate::format::crypto::decode_wal_envelope(&wal_bytes, &wal_path)
            .expect("structurally decode the first wal envelope")
            .expect("new generation wal must hold at least one complete record");
        let wal_aad = crate::format::crypto::wal_envelope_aad();
        assert!(
            old_ctx.open(&nonce, ciphertext, &wal_aad).is_none(),
            "the pre-rotation key + crypto.meta must not decrypt the new generation's WAL"
        );
        // Positive control: the same extracted (nonce, ciphertext, aad)
        // genuinely decrypts under the *new* generation's real context —
        // proves the `None` above is the DEK mismatch this test targets,
        // not a byte-slicing mistake that would return `None` regardless.
        let new_ctx = crypto::load_meta_for_generation(
            &new_gen_dir,
            b"fresh key",
            crypto::KeyMode::RawKey,
            pointer.current_generation,
        )
        .expect("load the new generation's own context");
        assert!(
            new_ctx.open(&nonce, ciphertext, &wal_aad).is_some(),
            "sanity check failed: the new generation's own key must decrypt its own WAL"
        );

        let sst_path = fs::read_dir(&new_gen_dir)
            .expect("read new generation directory")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sst"))
            .expect("full rotation must have re-sealed at least one SST into the new generation");
        let sst_bytes = fs::read(&sst_path).expect("read new generation sst");
        let header_len = crate::format::sst_block::SST_HEADER_TOTAL_LEN;
        let header = crate::format::sst_block::decode_sst_header(&sst_bytes[..header_len], &sst_path)
            .expect("decode the plaintext sst header");
        // `decode_encrypted_sst_block` requires the exact envelope slice (no
        // torn-tail tolerance) — peek the block's own `ct_len` field first
        // so the slice passed in is neither short nor carries the next
        // section's bytes.
        let block_bytes = &sst_bytes[header_len..];
        let nonce_len = crate::format::crypto::NONCE_LEN;
        let envelope_header_len = 4 + 2 + nonce_len + 4;
        let ct_len = u32::from_le_bytes(
            block_bytes[4 + 2 + nonce_len..envelope_header_len]
                .try_into()
                .expect("4-byte ct_len field"),
        ) as usize;
        let (sst_nonce, sst_ciphertext) =
            crate::format::crypto::decode_encrypted_sst_block(&block_bytes[..envelope_header_len + ct_len], &sst_path)
                .expect("structurally decode the first sealed data block");
        // Block 0 of the Data section, right after the plaintext header —
        // `sst_id` comes from the file's own header so the AAD matches
        // exactly what the writer used, isolating the assertion to the DEK
        // mismatch rather than an incidental AAD mismatch.
        let sst_aad = crate::format::crypto::encrypted_sst_block_aad(
            header.sst_id,
            crate::format::crypto::SstSectionType::Data,
            0,
        );
        assert!(
            old_ctx.open(&sst_nonce, sst_ciphertext, &sst_aad).is_none(),
            "the pre-rotation key + crypto.meta must not decrypt the new generation's SST, \
             even with the correct AAD shape reconstructed"
        );
        assert!(
            new_ctx.open(&sst_nonce, sst_ciphertext, &sst_aad).is_some(),
            "sanity check failed: the new generation's own key must decrypt its own SST block"
        );
    }

    #[test]
    fn full_rotation_can_switch_to_passphrase_mode() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open raw-key store");
            engine.put(b"key", b"value").expect("put");
            engine
                .rotate_passphrase_full(b"new human passphrase")
                .expect("full rotate to passphrase");
        }

        let Err(raw) = Engine::open_encrypted(root.path(), b"new human passphrase") else {
            panic!("same bytes in raw-key mode must be refused");
        };
        assert!(matches!(raw, EngineError::WrongEncryptionKey { .. }));
        let reopened = Engine::open_with_passphrase(root.path(), b"new human passphrase").expect("passphrase opens");
        assert_eq!(reopened.get(b"key").expect("get").as_deref(), Some(&b"value"[..]));
    }

    #[test]
    fn in_place_rotation_can_switch_to_passphrase_mode() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open raw-key store");
            engine.put(b"key", b"value").expect("put");
            engine
                .rotate_passphrase(b"new human passphrase")
                .expect("rotate to passphrase");
            assert_eq!(engine.get(b"key").expect("get live").as_deref(), Some(&b"value"[..]));
        }

        let Err(raw) = Engine::open_encrypted(root.path(), b"new human passphrase") else {
            panic!("same bytes in raw-key mode must be refused");
        };
        assert!(matches!(raw, EngineError::WrongEncryptionKey { .. }));
        let reopened = Engine::open_with_passphrase(root.path(), b"new human passphrase").expect("passphrase opens");
        assert_eq!(reopened.get(b"key").expect("get").as_deref(), Some(&b"value"[..]));
    }

    #[test]
    fn consecutive_full_rotations_advance_and_gc_generations() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open");
            engine.put(b"before", b"one").expect("put before");
            engine.rotate_key_full(b"second key").expect("first full rotate");
            engine.put(b"between", b"two").expect("put between");
            engine.rotate_key_full(b"third key").expect("second full rotate");
        }

        assert!(!root.path().join("gen-1").exists(), "previous generation must be GC'd");
        assert!(root.path().join("gen-2").is_dir());
        let engine = Engine::open_encrypted(root.path(), b"third key").expect("open latest generation");
        assert_eq!(engine.get(b"before").expect("get before").as_deref(), Some(&b"one"[..]));
        assert_eq!(
            engine.get(b"between").expect("get between").as_deref(),
            Some(&b"two"[..])
        );
    }

    #[test]
    fn full_rotation_preserves_monotonic_block_cache_counters() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open_encrypted_with_options(root.path(), KEY, small_options()).expect("open");
        engine.put(b"key", b"value").expect("put");
        engine.flush().expect("flush");
        assert_eq!(engine.get(b"key").expect("miss").as_deref(), Some(&b"value"[..]));
        assert_eq!(engine.get(b"key").expect("hit").as_deref(), Some(&b"value"[..]));
        let before = engine.stats().expect("stats before");

        engine.rotate_key_full(b"fresh key").expect("full rotate");
        let after = engine.stats().expect("stats after");
        assert!(after.block_cache_hits >= before.block_cache_hits);
        assert!(after.block_cache_misses >= before.block_cache_misses);
    }

    #[test]
    fn rotate_key_switches_keys_without_reopen_and_preserves_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("open");
            for i in 0..10u32 {
                engine
                    .put(format!("k{i}").as_bytes(), format!("v{i}").as_bytes())
                    .expect("put");
            }
            engine.rotate_key(b"the new key").expect("rotate");
            // The instance stays fully usable after rotation (ADR-030 §4) —
            // unlike libSQL, no drop-and-reopen dance.
            engine
                .put(b"post-rotation", b"still writable")
                .expect("put after rotate");
            assert_eq!(engine.get(b"k5").expect("get").as_deref(), Some(&b"v5"[..]));
        }

        let Err(err) = Engine::open_encrypted(dir.path(), KEY) else {
            panic!("old key must no longer open")
        };
        assert!(matches!(err, EngineError::WrongEncryptionKey { .. }));

        let engine = Engine::open_encrypted_with_options(dir.path(), b"the new key", small_options()).expect("reopen");
        assert_eq!(engine.get(b"k0").expect("get").as_deref(), Some(&b"v0"[..]));
        assert_eq!(engine.get(b"k9").expect("get").as_deref(), Some(&b"v9"[..]));
        assert_eq!(
            engine.get(b"post-rotation").expect("get").as_deref(),
            Some(&b"still writable"[..])
        );
    }

    #[test]
    fn rotate_key_on_plaintext_store_is_not_encrypted_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open(dir.path()).expect("open plaintext");
        assert!(!engine.is_encrypted());
        let err = engine.rotate_key(b"whatever").expect_err("nothing to rotate");
        assert!(matches!(err, EngineError::NotEncrypted { .. }));
    }
}
