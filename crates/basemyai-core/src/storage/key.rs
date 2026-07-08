// SPDX-License-Identifier: BUSL-1.1
//! Clé de chiffrement générique — indépendante du backend depuis la
//! suppression de libSQL (ADR-032). Utilisée par le moteur natif
//! (`basemyai-engine`, ADR-030) via [`EncryptionKey::expose`].

use std::fmt;

/// Clé de chiffrement, **fournie à l'ouverture, jamais persistée**. `Debug` masqué.
#[derive(Clone)]
pub struct EncryptionKey(String);

impl EncryptionKey {
    /// Wrap une clé de chiffrement. La valeur n'est jamais loguée ni affichée.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// Expose la clé brute — nécessaire pour ouvrir le moteur natif, qui
    /// reçoit la clé en `&str`/`&[u8]` plutôt qu'en ce type. À ne jamais
    /// loguer ni afficher : l'appelant hérite de la responsabilité du
    /// non-affichage.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptionKey(***)")
    }
}
