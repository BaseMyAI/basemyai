// SPDX-License-Identifier: BUSL-1.1
//! Helpers partagés par toutes les commandes : ouverture de la clé/du store/
//! de la mémoire, conversion `cli::Layer` -> `basemyai::MemoryLayer`. Isole
//! les commandes de la mécanique d'ouverture d'un `.bmai` (ADR-007/ADR-030/032).

use std::path::Path;
use std::sync::Arc;

use basemyai_core::{EncryptionKey, KeyResolveError};

use crate::cli::Layer;
use crate::error::CliError;

pub(crate) fn memory_layer(layer: Layer) -> basemyai::MemoryLayer {
    use basemyai::MemoryLayer;
    match layer {
        Layer::ShortTerm => MemoryLayer::ShortTerm,
        Layer::Episodic => MemoryLayer::Episodic,
        Layer::Procedural => MemoryLayer::Procedural,
        Layer::Semantic => MemoryLayer::Semantic,
    }
}

/// Passphrase de chiffrement via la résolution centralisée ADR-034.
pub(crate) fn require_key() -> Result<basemyai_core::EncryptionKey, CliError> {
    EncryptionKey::resolve(None).map_err(map_key_error)
}

fn map_key_error(err: KeyResolveError) -> CliError {
    match err {
        KeyResolveError::Missing(msg) => CliError::MissingKey(msg),
        other => CliError::KeyResolution(other.to_string()),
    }
}

/// Charge l'embedder baseline depuis le cache (sans téléchargement). Guide vers
/// `basemyai setup --fetch` si le modèle est absent.
pub(crate) async fn load_embedder() -> Result<Box<dyn basemyai_core::Embedder>, CliError> {
    let mp = basemyai::provision(false)
        .await
        .map_err(|e| CliError::ModelNotProvisioned(e.to_string()))?;
    let embedder = basemyai_core::CandleEmbedder::load(&mp.model_path, mp.device)?;
    Ok(Box::new(embedder))
}

/// Ouvre un store natif chiffré (au besoin le crée).
///
/// # Errors
/// Erreur de stockage si la clé est fausse ou si l'ouverture échoue.
pub(crate) async fn open_store(path: &Path) -> Result<basemyai::storage::NativeMemoryStore, CliError> {
    let key = require_key()?;
    open_store_with_key(path, key).await
}

/// Ouvre un store avec une credential déjà résolue. Les opérations qui
/// doivent connaître le mode authentifié évitent ainsi une seconde résolution
/// de l'environnement entre la détection et l'ouverture.
pub(crate) async fn open_store_with_key(
    path: &Path,
    key: basemyai_core::EncryptionKey,
) -> Result<basemyai::storage::NativeMemoryStore, CliError> {
    if path.extension().and_then(|e| e.to_str()) != Some("bmai") {
        crate::ui::render::warning(&format!("'{}' does not use the .bmai extension", path.display()));
    }
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || basemyai::storage::NativeMemoryStore::open_with_key(&path, &key))
        .await
        .map_err(|e| {
            CliError::Core(basemyai_core::CoreError::Storage(format!(
                "ouverture du store natif interrompue : {e}"
            )))
        })?
        .map_err(CliError::from)
}

/// Ouvre une mémoire complète (store + embedder + isolation agent).
pub(crate) async fn open_memory(path: &Path, agent: &str) -> Result<basemyai::Memory, CliError> {
    let agent_id = basemyai::AgentId::new(agent).ok_or(CliError::InvalidAgent)?;
    let key = require_key()?;
    let embedder = load_embedder().await?;
    Ok(basemyai::Memory::open_native(path, &key, embedder, agent_id).await?)
}

/// Ouvre l'accès mémoire bas niveau (store, sans embedder) — pour les
/// opérations qui ne font aucun embedding (forget, invalidate, purge, graphe,
/// list). Évite de payer le chargement du modèle Candle.
pub(crate) async fn open_engine(
    path: &Path,
    agent: &str,
) -> Result<(Arc<dyn basemyai::storage::MemoryStore>, basemyai::AgentId), CliError> {
    let agent_id = basemyai::AgentId::new(agent).ok_or(CliError::InvalidAgent)?;
    let store = open_store(path).await?;
    let engine: Arc<dyn basemyai::storage::MemoryStore> = Arc::new(store);
    Ok((engine, agent_id))
}

/// Temps Unix courant (secondes, UTC). `0` si l'horloge est antérieure à
/// l'epoch, sature à `i64::MAX` en cas de dépassement — même politique que
/// `basemyai::now_unix` (interne à ce crate, non accessible depuis le CLI).
pub(crate) fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
