// SPDX-License-Identifier: BUSL-1.1
//! Plafond global de taille du corps de requête (`RuntimeConfig::max_body_bytes`).
//! Une limite plus stricte spécifique à l'import est appliquée en handler
//! (`http::extract::validate_import_size`) puisque `import_jsonl_with_options`
//! doit de toute façon charger tout le texte pour parser l'en-tête avant
//! d'écrire quoi que ce soit.

use tower_http::limit::RequestBodyLimitLayer;

#[must_use]
pub fn layer(max_bytes: usize) -> RequestBodyLimitLayer {
    RequestBodyLimitLayer::new(max_bytes)
}
