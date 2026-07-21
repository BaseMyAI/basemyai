//! Construction de requêtes `http::Request` et lecture de corps JSON — les
//! deux briques répétées par tous les tests contract/integration/security.

use axum::body::Body;
use axum::http::{Request, header};
use serde_json::Value;

use super::app::KEY;

pub(crate) fn post(uri: &str, body: &Value, auth: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if auth {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {KEY}"));
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

/// Comme [`post`], mais avec un corps brut (pas nécessairement du JSON valide) —
/// pour les tests de payload malformé/`Content-Type` incorrect.
pub(crate) fn post_raw(uri: &str, content_type: &str, body: impl Into<Body>, auth: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type);
    if auth {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {KEY}"));
    }
    builder.body(body.into()).expect("request")
}

pub(crate) fn get(uri: &str, auth: bool) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if auth {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {KEY}"));
    }
    builder.body(Body::empty()).expect("request")
}

pub(crate) fn delete(uri: &str, auth: bool) -> Request<Body> {
    let mut builder = Request::builder().method("DELETE").uri(uri);
    if auth {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {KEY}"));
    }
    builder.body(Body::empty()).expect("request")
}

pub(crate) async fn json_body(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("collect body");
    serde_json::from_slice(&bytes).expect("json")
}
