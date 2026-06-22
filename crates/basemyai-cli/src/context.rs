//! Helpers partagés par toutes les commandes : ouverture de la clé/du store/
//! de la mémoire, conversion `cli::Layer` -> `basemyai::MemoryLayer`. Isole
//! les commandes de la mécanique d'ouverture d'un `.bmai` (ADR-007/ADR-019).

use std::path::Path;
use std::sync::Arc;

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

/// Clé de chiffrement depuis `BASEMYAI_DB_KEY` (obligatoire, ADR-007).
pub(crate) fn require_key() -> Result<basemyai_core::EncryptionKey, CliError> {
    let raw = std::env::var("BASEMYAI_DB_KEY").map_err(|_| CliError::MissingKey)?;
    Ok(basemyai_core::EncryptionKey::new(raw))
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

/// Ouvre un store chiffré sans embedder (commandes purement structurelles).
pub(crate) async fn open_store(path: &Path) -> Result<basemyai_core::Store, CliError> {
    if path.extension().and_then(|e| e.to_str()) != Some("bmai") {
        eprintln!("warning: '{}' does not use the .bmai extension", path.display());
    }
    let key = require_key()?;
    Ok(basemyai_core::Store::open(path, Some(key)).await?)
}

/// Ouvre une mémoire complète (store chiffré + embedder + isolation agent).
pub(crate) async fn open_memory(path: &Path, agent: &str) -> Result<basemyai::Memory, CliError> {
    let agent_id = basemyai::AgentId::new(agent).ok_or(CliError::InvalidAgent)?;
    let store = open_store(path).await?;
    let embedder = load_embedder().await?;
    Ok(basemyai::Memory::open(store, embedder, agent_id).await?)
}

/// Ouvre l'accès mémoire bas niveau (store chiffré + migrations), sans
/// embedder — pour les opérations qui ne font aucun embedding (forget,
/// invalidate, purge, graphe). Évite de payer le chargement du modèle
/// Candle pour des mutations purement SQL.
pub(crate) async fn open_engine(
    path: &Path,
    agent: &str,
) -> Result<(Arc<dyn basemyai::storage::MemoryStore>, basemyai::AgentId), CliError> {
    let agent_id = basemyai::AgentId::new(agent).ok_or(CliError::InvalidAgent)?;
    let store = open_store(path).await?;
    store.migrate(&basemyai::schema()).await?;
    let engine: Arc<dyn basemyai::storage::MemoryStore> = Arc::new(basemyai::storage::LibsqlMemoryStore::new(store));
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
