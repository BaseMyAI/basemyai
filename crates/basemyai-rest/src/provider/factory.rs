// SPDX-License-Identifier: BUSL-1.1
//! Construit le [`MemoryProvider`] de production : résolution de la clé de
//! chiffrement (ADR-034), embedder (modèle local ou provisioning
//! hardware-aware, ADR-010), ouverture du store natif. Extrait de `main.rs`
//! pour rester testable indépendamment du binaire et du réseau.

use std::sync::Arc;

use basemyai_core::{CandleEmbedder, Device, Embedder, EncryptionKey, KeyResolveError};

use super::error::ProviderError;
use super::production::FileProvider;
use crate::config::StartupConfig;

/// Construit le provider de production à partir d'une [`StartupConfig`].
///
/// # Errors
/// [`ProviderError::KeyResolution`] si aucune clé n'est résoluble (ADR-034) ;
/// [`ProviderError::ModelLoad`]/[`ProviderError::Provisioning`] si l'embedder
/// ne peut être chargé/provisionné ; [`ProviderError::Memory`]/
/// [`ProviderError::DataDirectory`] si l'ouverture du store échoue.
#[cfg(feature = "embed")]
pub async fn build(config: &StartupConfig) -> Result<FileProvider, ProviderError> {
    let db_key = EncryptionKey::resolve(config.db_key.as_ref().map(EncryptionKey::expose)).map_err(|e| match e {
        KeyResolveError::Missing(msg) => ProviderError::KeyResolution(msg),
        other => ProviderError::KeyResolution(other.to_string()),
    })?;

    let embedder: Arc<dyn Embedder> = if let Some(model_path) = config.model_path.clone() {
        Arc::new(CandleEmbedder::load(&model_path, Device::Cpu).map_err(|e| ProviderError::ModelLoad(e.to_string()))?)
    } else {
        let provisioned = basemyai::provision(config.consent_to_fetch)
            .await
            .map_err(|e| ProviderError::Provisioning(e.to_string()))?;
        Arc::new(
            CandleEmbedder::load(&provisioned.model_path, provisioned.device)
                .map_err(|e| ProviderError::ModelLoad(e.to_string()))?,
        )
    };

    FileProvider::open(config.db_path.clone(), db_key, embedder).await
}
