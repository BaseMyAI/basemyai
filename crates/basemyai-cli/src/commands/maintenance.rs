// SPDX-License-Identifier: BUSL-1.1
//! Consolidation (`consolidate`) : épisodes → faits + graphe, via
//! `basemyai::consolidate` — pas de nouvelle logique métier ici.
//!
//! `forget-adaptive` : oubli adaptatif (ADR-012 §4), porté sur le moteur
//! natif par ADR-037 — scan applicatif au lieu du `ROW_NUMBER() OVER` SQL
//! retiré par ADR-033. Recâblée ici comme passe manuelle ponctuelle ; la même
//! politique tourne en tâche de fond via `basemyai::AdaptiveForgettingTask`
//! (voir `crates/basemyai/tests/maintenance_worker.rs`) pour les surfaces qui
//! font tourner un `MaintenanceWorker` en continu (CLI = one-shot, pas de
//! worker de fond).
//!
//! `gc` : GC temporel (`valid_until <= now`), porté sur le moteur natif par
//! ADR-038 — même discipline (scan applicatif paginé au lieu d'un `DELETE`
//! SQL fenêtré), même tâche de fond équivalente
//! (`basemyai::ExpiredMemoryGcTask`).
//!
//! Ni `forget-adaptive` ni `gc` ne font le moindre embedding : les deux
//! passent par `open_engine` (store nu, `Arc<dyn MemoryStore>`), jamais par
//! `open_memory` — pas de chargement Candle pour des opérations purement
//! temporelles/de capacité (même raisonnement que `list`/`forget`/
//! `invalidate`/`purge`).

use std::path::Path;

use basemyai::AdaptiveForgettingPolicy;

use crate::context::{open_engine, open_memory};
use crate::error::CliError;
use crate::output::Format;

pub(crate) async fn consolidate(path: &Path, agent: &str, format: Format) -> Result<(), CliError> {
    let memory = open_memory(path, agent).await?;
    let provision = basemyai::choose_llm()
        .await
        .map_err(|e| CliError::LlmNotAvailable(e.to_string()))?;
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Consolidating episodes with local LLM...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    let report = basemyai::consolidate(&memory, provision.backend.as_ref()).await?;
    spinner.finish_and_clear();
    format.print(
        || {
            crate::ui::render::section(&format!("Consolidation via {}", provision.model_id));
            crate::ui::table::print_table(
                &["Metric", "Value"],
                vec![
                    vec!["episodes_seen".to_string(), report.episodes_seen.to_string()],
                    vec!["facts_added".to_string(), report.facts_added.to_string()],
                    vec!["facts_skipped".to_string(), report.facts_skipped.to_string()],
                    vec!["entities_upserted".to_string(), report.entities_upserted.to_string()],
                    vec!["relations_upserted".to_string(), report.relations_upserted.to_string()],
                ],
            );
        },
        || {
            serde_json::json!({
                "model_id": provision.model_id,
                "episodes_seen": report.episodes_seen,
                "facts_added": report.facts_added,
                "facts_skipped": report.facts_skipped,
                "entities_upserted": report.entities_upserted,
                "relations_upserted": report.relations_upserted,
            })
        },
    );
    Ok(())
}

pub(crate) async fn forget_adaptive(
    path: &Path,
    agent: &str,
    capacity: usize,
    half_life_secs: i64,
    dry_run: bool,
    format: Format,
) -> Result<(), CliError> {
    let (store, agent_id) = open_engine(path, agent).await?;
    let policy = AdaptiveForgettingPolicy {
        capacity,
        recency_half_life_secs: half_life_secs,
    };
    let report = basemyai::maintenance::run_adaptive_forget(&store, &agent_id, policy, dry_run).await?;
    format.print(
        || {
            crate::ui::render::section(if dry_run {
                "Adaptive forgetting (dry run)"
            } else {
                "Adaptive forgetting"
            });
            crate::ui::table::print_table(
                &["Metric", "Value"],
                vec![
                    vec!["scanned".to_string(), report.scanned.to_string()],
                    vec![
                        if dry_run { "would_evict" } else { "evicted" }.to_string(),
                        report.evicted.to_string(),
                    ],
                    vec!["capacity".to_string(), capacity.to_string()],
                ],
            );
        },
        || {
            serde_json::json!({
                "dry_run": dry_run,
                "scanned": report.scanned,
                "evicted": report.evicted,
                "capacity": capacity,
                "recency_half_life_secs": half_life_secs,
            })
        },
    );
    Ok(())
}

pub(crate) async fn gc(
    path: &Path,
    agent: &str,
    page_size: usize,
    dry_run: bool,
    format: Format,
) -> Result<(), CliError> {
    let (store, agent_id) = open_engine(path, agent).await?;
    let report = basemyai::maintenance::run_expired_gc(&store, &agent_id, page_size, dry_run).await?;
    format.print(
        || {
            crate::ui::render::section(if dry_run {
                "Expired memory GC (dry run)"
            } else {
                "Expired memory GC"
            });
            crate::ui::table::print_table(
                &["Metric", "Value"],
                vec![
                    vec!["examined".to_string(), report.examined.to_string()],
                    vec![
                        if dry_run { "would_delete" } else { "deleted" }.to_string(),
                        report.deleted.to_string(),
                    ],
                    vec!["pages".to_string(), report.pages.to_string()],
                    vec!["page_size".to_string(), page_size.to_string()],
                ],
            );
        },
        || {
            serde_json::json!({
                "dry_run": dry_run,
                "examined": report.examined,
                "deleted": report.deleted,
                "pages": report.pages,
                "page_size": page_size,
            })
        },
    );
    Ok(())
}
