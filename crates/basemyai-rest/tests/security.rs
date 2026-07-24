//! Tests de sécurité : entrées malformées, limites de payload, non-fuite de
//! secrets, valeurs par défaut sécurisées.
#![cfg(feature = "test-util")]

#[path = "support/mod.rs"]
mod support;

#[path = "security/bind_defaults.rs"]
mod bind_defaults;
#[path = "security/body_limits.rs"]
mod body_limits;
#[path = "security/malformed_inputs.rs"]
mod malformed_inputs;
#[path = "security/secret_redaction.rs"]
mod secret_redaction;
