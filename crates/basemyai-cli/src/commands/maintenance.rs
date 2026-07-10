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
//! GC temporel (`valid_until`) reste hors scope : il reposait lui aussi sur
//! du SQL spécifique à libSQL, retiré du workspace (ADR-032/033), et son
//! portage natif est un item de suivi séparé, non couvert par ADR-037.

use std::path::Path;

use basemyai::AdaptiveForgettingPolicy;

use crate::context::open_memory;
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
    format: Format,
) -> Result<(), CliError> {
    let memory = open_memory(path, agent).await?;
    let policy = AdaptiveForgettingPolicy {
        capacity,
        recency_half_life_secs: half_life_secs,
    };
    let report = memory.adaptive_forget(policy).await?;
    format.print(
        || {
            crate::ui::render::section("Adaptive forgetting");
            crate::ui::table::print_table(
                &["Metric", "Value"],
                vec![
                    vec!["scanned".to_string(), report.scanned.to_string()],
                    vec!["evicted".to_string(), report.evicted.to_string()],
                    vec!["capacity".to_string(), capacity.to_string()],
                ],
            );
        },
        || {
            serde_json::json!({
                "scanned": report.scanned,
                "evicted": report.evicted,
                "capacity": capacity,
                "recency_half_life_secs": half_life_secs,
            })
        },
    );
    Ok(())
}
