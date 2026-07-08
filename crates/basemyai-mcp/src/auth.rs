// SPDX-License-Identifier: BUSL-1.1
//! Authentification Bearer pour le transport HTTP, en **temps constant**
//! (`subtle::ConstantTimeEq`) pour ne pas fuiter d'information par timing.
//!
//! Implémenté comme un `tower::Layer` : toute requête sans `Authorization:
//! Bearer <clé>` valide reçoit `401` avant d'atteindre le service MCP.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use subtle::ConstantTimeEq;
use tower::{Layer, Service};

/// Couche tower injectant la vérification du jeton Bearer.
#[derive(Clone)]
pub(crate) struct BearerAuthLayer {
    api_key: Arc<str>,
}

impl BearerAuthLayer {
    /// Construit la couche autour de la clé API attendue.
    #[must_use]
    pub(crate) fn new(api_key: String) -> Self {
        Self {
            api_key: Arc::from(api_key),
        }
    }
}

impl<S> Layer<S> for BearerAuthLayer {
    type Service = BearerAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BearerAuthService {
            inner,
            api_key: Arc::clone(&self.api_key),
        }
    }
}

/// Service produit par [`BearerAuthLayer`].
#[derive(Clone)]
pub(crate) struct BearerAuthService<S> {
    inner: S,
    api_key: Arc<str>,
}

impl<S> Service<Request<Body>> for BearerAuthService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        if authorized(&req, &self.api_key) {
            Box::pin(self.inner.call(req))
        } else {
            Box::pin(async { Ok(unauthorized_response()) })
        }
    }
}

/// `true` si l'en-tête `Authorization: Bearer <clé>` correspond, en temps constant.
fn authorized(req: &Request<Body>, api_key: &str) -> bool {
    let Some(value) = req.headers().get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    let Some(token) = text.strip_prefix("Bearer ") else {
        return false;
    };
    // ct_eq renvoie Choice(0) si les longueurs diffèrent ; sinon comparaison
    // octet par octet en temps constant.
    token.as_bytes().ct_eq(api_key.as_bytes()).into()
}

/// Réponse `401` JSON, conforme au schéma d'erreur du sidecar.
fn unauthorized_response() -> Response<Body> {
    let body = Body::from(r#"{"error":{"code":"UNAUTHORIZED","message":"missing or invalid Bearer token"}}"#);
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::WWW_AUTHENTICATE, "Bearer")
        .body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}
