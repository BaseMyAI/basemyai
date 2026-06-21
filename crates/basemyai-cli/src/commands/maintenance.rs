//! Tâches de maintenance en mode one-shot (`maintenance gc`,
//! `maintenance forget-adaptive`) et consolidation (`consolidate`). Chacune
//! appelle une tâche déjà existante (`MaintenanceTask::run`) ou la fonction
//! libre `basemyai::consolidate` — pas de nouvelle logique métier ici.

use std::path::Path;

use basemyai_core::MaintenanceTask as _;

use crate::context::{open_memory, open_store};
use crate::error::CliError;
use crate::output::Format;

pub(crate) async fn gc(path: &Path, format: Format) -> Result<(), CliError> {
    let store = open_store(path).await?;
    basemyai::ExpiredMemoryGc.run(&store).await?;
    format.print(
        || println!("expired-memory GC complete"),
        || serde_json::json!({ "task": "expired-memory-gc" }),
    );
    Ok(())
}

pub(crate) async fn forget_adaptive(
    path: &Path,
    capacity_per_agent: usize,
    half_life_secs: i64,
    format: Format,
) -> Result<(), CliError> {
    let store = open_store(path).await?;
    let task = basemyai::AdaptiveForgetting {
        capacity_per_agent,
        recency_half_life_secs: half_life_secs,
    };
    task.run(&store).await?;
    format.print(
        || println!("adaptive forgetting complete (capacity={capacity_per_agent}, half_life_secs={half_life_secs})"),
        || serde_json::json!({ "task": "adaptive-forgetting", "capacity_per_agent": capacity_per_agent, "half_life_secs": half_life_secs }),
    );
    Ok(())
}

pub(crate) async fn consolidate(path: &Path, agent: &str, format: Format) -> Result<(), CliError> {
    let memory = open_memory(path, agent).await?;
    let provision = basemyai::choose_llm()
        .await
        .map_err(|e| CliError::LlmNotAvailable(e.to_string()))?;
    let report = basemyai::consolidate(&memory, provision.backend.as_ref()).await?;
    format.print(
        || {
            println!(
                "consolidation via {} — episodes_seen={}",
                provision.model_id, report.episodes_seen
            );
            println!(
                "  facts: {} added, {} skipped — graph: {} entities, {} relations upserted",
                report.facts_added, report.facts_skipped, report.entities_upserted, report.relations_upserted
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
