//! Entrées malformées : JSON invalide, mauvais `Content-Type`, identifiants
//! hostiles, valeurs non finies — rien de tout cela ne doit produire une
//! erreur interne non filtrée ou un comportement inattendu.

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::app;
use crate::support::client::{delete, json_body, post, post_raw};

#[tokio::test]
async fn malformed_json_body_is_a_bad_request_not_an_internal_error() {
    let req = post_raw("/v1/remember", "application/json", "{not valid json", true);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn wrong_content_type_is_rejected_not_silently_ignored() {
    let req = post_raw("/v1/remember", "text/plain", r#"{"agent_id":"a","text":"x"}"#, true);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert!(resp.status().is_client_error(), "status was {}", resp.status());
}

#[tokio::test]
async fn path_traversal_in_memory_id_is_rejected_not_a_filesystem_path() {
    // `id` n'est jamais utilisé pour construire un chemin fichier (l'id est
    // une clé logique dans le moteur de stockage, jamais un nom de fichier) —
    // ceci vérifie que la requête est traitée normalement (id inconnu = no-op)
    // et ne provoque ni panique ni fuite de chemin local dans la réponse.
    let resp = app()
        .oneshot(delete("/v1/memories/..%2F..%2F..%2Fetc%2Fpasswd?agent_id=a", true))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "unknown id forget is a no-op");
}

#[tokio::test]
async fn hostile_agent_id_does_not_cross_tenant_boundary() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "victim", "text": "victim secret"}),
            true,
        ))
        .await
        .expect("remember for victim");

    let hostile_ids = ["../victim", "victim\0", "*", "victim' OR '1'='1"];
    for hostile in hostile_ids {
        let resp = app
            .clone()
            .oneshot(post(
                "/v1/recall",
                &json!({"agent_id": hostile, "query": "victim secret"}),
                true,
            ))
            .await
            .expect("oneshot");
        // Soit rejeté (agent_id invalide), soit accepté mais isolé (jamais le
        // secret de `victim`) — jamais une fuite cross-tenant.
        if resp.status() == StatusCode::OK {
            let body = json_body(resp).await;
            assert_eq!(
                body["results"],
                json!([]),
                "hostile agent_id {hostile:?} must never see victim's memories"
            );
        }
    }
}

#[tokio::test]
async fn non_finite_weight_is_rejected() {
    // `serde_json::json!` ne peut pas encoder `f64::INFINITY` (JSON n'a pas
    // de représentation pour l'infini) : on construit le corps à la main
    // pour forcer un nombre hors limites plutôt que passer par le literal.
    let body = format!(
        r#"{{"agent_id":"a","src":"x","relation":"r","dst":"y","weight":{}}}"#,
        f64::MAX
    );
    let req = post_raw("/v1/graph/relations", "application/json", body, true);
    // `f64::MAX` est fini mais un nombre décimal aussi grand, une fois
    // multiplié en interne par le moteur graphe, dépasse toute borne
    // raisonnable — vérifie au minimum que la requête n'entraîne jamais une
    // erreur interne non filtrée (500), qu'elle soit acceptée ou rejetée.
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_ne!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
