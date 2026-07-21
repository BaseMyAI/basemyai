// SPDX-License-Identifier: BUSL-1.1
//! Arrêt gracieux : `axum::serve(...).with_graceful_shutdown(signal())`
//! laisse les requêtes en vol se terminer (jusqu'au timeout HTTP habituel)
//! avant de fermer le listener. Un client SSE lent se voit simplement couper
//! son flux — pas de blocage de l'arrêt (voir `endpoints::events`).

/// Résout à la première de `Ctrl+C` ou `SIGTERM` (Unix). Sur les plateformes
/// sans `SIGTERM` (Windows), seul `Ctrl+C` est écouté.
pub async fn signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received, draining in-flight requests");
}
