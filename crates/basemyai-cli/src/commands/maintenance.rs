// SPDX-License-Identifier: BUSL-1.1
//! Consolidation (`consolidate`) : épisodes → faits + graphe, via
//! `basemyai::consolidate` — pas de nouvelle logique métier ici.
//!
//! GC temporel et oubli adaptatif reposaient sur du SQL de fenêtrage
//! spécifique à libSQL, retiré du workspace (ADR-032) — supprimés avec lui
//! plutôt que portés en passant sur le moteur natif (un portage mérite son
//! propre design/tests, item de suivi séparé).

use std::path::Path;

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
