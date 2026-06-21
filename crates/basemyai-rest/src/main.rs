//! Binaire du sidecar REST. Wiring de production : provisioning hardware-aware
//! de l'embedder (Candle) → provider chiffré → serveur axum sur `127.0.0.1`.
//!
//! **Privacy-first** : écoute la boucle locale uniquement ; le téléchargement du
//! modèle baseline n'a lieu **que** sur consentement explicite
//! (`BASEMYAI_FETCH=1`), conformément à l'ADR-010.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    run().await
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;

    use basemyai_core::{CandleEmbedder, Device, Embedder, EncryptionKey};
    use basemyai_rest::{AppState, Config, EncryptedFileProvider, build_app};
    use tokio::net::TcpListener;

    let config = Config::from_env()?;

    if !config.dev && config.api_key.is_none() {
        return Err("no API key configured: set [rest].api_key, BASEMYAI_REST_API_KEY, \
                    or run with BASEMYAI_REST_DEV=1 for a localhost-only dev server"
            .into());
    }

    // Clé de chiffrement de la base (obligatoire — chiffrement au repos, ADR-007).
    let db_key = config
        .db_key
        .clone()
        .ok_or("BASEMYAI_REST_DB_KEY or BASEMYAI_DB_KEY is required (encryption is mandatory)")?;

    let db_path = config.db_path.clone();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Embedder : modèle local si fourni, sinon provisioning hardware-aware
    // (fetch seulement si consenti).
    let embedder: Arc<dyn Embedder> = if let Some(model_path) = config.model_path.clone() {
        Arc::new(CandleEmbedder::load(&model_path, Device::Cpu)?)
    } else {
        let mp = basemyai::provision(config.consent_to_fetch).await?;
        Arc::new(CandleEmbedder::load(&mp.model_path, mp.device)?)
    };

    let provider = Arc::new(EncryptedFileProvider::new(
        db_path,
        EncryptionKey::new(db_key),
        embedder,
    ));
    let addr = config.socket_addr();
    let app = build_app(AppState::new(provider, config));

    let listener = TcpListener::bind(addr).await?;
    eprintln!("basemyai-rest listening on http://{addr}/v1");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(not(all(feature = "crypto", feature = "embed")))]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("basemyai-rest must be built with the `crypto` and `embed` features for the production server".into())
}
