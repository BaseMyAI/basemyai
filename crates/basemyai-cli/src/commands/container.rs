// SPDX-License-Identifier: BUSL-1.1
//! Cycle de vie du conteneur `.bmai` lui-même (pas de son contenu mémoire) :
//! `init`, `migrate`, `inspect`, `verify`. Aucune n'a besoin de l'embedder.

use std::path::Path;

use crate::context::open_store;
use crate::error::CliError;
use crate::output::Format;
use crate::ui::color::Stream;
use crate::ui::theme;

pub(crate) async fn init(path: &Path, format: Format) -> Result<(), CliError> {
    if path.exists() {
        return Err(CliError::AlreadyExists(path.to_path_buf()));
    }
    let store = open_store(path).await?;
    let meta = store.container_metadata().await?;
    let get = |key: &str| meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let format_version = get("format_version").unwrap_or_default();
    format.print(
        || {
            println!("created .bmai container at {}", path.display());
            println!(
                "format_version={format_version}, embedding_dim={}",
                basemyai::EMBEDDING_DIM
            );
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "format_version": format_version,
                "embedding_dim": basemyai::EMBEDDING_DIM,
            })
        },
    );
    Ok(())
}

pub(crate) async fn migrate(path: &Path, format: Format) -> Result<(), CliError> {
    // Le schéma (format.lock, méta de conteneur) est appliqué à l'ouverture —
    // `open_store` suffit à "migrer" (idempotent par construction).
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Applying container migrations...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    open_store(path).await?;
    spinner.finish_and_clear();
    format.print(
        || crate::ui::render::success(&format!("migrations applied (idempotent) on {}", path.display())),
        || serde_json::json!({ "path": path.display().to_string(), "status": "migrated" }),
    );
    Ok(())
}

pub(crate) async fn inspect(path: &Path, format: Format) -> Result<(), CliError> {
    let store = open_store(path).await?;
    let meta = store.container_metadata().await?;
    let total = i64::try_from(store.total_memory_count().await?).unwrap_or(i64::MAX);
    format.print(
        || {
            crate::ui::render::section(&format!("Container metadata ({})", path.display()));
            crate::ui::table::print_table(
                &["Key", "Value"],
                meta.iter().map(|(k, v)| vec![k.clone(), v.clone()]).collect::<Vec<_>>(),
            );
            crate::ui::render::key_values(&[("total_memories:", total.to_string())]);
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "metadata": meta.iter().cloned().collect::<std::collections::BTreeMap<String, String>>(),
                "total_memories": total,
            })
        },
    );
    Ok(())
}

pub(crate) async fn verify(path: &Path, format: Format) -> Result<(), CliError> {
    let store = open_store(path).await?;
    let meta = store.container_metadata().await?;
    let get = |key: &str| meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    let format_field = get("format");
    let version = get("format_version");
    let engine = get("storage_engine");

    let expected_version = basemyai::storage::BMAI_FORMAT_VERSION.to_string();

    let format_ok = format_field.as_deref() == Some("basemyai-memory");
    let version_ok = version.as_deref() == Some(expected_version.as_str());
    let engine_ok = engine.is_some();
    let ok = format_ok && version_ok && engine_ok;

    format.print(
        || {
            let checks = vec![
                (
                    "format".to_string(),
                    if format_ok {
                        format!(
                            "{} basemyai-memory",
                            theme::success(&theme::ok_mark(Stream::Stdout), Stream::Stdout)
                        )
                    } else {
                        format!(
                            "{} expected 'basemyai-memory', got {:?}",
                            theme::error(&theme::fail_mark(Stream::Stdout), Stream::Stdout),
                            format_field.as_deref()
                        )
                    },
                ),
                (
                    "format_version".to_string(),
                    if version_ok {
                        format!(
                            "{} {}",
                            theme::success(&theme::ok_mark(Stream::Stdout), Stream::Stdout),
                            version.as_deref().unwrap_or_default()
                        )
                    } else {
                        format!(
                            "{} expected '{expected_version}', got {:?}",
                            theme::error(&theme::fail_mark(Stream::Stdout), Stream::Stdout),
                            version.as_deref()
                        )
                    },
                ),
                (
                    "storage_engine".to_string(),
                    match engine.as_deref() {
                        Some(e) => format!(
                            "{} {e}",
                            theme::success(&theme::ok_mark(Stream::Stdout), Stream::Stdout)
                        ),
                        None => format!(
                            "{} missing",
                            theme::error(&theme::fail_mark(Stream::Stdout), Stream::Stdout)
                        ),
                    },
                ),
            ];
            crate::ui::render::section("Verification checks");
            crate::ui::table::print_table(
                &["Check", "Result"],
                checks
                    .into_iter()
                    .map(|(check, result)| vec![check, result])
                    .collect::<Vec<_>>(),
            );
            if ok {
                crate::ui::render::success(&format!("{} is a valid .bmai container", path.display()));
            } else {
                crate::ui::render::hint("run `basemyai inspect` to inspect container metadata");
            }
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "checks": {
                    "format": format_field,
                    "format_version": version,
                    "storage_engine": engine,
                },
                "valid": ok,
            })
        },
    );

    if ok { Ok(()) } else { Err(CliError::VerificationFailed) }
}
