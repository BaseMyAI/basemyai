// SPDX-License-Identifier: BUSL-1.1
//! Binaire du sidecar REST. **Privacy-first** : écoute la boucle locale par
//! défaut ; le téléchargement du modèle baseline n'a lieu **que** sur
//! consentement explicite (`BASEMYAI_FETCH=1`/`BASEMYAI_REST_FETCH=1`,
//! ADR-010). Toute la logique testable vit dans `basemyai_rest` (bibliothèque) ;
//! ce binaire ne fait que charger la config, initialiser la télémétrie,
//! construire le serveur, l'exécuter et gérer l'arrêt.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    run().await
}

#[cfg(feature = "embed")]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    use basemyai_rest::server::{bootstrap, build_router, shutdown, telemetry};
    use tokio::net::TcpListener;

    telemetry::init();

    let (startup, state) = bootstrap::build_state().await?;
    let addr = startup.socket_addr();
    let app = build_router(state);

    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "basemyai-rest listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown::signal())
        .await?;
    Ok(())
}

#[cfg(not(feature = "embed"))]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "basemyai-rest must be built with the `embed` feature for the production server \
         (it is in the default feature set)"
            .into(),
    )
}
