//! Aucun secret (clé API, clé de chiffrement) ne doit apparaître dans une
//! sortie `Debug` — la seule voie par laquelle un secret fuiterait dans des
//! logs (`tracing::debug!(?config, ...)`, un `panic!` qui affiche une
//! structure, etc.).

use basemyai_core::EncryptionKey;
use basemyai_rest::{ApiKey, StartupConfig};

const SECRET: &str = "super-secret-value-that-must-never-leak";

#[test]
fn api_key_debug_output_never_contains_the_secret() {
    let key = ApiKey::new(SECRET);
    let debug = format!("{key:?}");
    assert!(!debug.contains(SECRET), "ApiKey Debug leaked the secret: {debug}");
    assert_eq!(key.expose(), SECRET, "expose() must still return the real value");
}

#[test]
fn startup_config_debug_output_never_contains_the_raw_key() {
    let config = StartupConfig {
        db_key: Some(EncryptionKey::raw(SECRET)),
        ..StartupConfig::default()
    };
    let debug = format!("{config:?}");
    assert!(
        !debug.contains(SECRET),
        "StartupConfig Debug leaked the encryption key: {debug}"
    );
}
