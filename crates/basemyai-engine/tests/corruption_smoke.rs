// SPDX-License-Identifier: BUSL-1.1
//! Corruption smoke test (N7 → gate à chaque PR, `PLAN-NATIVE-ENGINE.md`
//! §8.3) : pour chaque artefact on-disk (SST, WAL, `crypto.meta`), un octet
//! corrompu doit produire une **erreur typée** à l'ouverture — jamais un
//! panic, jamais des données silencieusement fausses. Clair et chiffré.
//!
//! Ce fichier documente aussi, en tant que test, un **gap connu et toujours
//! ouvert** : sans manifest, une SST supprimée du disque disparaît
//! silencieusement (l'ouverture réussit, les données sont juste absentes).
//! N9/ADR-040 a livré `verify`, mais confirmé empiriquement (N11.3) que
//! `verify` ne le détecte pas non plus — aucun mode n'a de source
//! indépendante listant les SSTs attendues. Le test est le marqueur
//! exécutable du gap, pas une promesse que N9 le ferme.

use std::path::{Path, PathBuf};

use basemyai_engine::{Engine, EngineError, EngineOptions};

const KEY: &[u8] = b"corruption smoke key";

fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 4,
        compaction_sst_threshold: 100, // jamais de compaction : les fichiers restent en place
        block_size: 256,
        ..EngineOptions::default()
    }
}

/// Construit un store avec au moins une SST durable et une queue WAL non
/// flushée, puis rend la main pour la corruption.
fn build_store(dir: &Path, encrypted: bool) {
    let mut engine = if encrypted {
        Engine::open_encrypted_with_options(dir, KEY, small_options()).expect("open encrypted")
    } else {
        Engine::open_with_options(dir, small_options()).expect("open clear")
    };
    for i in 0..4u32 {
        engine.put(format!("k{i}").as_bytes(), b"value").expect("put"); // 4e put → flush → SST
    }
    engine.put(b"tail", b"unflushed").expect("put tail"); // reste dans le WAL
    // Pas de close() : le WAL doit garder sa queue.
    drop(engine);
}

fn reopen(dir: &Path, encrypted: bool) -> Result<Engine, EngineError> {
    if encrypted {
        Engine::open_encrypted_with_options(dir, KEY, small_options())
    } else {
        Engine::open_with_options(dir, small_options())
    }
}

fn sst_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sst"))
        .collect();
    files.sort();
    files
}

fn flip_byte(path: &Path, offset_from_end: usize) {
    let mut bytes = std::fs::read(path).expect("read artifact");
    let idx = bytes.len().saturating_sub(1 + offset_from_end);
    bytes[idx] ^= 0xFF;
    std::fs::write(path, &bytes).expect("write corrupted artifact");
}

#[test]
fn sst_bit_flip_is_typed_corruption_clear_and_encrypted() {
    for encrypted in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        build_store(dir.path(), encrypted);
        let ssts = sst_files(dir.path());
        assert!(!ssts.is_empty(), "the store must have flushed at least one SST");
        // L'octet final vit dans le footer (bloc-based SST, ADR-039) : le
        // `footer_magic` de fin en clair, ou le tag Poly1305 de l'enveloppe
        // scellée en chiffré — les deux chemins doivent détecter le flip.
        flip_byte(&ssts[0], 0);

        let Err(err) = reopen(dir.path(), encrypted) else {
            panic!("encrypted={encrypted}: a flipped SST byte must fail the open")
        };
        assert!(
            matches!(
                err,
                EngineError::CorruptSstFooter { .. } | EngineError::CorruptEncryptedSstBlock { .. }
            ),
            "encrypted={encrypted}: expected CorruptSstFooter/CorruptEncryptedSstBlock, got {err:?}"
        );
    }
}

#[test]
fn wal_complete_record_bit_flip_is_typed_corruption() {
    for encrypted in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        build_store(dir.path(), encrypted);
        // Flip un octet du DERNIER enregistrement complet (la queue "tail") :
        // enregistrement entièrement bufferisé + checksum/AEAD faux = erreur
        // franche, pas la tolérance torn-tail (qui ne couvre que l'incomplet).
        flip_byte(&dir.path().join("wal.log"), 0);

        let Err(err) = reopen(dir.path(), encrypted) else {
            panic!("encrypted={encrypted}: a flipped complete WAL record must fail the open")
        };
        assert!(
            matches!(err, EngineError::CorruptWal { .. }),
            "encrypted={encrypted}: expected CorruptWal, got {err:?}"
        );
    }
}

#[test]
fn wal_truncated_tail_is_tolerated_and_prior_data_survives() {
    for encrypted in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        build_store(dir.path(), encrypted);
        // Ampute la fin du WAL (crash mid-append simulé) : la queue "tail"
        // devient un enregistrement déchiré → droppée en silence, les
        // données flushées restent intactes.
        let wal = dir.path().join("wal.log");
        let bytes = std::fs::read(&wal).expect("read wal");
        std::fs::write(&wal, &bytes[..bytes.len() - 3]).expect("truncate wal");

        let engine = reopen(dir.path(), encrypted).expect("torn tail must be tolerated");
        assert_eq!(engine.get(b"k0").expect("get").as_deref(), Some(&b"value"[..]));
        assert_eq!(
            engine.get(b"tail").expect("get"),
            None,
            "encrypted={encrypted}: the torn record is dropped, never half-applied"
        );
    }
}

#[test]
fn crypto_meta_bit_flip_is_typed_error_never_garbage() {
    let dir = tempfile::tempdir().expect("tempdir");
    build_store(dir.path(), true);
    flip_byte(&dir.path().join("crypto.meta"), 0);

    let Err(err) = reopen(dir.path(), true) else {
        panic!("a flipped crypto.meta byte must fail the open")
    };
    // Selon l'octet touché : structure invalide (CorruptCryptoMeta) ou wrap
    // qui n'authentifie plus (WrongEncryptionKey). Les deux sont des refus
    // typés — jamais un déchiffrement de garbage.
    assert!(
        matches!(
            err,
            EngineError::CorruptCryptoMeta { .. } | EngineError::WrongEncryptionKey { .. }
        ),
        "expected CorruptCryptoMeta or WrongEncryptionKey, got {err:?}"
    );
}

/// **Gap connu, assumé, toujours ouvert après N9 (ADR-040)** : le moteur n'a
/// aucun manifest listant les SSTs attendues — ni `Engine::open` (toujours
/// O(métadonnées), par design) ni `verify_store` en mode `FullLogical`
/// (confirmé empiriquement, N11.3 : `verify_store` rapporte `healthy: true`
/// sans warning sur un store dont une SST vivante a été supprimée — la passe
/// logique reconstruit la vue KV à partir des SSTs *présentes*, elle n'a
/// aucune source indépendante pour savoir qu'il en manque une). Une SST
/// supprimée (rm accidentel, ransomware, disque partiel) ne fait donc échouer
/// **ni** l'ouverture **ni** `verify` — les données qu'elle portait sont
/// juste absentes, silencieusement. Ce test pinne ce comportement ; le
/// fermer réclame un manifest/version-set (candidat naturel : N13/ADR-043),
/// pas juste `verify`.
#[test]
fn deleted_sst_is_currently_silent_data_loss_no_manifest_yet() {
    let dir = tempfile::tempdir().expect("tempdir");
    build_store(dir.path(), false);
    let ssts = sst_files(dir.path());
    std::fs::remove_file(&ssts[0]).expect("delete the only SST");

    let engine = reopen(dir.path(), false).expect(
        "sans manifest, open réussit sans la SST manquante — voir le commentaire du test \
         pour la portée exacte du gap (N13/ADR-043 est le candidat naturel pour le fermer)",
    );
    assert_eq!(
        engine.get(b"k0").expect("get"),
        None,
        "flushed data silently gone — aucun manifest pour le détecter, ni à l'open ni via verify"
    );
    // La queue WAL, elle, survit (fichier séparé).
    assert_eq!(engine.get(b"tail").expect("get").as_deref(), Some(&b"unflushed"[..]));
}
