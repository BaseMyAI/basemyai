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

    use basemyai_core::{CandleEmbedder, Embedder, EncryptionKey};
    use basemyai_rest::{AppState, Config, EncryptedFileProvider, build_app};
    use tokio::net::TcpListener;

    let config = Config::from_env()?;

    if !config.dev && config.api_key.is_none() {
        return Err("no API key configured: set [rest].api_key, BASEMYAI_REST_API_KEY, \
                    or run with BASEMYAI_REST_DEV=1 for a localhost-only dev server"
            .into());
    }

    // Clé de chiffrement de la base (obligatoire — chiffrement au repos, ADR-007).
    let db_key =
        std::env::var("BASEMYAI_DB_KEY").map_err(|_| "BASEMYAI_DB_KEY is required (encryption is mandatory)")?;

    // Chemin de la base : ~/.basemyai/memory.db.
    let home = dirs::home_dir().ok_or("cannot resolve home directory")?;
    let db_path = home.join(".basemyai").join("memory.db");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Embedder : provisioning hardware-aware (fetch seulement si consenti).
    let consent = std::env::var("BASEMYAI_FETCH").map(|v| v == "1").unwrap_or(false);
    let mp = basemyai::provision(consent).await?;
    let embedder: Arc<dyn Embedder> = Arc::new(CandleEmbedder::load(&mp.model_path, mp.device)?);

    let provider = Arc::new(EncryptedFileProvider::new(
        db_path,
        EncryptionKey::new(db_key),
        embedder,
    ));
    let port = config.port;
    let app = build_app(AppState::new(provider, config));

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!("basemyai-rest listening on http://127.0.0.1:{port}/v1");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(not(all(feature = "crypto", feature = "embed")))]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("basemyai-rest must be built with the `crypto` and `embed` features for the production server".into())
}
