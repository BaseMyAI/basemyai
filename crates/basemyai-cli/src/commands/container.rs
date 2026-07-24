// SPDX-License-Identifier: BUSL-1.1
//! Cycle de vie du conteneur `.bmai` lui-même (pas de son contenu mémoire) :
//! `init`, `migrate`, `inspect`, `verify`, `repair`, `rebuild-indexes`,
//! `compact`, `reembed`. Seule `reembed` a besoin de l'embedder (chargé par
//! l'appelant dans `commands::dispatch`, comme `remember`/`recall`).

use std::path::Path;

use basemyai::storage::integrity::{
    self, EngineStats, IntegrityIssue, RebuildReport, ReembedReport, VerifyMode, VerifyReport,
};

use crate::context::{open_store, require_key};
use crate::error::CliError;
use crate::output::Format;
use crate::ui::color::Stream;
use crate::ui::theme;

pub(crate) async fn init(path: &Path, low_memory: bool, format: Format) -> Result<(), CliError> {
    if path.exists() {
        return Err(CliError::AlreadyExists(path.to_path_buf()));
    }
    let store = if low_memory {
        use basemyai::storage::{Argon2idProfile, NativeMemoryStore};

        let key = require_key()?.into_passphrase();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            NativeMemoryStore::open_with_passphrase_and_profile(&path, key.expose(), Argon2idProfile::LowMemory)
        })
        .await
        .map_err(|error| {
            CliError::Core(basemyai_core::CoreError::Storage(format!(
                "ouverture du store natif interrompue : {error}"
            )))
        })??
    } else {
        open_store(path).await?
    };
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

/// Label stable (texte + JSON) pour un [`VerifyMode`] — `#[non_exhaustive]`
/// côté `basemyai-engine`, donc un bras `_` reste nécessaire même si les
/// trois variantes actuelles sont couvertes.
fn mode_label(mode: VerifyMode) -> &'static str {
    match mode {
        VerifyMode::Quick => "quick",
        VerifyMode::FullPhysical => "physical",
        VerifyMode::FullLogical => "logical",
        _ => "unknown",
    }
}

fn check_line(ok: bool, got: Option<&str>, expected: &str) -> String {
    if ok {
        format!(
            "{} {expected}",
            theme::success(&theme::ok_mark(Stream::Stdout), Stream::Stdout)
        )
    } else {
        format!(
            "{} expected {expected:?}, got {got:?}",
            theme::error(&theme::fail_mark(Stream::Stdout), Stream::Stdout)
        )
    }
}

fn print_issues(issues: &[IntegrityIssue]) {
    crate::ui::table::print_table(
        &["Kind", "Path", "Detail"],
        issues
            .iter()
            .map(|i| vec![format!("{:?}", i.kind), i.path.display().to_string(), i.detail.clone()])
            .collect(),
    );
}

fn issues_json(issues: &[IntegrityIssue]) -> serde_json::Value {
    serde_json::json!(
        issues
            .iter()
            .map(|i| serde_json::json!({
                "kind": format!("{:?}", i.kind),
                "path": i.path.display().to_string(),
                "detail": i.detail,
            }))
            .collect::<Vec<_>>()
    )
}

fn print_rebuild_report(r: &RebuildReport) {
    crate::ui::render::key_values(&[
        ("memory_mappings_rebuilt:", r.memory_mappings_rebuilt.to_string()),
        ("fts_documents_rebuilt:", r.fts_documents_rebuilt.to_string()),
        ("vector_graph_rebuilt:", r.vector_graph_rebuilt.to_string()),
        ("reembedding_required:", r.reembedding_required.len().to_string()),
    ]);
    if !r.reembedding_required.is_empty() {
        crate::ui::render::hint("some memories lost their vector — run `basemyai reembed` to fix them");
    }
}

fn rebuild_report_json(r: &RebuildReport) -> serde_json::Value {
    serde_json::json!({
        "memory_mappings_rebuilt": r.memory_mappings_rebuilt,
        "fts_documents_rebuilt": r.fts_documents_rebuilt,
        "vector_graph_rebuilt": r.vector_graph_rebuilt,
        "reembedding_required": r.reembedding_required
            .iter()
            .map(|(agent, id)| serde_json::json!({ "agent": agent, "id": id }))
            .collect::<Vec<_>>(),
    })
}

fn stats_json(s: &EngineStats) -> serde_json::Value {
    serde_json::json!({
        "wal_bytes": s.wal_bytes,
        "sst_count": s.sst_count,
        "sst_bytes": s.sst_bytes,
        "tombstone_count": s.tombstone_count,
    })
}

/// Vérifie un `.bmai` : métadonnées de conteneur (format/version/moteur) +
/// audit d'intégrité moteur (ADR-040). `mode` va du plus rapide (`Quick`,
/// défaut) au plus profond (`FullLogical`, `--logical`).
///
/// L'audit moteur tourne **avant** toute ouverture normale du store : un
/// `open` recouvre une queue WAL déchirée, ce qui effacerait exactement
/// l'anomalie qu'un audit `Quick` doit révéler. Les métadonnées de conteneur
/// sont lues après coup, via l'ouverture normale (déjà mutante par ailleurs).
pub(crate) async fn verify(path: &Path, mode: VerifyMode, format: Format) -> Result<(), CliError> {
    let key = require_key()?;
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Auditing store integrity...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    let report: VerifyReport = integrity::verify_container(path, key, mode).await?;
    spinner.finish_and_clear();

    let store = open_store(path).await?;
    let meta = store.container_metadata().await?;
    let get = |key: &str| meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    let format_field = get("format");
    let version = get("format_version");
    let engine_name = get("storage_engine");

    let expected_version = basemyai::storage::BMAI_FORMAT_VERSION.to_string();

    let format_ok = format_field.as_deref() == Some("basemyai-memory");
    let version_ok = version.as_deref() == Some(expected_version.as_str());
    let meta_ok = format_ok && version_ok && engine_name.is_some();
    let ok = meta_ok && report.healthy;

    format.print(
        || {
            crate::ui::render::section(&format!("Container metadata ({})", path.display()));
            crate::ui::table::print_table(
                &["Check", "Result"],
                vec![
                    vec![
                        "format".to_string(),
                        check_line(format_ok, format_field.as_deref(), "basemyai-memory"),
                    ],
                    vec![
                        "format_version".to_string(),
                        check_line(version_ok, version.as_deref(), &expected_version),
                    ],
                    vec![
                        "storage_engine".to_string(),
                        engine_name.clone().unwrap_or_else(|| {
                            format!(
                                "{} missing",
                                theme::error(&theme::fail_mark(Stream::Stdout), Stream::Stdout)
                            )
                        }),
                    ],
                ],
            );

            crate::ui::render::section(&format!("Integrity audit ({})", mode_label(mode)));
            crate::ui::render::key_values(&[
                ("files_checked:", report.files_checked.to_string()),
                ("blocks_checked:", report.blocks_checked.to_string()),
                ("records_checked:", report.records_checked.to_string()),
                ("errors:", report.errors.len().to_string()),
                ("warnings:", report.warnings.len().to_string()),
            ]);
            if !report.errors.is_empty() {
                crate::ui::render::section("Errors");
                print_issues(&report.errors);
            }
            if !report.warnings.is_empty() {
                crate::ui::render::section("Warnings");
                print_issues(&report.warnings);
            }

            if ok {
                crate::ui::render::success(&format!("{} is healthy", path.display()));
            } else if meta_ok {
                crate::ui::render::hint("run `basemyai repair --dry-run` to see what can be fixed automatically");
            } else {
                crate::ui::render::hint("run `basemyai inspect` to inspect container metadata");
            }
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "metadata": {
                    "format": format_field,
                    "format_version": version,
                    "storage_engine": engine_name,
                    "valid": meta_ok,
                },
                "integrity": {
                    "mode": mode_label(mode),
                    "healthy": report.healthy,
                    "files_checked": report.files_checked,
                    "blocks_checked": report.blocks_checked,
                    "records_checked": report.records_checked,
                    "errors": issues_json(&report.errors),
                    "warnings": issues_json(&report.warnings),
                },
                "valid": ok,
            })
        },
    );

    if ok { Ok(()) } else { Err(CliError::VerificationFailed) }
}

/// Audite le conteneur (`FullLogical`) et affiche le plan de réparation des
/// index dérivés. Sans `--dry-run`, applique le plan (`rebuild-indexes`) si
/// aucune donnée primaire n'est à risque ; sinon refuse — jamais de
/// réparation automatique sur des données primaires (ADR-040 §3).
pub(crate) async fn repair(path: &Path, dry_run: bool, format: Format) -> Result<(), CliError> {
    let key = require_key()?;
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Auditing store before planning repairs...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    let report = integrity::verify_container(path, key.clone(), VerifyMode::FullLogical).await?;
    let plan = integrity::plan_repair(&report);
    let can_apply = plan.can_apply_derived_only();

    let applied = if dry_run || !can_apply {
        None
    } else {
        Some(integrity::rebuild_indexes_container(path, key).await?)
    };
    spinner.finish_and_clear();

    format.print(
        || {
            crate::ui::render::section(&format!("Repair plan ({})", path.display()));
            if plan.actions.is_empty() {
                crate::ui::render::success("no derived-index repair needed");
            } else {
                crate::ui::table::print_table(
                    &["Action"],
                    plan.actions.iter().map(|a| vec![format!("{a:?}")]).collect(),
                );
            }
            if !plan.primary_data_at_risk.is_empty() {
                crate::ui::render::section("Primary data at risk (never auto-repaired)");
                print_issues(&plan.primary_data_at_risk);
                crate::ui::render::hint(
                    "restore from a trusted `basemyai export` backup — this engine never rewrites primary records",
                );
            }
            if !plan.warnings.is_empty() {
                crate::ui::render::section("Warnings (self-healing at the next open/search)");
                print_issues(&plan.warnings);
            }
            match &applied {
                Some(rebuilt) => {
                    crate::ui::render::section("Applied");
                    print_rebuild_report(rebuilt);
                }
                None if dry_run => crate::ui::render::hint("dry run: nothing was written"),
                None => crate::ui::render::warning("primary data is at risk — refusing to auto-apply"),
            }
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "dry_run": dry_run,
                "actions": plan.actions.iter().map(|a| format!("{a:?}")).collect::<Vec<_>>(),
                "primary_data_at_risk": issues_json(&plan.primary_data_at_risk),
                "warnings": issues_json(&plan.warnings),
                "applied": applied.as_ref().map(rebuild_report_json),
            })
        },
    );

    if !dry_run && !can_apply {
        return Err(CliError::RepairRefused);
    }
    Ok(())
}

/// Reconstruit sans condition les index dérivés depuis les souvenirs
/// primaires (ADR-040 §3) — pas d'audit préalable, contrairement à `repair`.
pub(crate) async fn rebuild_indexes(path: &Path, format: Format) -> Result<(), CliError> {
    let key = require_key()?;
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Rebuilding derived indexes...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    let report = integrity::rebuild_indexes_container(path, key).await?;
    spinner.finish_and_clear();

    format.print(
        || {
            crate::ui::render::section(&format!("Rebuild indexes ({})", path.display()));
            print_rebuild_report(&report);
            crate::ui::render::success("derived indexes rebuilt");
        },
        || rebuild_report_json(&report),
    );
    Ok(())
}

/// Compacte le store : fusion complète en un seul SST, tombstones purgés.
pub(crate) async fn compact(path: &Path, format: Format) -> Result<(), CliError> {
    let key = require_key()?;
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Compacting store...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    let (before, after) = integrity::compact_container(path, key).await?;
    spinner.finish_and_clear();

    format.print(
        || {
            crate::ui::render::section(&format!("Compact ({})", path.display()));
            crate::ui::table::print_table(
                &["Metric", "Before", "After"],
                vec![
                    vec![
                        "sst_count".to_string(),
                        before.sst_count.to_string(),
                        after.sst_count.to_string(),
                    ],
                    vec![
                        "sst_bytes".to_string(),
                        before.sst_bytes.to_string(),
                        after.sst_bytes.to_string(),
                    ],
                    vec![
                        "tombstone_count".to_string(),
                        before.tombstone_count.to_string(),
                        after.tombstone_count.to_string(),
                    ],
                    vec![
                        "wal_bytes".to_string(),
                        before.wal_bytes.to_string(),
                        after.wal_bytes.to_string(),
                    ],
                ],
            );
            crate::ui::render::success("compaction complete");
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "before": stats_json(&before),
                "after": stats_json(&after),
            })
        },
    );
    Ok(())
}

fn print_reembed_report(r: &ReembedReport) {
    crate::ui::render::key_values(&[
        ("reembedded:", r.reembedded.to_string()),
        ("missing:", r.missing.len().to_string()),
    ]);
    if !r.missing.is_empty() {
        crate::ui::render::hint("some requested ids no longer exist (forgotten meanwhile) — skipped, not an error");
    }
}

fn reembed_report_json(r: &ReembedReport) -> serde_json::Value {
    serde_json::json!({
        "reembedded": r.reembedded,
        "missing": r.missing
            .iter()
            .map(|(agent, id)| serde_json::json!({ "agent": agent, "id": id }))
            .collect::<Vec<_>>(),
    })
}

/// Réembed chaque souvenir que le conteneur signale actuellement comme
/// ayant perdu son vecteur (relance `rebuild-indexes` en interne pour une
/// liste à jour) — portée : tout le conteneur, tous agents confondus.
pub(crate) async fn reembed_missing(
    path: &Path,
    embedder: Box<dyn basemyai_core::Embedder>,
    format: Format,
) -> Result<(), CliError> {
    let key = require_key()?;
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Reembedding memories with a missing vector...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    let report = integrity::reembed_missing_container(path, key, embedder).await?;
    spinner.finish_and_clear();

    format.print(
        || {
            crate::ui::render::section(&format!("Reembed ({})", path.display()));
            print_reembed_report(&report);
            crate::ui::render::success("reembedding complete");
        },
        || reembed_report_json(&report),
    );
    Ok(())
}

/// Réembed sans condition les souvenirs de `agent` — soit une liste précise
/// (`ids`), soit tous (`all`) — même s'ils ont déjà un vecteur vivant (ex.
/// changement de modèle d'embedding).
pub(crate) async fn reembed_scoped(
    path: &Path,
    agent: &str,
    all: bool,
    ids: Vec<String>,
    embedder: Box<dyn basemyai_core::Embedder>,
    format: Format,
) -> Result<(), CliError> {
    let key = require_key()?;
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Reembedding memories...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    let report = if all {
        integrity::reembed_all_container(path, key, agent, embedder).await?
    } else {
        integrity::reembed_ids_container(path, key, agent, ids, embedder).await?
    };
    spinner.finish_and_clear();

    format.print(
        || {
            crate::ui::render::section(&format!("Reembed ({}, agent '{agent}')", path.display()));
            print_reembed_report(&report);
            crate::ui::render::success("reembedding complete");
        },
        || reembed_report_json(&report),
    );
    Ok(())
}

/// Re-scelle la DEK du conteneur sous une nouvelle passphrase (ADR-030).
pub(crate) async fn rotate_key(
    path: &Path,
    new_key: Option<String>,
    passphrase: bool,
    low_memory: bool,
    full: bool,
    format: Format,
) -> Result<(), CliError> {
    use basemyai_core::{EncryptionKey, KeyResolveError};

    let current_key = crate::context::require_key()?;
    let current_mode = current_key.mode();
    let mut new_key = match new_key {
        Some(k) => EncryptionKey::new(k),
        None => EncryptionKey::resolve(None).map_err(|e| match e {
            KeyResolveError::Missing(msg) => CliError::MissingKey(msg),
            other => CliError::KeyResolution(other.to_string()),
        })?,
    };
    if passphrase {
        new_key = new_key.into_passphrase();
    }
    let target_mode = new_key.mode();
    let store = crate::context::open_store_with_key(path, current_key).await?;
    if let Some(warning) = rotation_mode_warning(current_mode, target_mode, full) {
        crate::ui::render::warning(warning);
    }
    if low_memory && full {
        store
            .rotate_passphrase_full_with_profile(new_key, basemyai::storage::Argon2idProfile::LowMemory)
            .await?;
    } else if low_memory {
        store
            .rotate_passphrase_with_profile(new_key, basemyai::storage::Argon2idProfile::LowMemory)
            .await?;
    } else if full {
        store.rotate_key_full(new_key).await?;
    } else {
        store.rotate_with_key(new_key).await?;
    }
    format.print(
        || {
            let kind = if full {
                "full encryption key rotation"
            } else {
                "encryption key rotation"
            };
            crate::ui::render::success(&format!("{kind} complete for {}", path.display()));
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "rotated": true,
                "full": full,
                "low_memory": low_memory,
            })
        },
    );
    Ok(())
}

fn rotation_mode_warning(
    current: basemyai_core::EncryptionKeyMode,
    target: basemyai_core::EncryptionKeyMode,
    full: bool,
) -> Option<&'static str> {
    (!full && current != target).then_some(
        "changing credential mode without --full only re-wraps the existing DEK; use --full to rotate the DEK and re-encrypt all current data",
    )
}

#[cfg(test)]
mod rotation_tests {
    use basemyai_core::EncryptionKeyMode;

    use super::rotation_mode_warning;

    #[test]
    fn warns_when_rewrap_changes_credential_mode() {
        let warning = rotation_mode_warning(EncryptionKeyMode::RawKey, EncryptionKeyMode::Passphrase, false)
            .expect("mode-changing rewrap must warn");
        assert!(warning.contains("--full"));
        assert!(warning.contains("existing DEK"));
    }

    #[test]
    fn does_not_warn_for_same_mode_or_full_rotation() {
        assert!(rotation_mode_warning(EncryptionKeyMode::RawKey, EncryptionKeyMode::RawKey, false).is_none());
        assert!(rotation_mode_warning(EncryptionKeyMode::Passphrase, EncryptionKeyMode::RawKey, true).is_none());
    }
}
