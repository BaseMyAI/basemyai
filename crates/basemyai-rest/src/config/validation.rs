// SPDX-License-Identifier: BUSL-1.1
//! Invariants de configuration vérifiés au démarrage, avant toute ouverture
//! de store ou tout bind réseau.

use super::{RuntimeConfig, StartupConfig};
use crate::http::RestError;

/// Refuse le mode dev hors boucle locale (sécurité par défaut) et l'absence
/// de clé API hors mode dev.
///
/// # Errors
/// [`RestError::Config`] si `dev=true` avec une adresse non-loopback, ou si
/// ni `dev` ni une clé API ne sont configurés.
pub fn validate(startup: &StartupConfig, runtime: &RuntimeConfig) -> Result<(), RestError> {
    if runtime.dev && !startup.bind.is_loopback() {
        return Err(RestError::Config(
            "BASEMYAI_REST_DEV=1 is only allowed with a loopback bind address \
             (127.0.0.1 or ::1); refusing to start without authentication on a \
             non-loopback interface"
                .to_string(),
        ));
    }
    if !runtime.dev && runtime.api_key.is_none() {
        return Err(RestError::Config(
            "no API key configured: set BASEMYAI_REST_API_KEY, or run with \
             BASEMYAI_REST_DEV=1 for a localhost-only dev server"
                .to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn dev_mode_rejects_non_loopback_bind() {
        let startup = StartupConfig {
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ..StartupConfig::default()
        };
        let runtime = RuntimeConfig {
            dev: true,
            ..RuntimeConfig::default()
        };
        let err = validate(&startup, &runtime).expect_err("must reject non-loopback dev bind");
        assert!(matches!(err, RestError::Config(_)));
    }

    #[test]
    fn missing_api_key_outside_dev_is_rejected() {
        let startup = StartupConfig::default();
        let runtime = RuntimeConfig::default();
        let err = validate(&startup, &runtime).expect_err("must reject missing api key");
        assert!(matches!(err, RestError::Config(_)));
    }

    #[test]
    fn dev_mode_on_loopback_is_accepted() {
        let startup = StartupConfig::default();
        let runtime = RuntimeConfig {
            dev: true,
            ..RuntimeConfig::default()
        };
        assert!(validate(&startup, &runtime).is_ok());
    }
}
