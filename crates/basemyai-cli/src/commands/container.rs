// SPDX-License-Identifier: BUSL-1.1
//! Cycle de vie du conteneur `.bmai` lui-même (pas de son contenu mémoire) :
//! `init`, `migrate`, `inspect`, `verify`. Aucune n'a besoin de l'embedder.

use std::path::Path;

use crate::context::open_store;
use crate::error::CliError;
use crate::output::Format;

pub(crate) async fn init(path: &Path, format: Format) -> Result<(), CliError> {
    if path.exists() {
        return Err(CliError::AlreadyExists(path.to_path_buf()));
    }
    let store = open_store(path).await?;
    store.migrate(&basemyai::schema()).await?;
    format.print(
        || {
            println!("created encrypted .bmai container at {}", path.display());
            println!(
                "format_version={}, embedding_dim={}",
                basemyai::BMAI_FORMAT_VERSION,
                basemyai::EMBEDDING_DIM
            );
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "format_version": basemyai::BMAI_FORMAT_VERSION,
                "embedding_dim": basemyai::EMBEDDING_DIM,
            })
        },
    );
    Ok(())
}

pub(crate) async fn migrate(path: &Path, format: Format) -> Result<(), CliError> {
    let store = open_store(path).await?;
    store.migrate(&basemyai::schema()).await?;
    format.print(
        || println!("migrations applied (idempotent) on {}", path.display()),
        || serde_json::json!({ "path": path.display().to_string(), "status": "migrated" }),
    );
    Ok(())
}

pub(crate) async fn inspect(path: &Path, format: Format) -> Result<(), CliError> {
    let store = open_store(path).await?;
    let meta = read_meta(&store).await?;
    let total = count_memories(&store).await?;
    format.print(
        || {
            println!("Container metadata ({}):", path.display());
            for (k, v) in &meta {
                println!("  {k} = {v}");
            }
            println!("Total memory rows (all agents): {total}");
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
    let meta = read_meta(&store).await?;
    let get = |key: &str| meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    let format_field = get("format");
    let version = get("format_version");
    let engine = get("storage_engine");

    let expected_version = basemyai::BMAI_FORMAT_VERSION.to_string();

    let format_ok = format_field.as_deref() == Some("basemyai-memory");
    let version_ok = version.as_deref() == Some(expected_version.as_str());
    let engine_ok = engine.is_some();
    let ok = format_ok && version_ok && engine_ok;

    format.print(
        || {
            if format_ok {
                println!("✓ format: basemyai-memory");
            } else {
                println!(
                    "✗ format: expected 'basemyai-memory', got {:?}",
                    format_field.as_deref()
                );
            }
            if version_ok {
                println!("✓ format_version: {}", version.as_deref().unwrap_or_default());
            } else {
                println!(
                    "✗ format_version: expected '{expected_version}', got {:?}",
                    version.as_deref()
                );
            }
            match engine.as_deref() {
                Some(e) => println!("✓ storage_engine: {e}"),
                None => println!("✗ storage_engine: missing"),
            }
            if ok {
                println!("{} is a valid .bmai container", path.display());
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

/// Lit la table de métadonnées `bmai_meta`.
async fn read_meta(store: &basemyai_core::Store) -> Result<Vec<(String, String)>, CliError> {
    let conn = store.connect();
    let mut rows = conn.query("SELECT key, value FROM bmai_meta ORDER BY key", ()).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let k: String = row.get(0)?;
        let v: String = row.get(1)?;
        out.push((k, v));
    }
    Ok(out)
}

async fn count_memories(store: &basemyai_core::Store) -> Result<i64, CliError> {
    let conn = store.connect();
    let mut rows = conn.query("SELECT COUNT(*) FROM memory", ()).await?;
    let total = match rows.next().await? {
        Some(row) => row.get::<i64>(0)?,
        None => 0,
    };
    Ok(total)
}
