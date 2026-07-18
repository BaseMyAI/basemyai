// SPDX-License-Identifier: BUSL-1.1
//! Seconde implémentation de [`MemoryStore`](super::MemoryStore) : le moteur
//! natif BaseMyAI (ADR-024/025/026, câblage acté par ADR-027). Enveloppe
//! [`basemyai_engine::Engine`] + ses quatre index logiques (vecteur, graphe,
//! mémoire, full-text) et concentre toute la **politique** de requête du
//! backend natif — fenêtres de validité, filtre de couche, oversampling —
//! pendant que la **mécanique** crash-critique (composition de batchs
//! atomiques, allocation d'ids) vit côté moteur
//! (`idx::memory::PersistentMemoryIndex`, `idx::fts::PersistentFts`).
//!
//! Module scindé par responsabilité (le fichier plat dépassait 1250 lignes) :
//! ce fichier porte le cycle de vie du store (ouverture, rotation de clé,
//! méta conteneur, pont sync↔async) ; [`inner`] porte les primitives
//! internes de [`NativeInner`] (recherche filtrée, insertion batch) ;
//! [`porting`] porte export/import JSONL ; [`trait_impl`] porte
//! l'implémentation du trait [`MemoryStore`](super::MemoryStore) lui-même.
//!
//! ## Parité comportementale (ADR-027 §6, ADR-028)
//!
//! Chaque méthode implémente la sémantique de requête historique du domaine,
//! y compris ses non-filtres : `hydrate` et `exact_fact_exists` ne vérifient
//! **pas** la validité temporelle, `graph_upsert_edge`
//! préserve le `valid_from` d'une arête existante et ne met à jour que
//! `weight` (parité sémantique historique V1 : upsert ne modifie que le poids
//! d'une arête existante). Le KNN
//! oversample ×[`OVERSAMPLE`] puis post-filtre (ADR-012 : un filtre
//! agent+validité est *toujours* présent). `keyword_ranking_ids` est BM25
//! natif (ADR-028) sur le sous-ensemble de `match_expr` que
//! `fts_match_expr()` produit réellement — pas de racinisation Porter (gap
//! assumé, ADR-028 §2).
//!
//! `put_memory_batch` est **tout-ou-rien** depuis N5.5
//! (`PersistentMemoryIndex::put_many`, résorbant l'écart initial d'ADR-027
//! §6). Écart restant, assumé et documenté : `purge_agent` est
//! idempotent/reprennable (pas globalement atomique — un crash au milieu se
//! répare en relançant, ADR-027 §6). Les métriques non-cosinus retournent
//! une **erreur franche** (N5.3) — jamais un faux résultat.
//!
//! ## Pont sync↔async et concurrence (ADR-027 §5, N5.5)
//!
//! Le moteur (`basemyai_engine::Engine`) est sync **mono-écrivain** — ça ne
//! change pas ici, `apply_batch`/`put`/`delete` exigent `&mut Engine`. Le
//! trait est async ; chaque méthode s'exécute dans `tokio::task::
//! spawn_blocking`, le verrou pris à l'intérieur de la closure bloquante —
//! jamais tenu à travers un `.await` (lint `await_holding_lock`). Depuis
//! N5.5, `inner` est un `RwLock` : les lectures pures (`vector_ranking_ids`,
//! `keyword_ranking_ids`, `agent_stats`, `graph_traverse`,
//! `recent_episodes`, `exact_fact_exists`) prennent un verrou de lecture et
//! s'exécutent concurremment entre elles (mesuré : ~3× plus rapide que
//! séquentiel sur 64 lectures mixtes, `tests/memory_tests.rs
//! native_concurrent_reads_are_correct_and_faster_than_sequential`). Les
//! chemins hybrides (`recall_vector`, `recall_graph_filtered`, `hydrate`)
//! font deux passes — recherche sous verrou de lecture, `touch` de
//! `last_access` sous un verrou d'écriture bref séparé — plutôt qu'une passe
//! unique sous verrou exclusif qui bloquerait tout lecteur concurrent
//! pendant toute la recherche. Les écritures restent sérialisées entre elles
//! (verrou d'écriture exclusif) : lever *ça* exigerait de faire du moteur
//! lui-même un multi-écrivain, hors périmètre N5.5 (voir
//! `docs/adr/ADR-027-native-memory-store.md` §5).

mod inner;
mod porting;
mod trait_impl;

pub use porting::NativeExportRows;
pub(crate) use porting::{NativeImportEdge, NativeImportEntity, NativeImportMemory};

use std::path::Path;
use std::sync::{Arc, RwLock};

use basemyai_engine::{Engine, PersistentFts, PersistentGraph, PersistentMemoryIndex, PersistentVectorIndex};

use crate::Result;

/// Préfixe KV des métadonnées conteneur (équivalent sémantique `bmai_meta`, ADR-019).
/// Paires clé/valeur UTF-8 brutes que `basemyai` possède (contrat embedding,
/// `format`/`storage_engine`/…) — hors du keyspace réservé `idx/` du moteur.
const BMAI_META_PREFIX: &[u8] = b"meta/bmai/";

/// Clé complète d'une entrée de méta consommateur.
fn bmai_meta_key(name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(BMAI_META_PREFIX.len() + name.len());
    key.extend_from_slice(BMAI_META_PREFIX);
    key.extend_from_slice(name.as_bytes());
    key
}

/// Version publique du conteneur `.bmai` (ADR-033 : moteur natif, seul backend).
pub const BMAI_FORMAT_VERSION: u32 = 2;

/// Seme les méta de conteneur (`format`/`format_version`/`storage_engine`/
/// `schema_family`/`embedding_dim`) au premier `open` d'un store natif.
/// Idempotent (`INSERT OR IGNORE`, une entrée déjà présente n'est jamais
/// réécrite) : appelé à **chaque** ouverture, jamais seulement à la création.
fn ensure_container_meta(engine: &mut Engine) -> Result<()> {
    for (name, value) in [
        ("format", "basemyai-memory".to_string()),
        ("format_version", BMAI_FORMAT_VERSION.to_string()),
        ("storage_engine", "native".to_string()),
        ("schema_family", "agent-memory".to_string()),
        ("embedding_dim", crate::EMBEDDING_DIM.to_string()),
    ] {
        let key = bmai_meta_key(name);
        if engine.get(&key).map_err(storage)?.is_none() {
            engine.put(&key, value.as_bytes()).map_err(storage)?;
        }
    }
    Ok(())
}

/// Facteur d'oversampling du KNN filtré (ADR-012) : on demande `k × 8`
/// candidats à l'index, puis le post-filtre agent/validité/couche réduit à
/// `k` — politique ADR-012 (filtre agent+validité toujours présent).
const OVERSAMPLE: usize = 8;

/// Moteur de stockage natif — ADR-024/ADR-027/ADR-033 (unique implémentation `MemoryStore`).
///
/// Concurrence (N5.5, barre hardening M6) : `inner` est un `RwLock`, pas un
/// `Mutex` — les chemins de lecture pure (`vector_ranking_ids`,
/// `keyword_ranking_ids`, `agent_stats`, `graph_traverse`,
/// `recent_episodes`, `exact_fact_exists`) prennent un verrou de **lecture**
/// et s'exécutent concurremment entre eux. Les chemins hybrides
/// (`recall_vector`, `recall_graph_filtered`, `hydrate`) font deux passes
/// séparées : la recherche sous verrou de lecture, puis le `touch`
/// (`last_access`) sous un bref verrou d'écriture — jamais une passe unique
/// sous verrou d'écriture qui bloquerait les lecteurs pendant toute la
/// recherche. Les écritures (`put_memory*`, `invalidate`, `forget`,
/// `purge_agent`, `graph_upsert_*`, `rotate_key`) restent sous verrou
/// d'écriture exclusif — `Engine` lui-même reste mono-écrivain (ADR-025) ;
/// ce `RwLock` ne change rien à ça, il ne fait qu'arrêter de sérialiser les
/// lecteurs entre eux. Voir `docs/adr/ADR-027-native-memory-store.md` §5
/// pour le contexte : ce `RwLock` remplace le `Mutex` que ce paragraphe
/// décrivait comme la barre à lever en N5.5.
pub struct NativeMemoryStore {
    inner: Arc<RwLock<NativeInner>>,
    /// Garde de vie du répertoire temporaire d'[`Self::open_ephemeral`] —
    /// supprimé au drop du store (store éphémère test-only).
    #[cfg(any(test, feature = "test-util"))]
    _tempdir: Option<tempfile::TempDir>,
}

struct NativeInner {
    engine: Engine,
    vectors: PersistentVectorIndex,
    memory: PersistentMemoryIndex,
    graph: PersistentGraph,
    fts: PersistentFts,
}

/// Mappe une erreur du backend natif (ou du pont async) en
/// [`crate::MemoryError`].
fn storage(e: impl std::fmt::Display) -> crate::MemoryError {
    basemyai_core::CoreError::Storage(e.to_string()).into()
}

/// Traduit les erreurs crypto/ouverture du moteur en variantes stables du
/// core — sans exposer chemins disque ni matériel de clé dans le message.
/// `pub(crate)` : réutilisée par `storage::integrity`, qui ouvre le moteur
/// natif directement (pas via [`NativeMemoryStore`]) mais doit préserver la
/// même distinction typée (`WRONG_ENCRYPTION_KEY` vs `STORAGE_ERROR`, etc.)
/// côté CLI.
pub(crate) fn map_engine_error(e: basemyai_engine::EngineError) -> crate::MemoryError {
    use basemyai_core::CoreError;
    use basemyai_engine::EngineError;
    match e {
        EngineError::MissingEncryptionKey { .. } => CoreError::EncryptionKeyRequired.into(),
        EngineError::WrongEncryptionKey { .. } => CoreError::WrongEncryptionKey.into(),
        EngineError::CorruptCryptoMeta { .. } => CoreError::CorruptEncryptionMetadata.into(),
        EngineError::StoreLocked { .. } => CoreError::StoreLocked.into(),
        EngineError::CorruptGenerationMeta { .. } => CoreError::CorruptStoreGenerationMetadata.into(),
        EngineError::PlaintextStoreKeySupplied { .. } => CoreError::PlaintextStoreEncryptedKeySupplied.into(),
        EngineError::NotEncrypted { .. } => CoreError::Encryption.into(),
        EngineError::CryptoFailure { .. } => CoreError::Encryption.into(),
        other => CoreError::Storage(other.to_string()).into(),
    }
}

/// `true` si la fenêtre `[valid_from, valid_until)` couvre `now` — le filtre
/// `valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)` commun à
/// tous les recalls (ADR-005). Utilisée par [`inner`] et [`trait_impl`].
fn record_valid_at(record: &basemyai_engine::MemoryRecord, now: i64) -> bool {
    record.valid_from <= now && record.valid_until.is_none_or(|until| until > now)
}

impl NativeMemoryStore {
    /// Ouvre (en le créant au besoin) un store natif **en clair** dans le
    /// répertoire `path` — **réservé aux tests** (`test-util`).
    ///
    /// La production utilise [`Self::open_encrypted`] (ADR-030/033).
    ///
    /// # Errors
    /// Erreur de stockage si le moteur ou l'un de ses index ne s'ouvre pas.
    #[cfg(any(test, feature = "test-util"))]
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_engine(Engine::open(path).map_err(map_engine_error)?)
    }

    /// Ouvre (en le créant au besoin) un store natif **chiffré au repos**
    /// par une clé brute (ADR-030) : WAL et SST scellés sous la DEK du store,
    /// `key` vérifiée contre `crypto.meta` à l'ouverture — une mauvaise clé
    /// échoue ici, typée, jamais en corruption inexplicable plus loin.
    ///
    /// # Errors
    /// Erreur de stockage si la clé est fausse, si `path` contient déjà un
    /// store en clair (pas de chiffrement a posteriori, ADR-030 §2), ou sur
    /// toute erreur I/O/corruption d'ouverture.
    pub fn open_encrypted(path: impl AsRef<Path>, key: &str) -> Result<Self> {
        Self::from_engine(Engine::open_encrypted(path, key.as_bytes()).map_err(map_engine_error)?)
    }

    /// Ouvre un store chiffré selon le mode explicitement porté par
    /// [`basemyai_core::EncryptionKey`].
    ///
    /// C'est le point d'entrée recommandé aux SDKs : il évite qu'une
    /// passphrase soit accidentellement ouverte comme une clé brute.
    pub fn open_with_key(path: impl AsRef<Path>, key: &basemyai_core::EncryptionKey) -> Result<Self> {
        match key.mode() {
            basemyai_core::EncryptionKeyMode::RawKey => Self::open_encrypted(path, key.expose()),
            basemyai_core::EncryptionKeyMode::Passphrase => Self::open_with_passphrase(path, key.expose()),
            _ => Err(storage("unsupported encryption key mode")),
        }
    }

    /// Ouvre (en le créant au besoin) un store natif chiffré avec une
    /// passphrase humaine, étirée avec Argon2id et persistée comme telle dans
    /// `CryptoMeta:2` (ADR-042).
    ///
    /// Une passphrase et une clé brute de mêmes octets ne se substituent
    /// jamais : un store de l'autre mode échoue à l'ouverture avec l'erreur
    /// de clé typée du moteur.
    ///
    /// # Errors
    /// Erreur de stockage si la passphrase est fausse, si le store est déjà
    /// chiffré dans l'autre mode, ou sur toute erreur I/O/corruption
    /// d'ouverture.
    pub fn open_with_passphrase(path: impl AsRef<Path>, passphrase: &str) -> Result<Self> {
        Self::from_engine(Engine::open_with_passphrase(path, passphrase.as_bytes()).map_err(map_engine_error)?)
    }

    /// Creates a passphrase store with an explicit Argon2id cost profile.
    /// On reopen, the persisted parameters are replayed regardless of the
    /// caller's current defaults.
    pub fn open_with_passphrase_and_profile(
        path: impl AsRef<Path>,
        passphrase: &str,
        profile: basemyai_engine::Argon2idProfile,
    ) -> Result<Self> {
        Self::from_engine(
            Engine::open_with_passphrase_and_profile(path, passphrase.as_bytes(), profile).map_err(map_engine_error)?,
        )
    }

    fn from_engine(mut engine: Engine) -> Result<Self> {
        let params = basemyai_engine::VectorIndexParams::with_dim(crate::EMBEDDING_DIM);
        let vectors = PersistentVectorIndex::open(&mut engine, params).map_err(storage)?;
        let memory = PersistentMemoryIndex::open(&engine).map_err(storage)?;
        ensure_container_meta(&mut engine)?;
        Ok(Self {
            inner: Arc::new(RwLock::new(NativeInner {
                engine,
                vectors,
                memory,
                graph: PersistentGraph::new(),
                fts: PersistentFts::new(),
            })),
            #[cfg(any(test, feature = "test-util"))]
            _tempdir: None,
        })
    }

    /// Fait tourner la clé de chiffrement **en place** (ADR-030 §4) : la DEK
    /// du store est ré-enveloppée sous une KEK dérivée de `new_key`,
    /// `crypto.meta` remplacé atomiquement. O(1), et **cette instance reste
    /// pleinement utilisable après l'appel** — pas de réouverture requise
    /// (contrairement à l'ancien chemin `PRAGMA rekey` libSQL).
    ///
    /// # Errors
    /// Erreur de stockage si le store n'a pas été ouvert chiffré (rien à
    /// rotater — parité de posture avec `CoreError::Encryption`, ADR-007) ou
    /// si le remplacement atomique échoue.
    pub async fn rotate_key(&self, new_key: &str) -> Result<()> {
        self.rotate_with_key(basemyai_core::EncryptionKey::raw(new_key)).await
    }

    /// Re-scelle la DEK selon le mode explicite porté par `new_key`.
    /// Cette variante évite toute réinterprétation d'une passphrase en clé
    /// brute et conserve le secret dans un buffer zeroizable jusqu'à la fin
    /// de la closure bloquante.
    pub async fn rotate_with_key(&self, new_key: basemyai_core::EncryptionKey) -> Result<()> {
        self.with_inner(move |inner| {
            let result = match new_key.mode() {
                basemyai_core::EncryptionKeyMode::RawKey => inner.engine.rotate_key(new_key.expose().as_bytes()),
                basemyai_core::EncryptionKeyMode::Passphrase => {
                    inner.engine.rotate_passphrase(new_key.expose().as_bytes())
                }
                _ => return Err(storage("unsupported encryption key mode")),
            };
            result.map_err(map_engine_error)
        })
        .await
    }

    /// Re-scelle la DEK avec une passphrase et un profil Argon2id explicite.
    /// Le profil low-memory doit être redemandé à chaque rotation qui doit le
    /// conserver ; sinon la rotation revient au profil par défaut ADR-042.
    pub async fn rotate_passphrase_with_profile(
        &self,
        new_passphrase: basemyai_core::EncryptionKey,
        profile: basemyai_engine::Argon2idProfile,
    ) -> Result<()> {
        if new_passphrase.mode() != basemyai_core::EncryptionKeyMode::Passphrase {
            return Err(storage("Argon2id profiles require a passphrase encryption key"));
        }
        self.with_inner(move |inner| {
            inner
                .engine
                .rotate_passphrase_with_profile(new_passphrase.expose().as_bytes(), profile)
                .map_err(map_engine_error)
        })
        .await
    }

    /// Ré-encrypte tous les enregistrements vivants sous une nouvelle DEK,
    /// publie atomiquement la génération résultante puis collecte l'ancienne.
    pub async fn rotate_key_full(&self, new_key: basemyai_core::EncryptionKey) -> Result<()> {
        self.with_inner(move |inner| {
            let result = match new_key.mode() {
                basemyai_core::EncryptionKeyMode::RawKey => inner.engine.rotate_key_full(new_key.expose().as_bytes()),
                basemyai_core::EncryptionKeyMode::Passphrase => {
                    inner.engine.rotate_passphrase_full(new_key.expose().as_bytes())
                }
                _ => return Err(storage("unsupported encryption key mode")),
            };
            result.map_err(map_engine_error)
        })
        .await
    }

    /// Rotation complète de DEK avec un profil Argon2id explicite.
    pub async fn rotate_passphrase_full_with_profile(
        &self,
        new_passphrase: basemyai_core::EncryptionKey,
        profile: basemyai_engine::Argon2idProfile,
    ) -> Result<()> {
        if new_passphrase.mode() != basemyai_core::EncryptionKeyMode::Passphrase {
            return Err(storage("Argon2id profiles require a passphrase encryption key"));
        }
        self.with_inner(move |inner| {
            inner
                .engine
                .rotate_passphrase_full_with_profile(new_passphrase.expose().as_bytes(), profile)
                .map_err(map_engine_error)
        })
        .await
    }

    /// Store natif jetable dans un répertoire temporaire, supprimé au drop
    /// (le moteur LSM n'a pas de mode in-memory). Réservé aux tests.
    ///
    /// # Errors
    /// Erreur de stockage si le répertoire temporaire ou le store ne se
    /// crée pas.
    #[cfg(any(test, feature = "test-util"))]
    pub fn open_ephemeral() -> Result<Self> {
        let dir = tempfile::tempdir().map_err(storage)?;
        let mut store = Self::open(dir.path())?;
        store._tempdir = Some(dir);
        Ok(store)
    }

    /// Variante chiffrée d'[`Self::open_ephemeral`] — même répertoire
    /// temporaire jetable, ouvert via [`Self::open_encrypted`]. Réservé aux
    /// tests (le diff multi-backend rejoue la suite complète des scénarios
    /// contre un store natif chiffré, N5.4).
    ///
    /// # Errors
    /// Erreur de stockage si le répertoire temporaire ou le store ne se
    /// crée pas.
    #[cfg(any(test, feature = "test-util"))]
    pub fn open_ephemeral_encrypted(key: &str) -> Result<Self> {
        let dir = tempfile::tempdir().map_err(storage)?;
        let mut store = Self::open_encrypted(dir.path(), key)?;
        store._tempdir = Some(dir);
        Ok(store)
    }

    /// Méta de conteneur (`format`/`format_version`/`storage_engine`/…, plus
    /// `embedding_model_id`/`embedding_dim` si une [`crate::Memory`] a déjà
    /// été ouverte dessus) — lecture des paires clé/valeur sous le préfixe
    /// `bmai_meta` (CLI `inspect`/`verify`, ADR-033). Triée par
    /// nom pour un affichage stable.
    ///
    /// # Errors
    /// Erreur de stockage si le scan échoue.
    pub async fn container_metadata(&self) -> Result<Vec<(String, String)>> {
        self.with_inner_read(|inner| {
            let entries = inner.engine.scan_prefix(BMAI_META_PREFIX).map_err(storage)?;
            let mut out = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                let name = String::from_utf8(key.as_bytes()[BMAI_META_PREFIX.len()..].to_vec())
                    .map_err(|e| storage(format!("nom de méta consommateur non UTF-8 : {e}")))?;
                let value = String::from_utf8(value).map_err(|e| storage(format!("valeur de méta non UTF-8 : {e}")))?;
                out.push((name, value));
            }
            out.sort();
            Ok(out)
        })
        .await
    }

    /// Nombre total de souvenirs, **toutes couches et tous agents confondus**
    /// — l'homologue natif de `SELECT COUNT(*) FROM memory` du CLI `inspect`
    /// (ADR-032).
    ///
    /// # Errors
    /// Erreur de stockage si le scan échoue.
    pub async fn total_memory_count(&self) -> Result<u64> {
        self.with_inner_read(|inner| inner.memory.count_all(&inner.engine).map_err(storage))
            .await
    }

    /// Exécute `f` sur l'état natif dans le pool bloquant de tokio sous
    /// verrou d'**écriture** (exclusif), pris à l'intérieur de la closure
    /// (jamais à travers un `.await`) — les mutations (`put_memory*`,
    /// `invalidate`, `forget`, `purge_agent`, `graph_upsert_*`,
    /// `rotate_key`, et le `touch` des chemins hybrides) passent par ici.
    async fn with_inner<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut NativeInner) -> Result<T> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard = inner
                .write()
                .map_err(|_| storage("verrou d'écriture du store natif empoisonné"))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| storage(format!("tâche bloquante du store natif interrompue : {e}")))?
    }

    /// [`Self::with_inner`], sous verrou de **lecture** partagé (N5.5) : `f`
    /// n'a droit qu'à `&NativeInner` — plusieurs lectures peuvent s'exécuter
    /// concurremment tant qu'aucune écriture n'est en cours. Réservé aux
    /// chemins qui ne mutent rien (ni les index, ni `last_access`).
    async fn with_inner_read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&NativeInner) -> Result<T> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let guard = inner
                .read()
                .map_err(|_| storage("verrou de lecture du store natif empoisonné"))?;
            f(&guard)
        })
        .await
        .map_err(|e| storage(format!("tâche bloquante du store natif interrompue : {e}")))?
    }

    /// Les identifiants d'agents du registre natif (ADR-041 §7.5), triés en
    /// ordre d'octets croissant — **identifiants seuls**, jamais aucune
    /// donnée par agent (l'isolation ADR-006 reste structurelle pour tout ce
    /// que l'id débloque ensuite). Un agent est inscrit par son premier
    /// souvenir et désinscrit par [`MemoryStore`](super::MemoryStore)`::purge_agent` ;
    /// oublier son dernier souvenir laisse volontairement l'entrée (le
    /// registre répond « quels agents une passe de maintenance doit-elle
    /// visiter » — visiter un agent vide est un no-op bon marché). Méthode
    /// inhérente, pas sur le trait : c'est une lecture **conteneur**, pas une
    /// opération d'agent — même statut que [`Self::total_memory_count`].
    ///
    /// # Errors
    /// Erreur de stockage si le scan échoue.
    pub async fn list_agents(&self) -> Result<Vec<String>> {
        self.with_inner_read(|inner| inner.memory.list_agents(&inner.engine).map_err(storage))
            .await
    }

    /// Sémantique `INSERT OR IGNORE` puis lecture sur la méta consommateur
    /// (ADR-033) : si `name` existe, renvoie la valeur **stockée** (jamais
    /// écrasée) ; sinon écrit `value` et la renvoie. Brique du contrat
    /// embedding (`ensure_embedding_contract`).
    pub(crate) async fn meta_ensure(&self, name: &str, value: &str) -> Result<String> {
        let (name, value) = (name.to_string(), value.to_string());
        self.with_inner(move |inner| {
            let key = bmai_meta_key(&name);
            if let Some(existing) = inner.engine.get(&key).map_err(storage)? {
                return String::from_utf8(existing)
                    .map_err(|e| storage(format!("méta consommateur {name:?} non UTF-8 : {e}")));
            }
            inner.engine.put(&key, value.as_bytes()).map_err(storage)?;
            Ok(value)
        })
        .await
    }
}
