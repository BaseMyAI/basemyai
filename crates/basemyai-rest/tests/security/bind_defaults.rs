//! Le serveur est sécurisé par défaut : boucle locale, auth exigée hors mode
//! `dev`, mode `dev` refusé hors boucle locale.

use std::net::{IpAddr, Ipv4Addr};

use basemyai_rest::{ApiKey, RuntimeConfig, StartupConfig};

#[test]
fn default_startup_config_binds_loopback() {
    let config = StartupConfig::default();
    assert!(
        config.bind.is_loopback(),
        "default bind must be loopback, got {}",
        config.bind
    );
}

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
    assert!(basemyai_rest::config::validate(&startup, &runtime).is_err());
}

#[test]
fn missing_api_key_outside_dev_mode_is_rejected() {
    let startup = StartupConfig::default();
    let runtime = RuntimeConfig::default();
    assert!(basemyai_rest::config::validate(&startup, &runtime).is_err());
}

#[test]
fn dev_mode_on_loopback_with_no_api_key_is_accepted() {
    let startup = StartupConfig::default();
    let runtime = RuntimeConfig {
        dev: true,
        ..RuntimeConfig::default()
    };
    assert!(basemyai_rest::config::validate(&startup, &runtime).is_ok());
}

#[test]
fn api_key_present_outside_dev_on_loopback_is_accepted() {
    let startup = StartupConfig::default();
    let runtime = RuntimeConfig {
        api_key: Some(ApiKey::new("k")),
        ..RuntimeConfig::default()
    };
    assert!(basemyai_rest::config::validate(&startup, &runtime).is_ok());
}
