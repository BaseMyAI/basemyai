// SPDX-License-Identifier: BUSL-1.1
//! `basemyai config key` — génération et diagnostic de la passphrase (ADR-034).

use basemyai_core::{EncryptionKey, KeyResolveError, KeySource, key_source_label};

use crate::cli::ConfigKeyAction;
use crate::error::CliError;
use crate::output::Format;
use crate::ui::render;

pub(crate) fn run(action: ConfigKeyAction, format: Format) -> Result<(), CliError> {
    match action {
        ConfigKeyAction::Generate { force } => generate(force, format),
        ConfigKeyAction::Path => path(format),
        ConfigKeyAction::Check => check(format),
    }
}

fn generate(force: bool, format: Format) -> Result<(), CliError> {
    let generated_key = EncryptionKey::generate_passphrase();
    let path = EncryptionKey::persist_to_default_file(&generated_key, force).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            CliError::KeyResolution(e.to_string())
        } else {
            CliError::Io(e)
        }
    })?;
    format.print(
        || {
            render::success(&format!("encryption key written to {}", path.display()));
            render::hint(
                "back up this file securely — losing the key means permanent loss of access to \
                 encrypted .bmai containers",
            );
            render::hint("the key value is never printed; store ~/.basemyai/key offline");
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "backup_required": true,
            })
        },
    );
    Ok(())
}

fn path(format: Format) -> Result<(), CliError> {
    let default = EncryptionKey::default_key_file_path();
    format.print(
        || {
            render::section("Default encryption key file");
            if let Some(path) = &default {
                println!("{}", path.display());
            } else {
                println!("(home directory not resolvable)");
            }
        },
        || {
            serde_json::json!({
                "default_key_file": default.as_ref().map(|p| p.display().to_string()),
            })
        },
    );
    Ok(())
}

fn check(format: Format) -> Result<(), CliError> {
    match EncryptionKey::resolve_with_source(None) {
        Ok(resolved) => {
            format.print(
                || {
                    render::success("encryption key is configured");
                    render::key_values(&[("source:", key_source_label(resolved.source).to_string())]);
                },
                || {
                    serde_json::json!({
                        "ok": true,
                        "source": source_id(resolved.source),
                    })
                },
            );
            Ok(())
        }
        Err(KeyResolveError::Missing(msg)) => {
            format.print(
                || {
                    render::error("encryption key is not configured");
                    render::hint(&msg);
                },
                || {
                    serde_json::json!({
                        "ok": false,
                        "code": "KEY_REQUIRED",
                        "message": msg,
                    })
                },
            );
            Err(CliError::MissingKey(msg))
        }
        Err(other) => {
            let msg = other.to_string();
            format.print(
                || {
                    render::error("encryption key check failed");
                    render::hint(&msg);
                },
                || {
                    serde_json::json!({
                        "ok": false,
                        "code": "KEY_INSECURE",
                        "message": msg,
                    })
                },
            );
            Err(CliError::KeyResolution(msg))
        }
    }
}

fn source_id(source: KeySource) -> &'static str {
    match source {
        KeySource::Explicit => "explicit",
        KeySource::EnvDbKey => "env_db_key",
        KeySource::EnvEncryptionKey => "env_encryption_key",
        KeySource::KeyFileEnv => "env_key_file",
        KeySource::DockerSecret => "docker_secret",
        KeySource::DefaultKeyFile => "default_key_file",
        _ => "unknown",
    }
}
