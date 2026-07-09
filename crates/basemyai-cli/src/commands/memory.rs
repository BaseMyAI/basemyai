// SPDX-License-Identifier: BUSL-1.1
//! Commandes de cycle de vie mémoire : `remember`, `recall`, `list`, `forget`,
//! `invalidate`, `purge`, `export`, `import`. Chacune appelle une méthode déjà
//! existante de `basemyai::Memory` — pas de nouvelle logique métier ici.

use std::io::Read as _;
use std::path::Path;

use basemyai::{Memory, Record};

use crate::cli::Layer;
use crate::context::{memory_layer, now_unix, open_engine, open_memory};
use crate::error::CliError;
use crate::output::Format;
use crate::ui::color::Stream;
use crate::ui::theme;

pub(crate) async fn remember(
    memory: &Memory,
    layer: Layer,
    text: Option<String>,
    file: Option<String>,
    format: Format,
) -> Result<(), CliError> {
    let layer = memory_layer(layer);
    match (text, file) {
        (Some(_), Some(_)) | (None, None) => {
            unreachable!("clap enforces text/file mutual exclusivity (required_unless_present/conflicts_with)")
        }
        (Some(text), None) => {
            let spinner = if format.is_text() {
                crate::ui::progress::spinner("Embedding and storing memory...")
            } else {
                crate::ui::progress::Spinner::Disabled
            };
            let id = memory.remember(&text, layer).await?;
            spinner.finish_and_clear();
            format.print(
                || crate::ui::render::success(&format!("remembered {id} in layer {}", layer.table())),
                || serde_json::json!({ "id": id, "layer": layer.table() }),
            );
            Ok(())
        }
        (None, Some(file)) => {
            let raw = read_input(&file)?;
            let texts: Vec<String> = raw
                .lines()
                .map(str::to_string)
                .filter(|l| !l.trim().is_empty())
                .collect();
            let spinner = if format.is_text() {
                crate::ui::progress::spinner("Embedding and storing memory batch...")
            } else {
                crate::ui::progress::Spinner::Disabled
            };
            let ids = memory.remember_batch(&texts, layer).await?;
            spinner.finish_and_clear();
            format.print(
                || crate::ui::render::success(&format!("remembered {} item(s) in layer {}", ids.len(), layer.table())),
                || serde_json::json!({ "ids": ids, "layer": layer.table(), "count": ids.len() }),
            );
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn recall(
    memory: &Memory,
    query: &str,
    k: usize,
    hybrid: bool,
    layer: Option<Layer>,
    graph: bool,
    format: Format,
) -> Result<(), CliError> {
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Searching memories...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    let records: Vec<Record> = match (hybrid, layer, graph) {
        (true, None, false) => memory.recall_hybrid(query, k).await?,
        (false, Some(l), false) => memory.recall_by_layer(query, memory_layer(l), k).await?,
        (false, None, true) => memory.search_graph(query, k).await?,
        (false, None, false) => memory.recall(query, k).await?,
        _ => {
            spinner.finish_and_clear();
            return Err(CliError::MutuallyExclusive(
                "--hybrid, --layer and --graph are mutually exclusive",
            ));
        }
    };
    spinner.finish_and_clear();
    print_records(&records, query, format);
    Ok(())
}

fn print_records(records: &[Record], query: &str, format: Format) {
    format.print(
        || {
            if records.is_empty() {
                crate::ui::render::warning("no memories matched the current query");
                crate::ui::render::hint("try a broader query or `--hybrid`");
            } else {
                crate::ui::render::section(&format!(
                    "{} result(s) for {}",
                    records.len(),
                    theme::accent(&format!("\"{query}\""), Stream::Stdout)
                ));
                crate::ui::table::print_table(
                    &["#", "score", "layer", "id", "excerpt"],
                    records
                        .iter()
                        .enumerate()
                        .map(|(i, r)| {
                            vec![
                                (i + 1).to_string(),
                                format!("{:.3}", r.similarity()),
                                theme::layer(r.layer.table(), Stream::Stdout),
                                r.id.clone(),
                                crate::ui::table::wrap_excerpt(&r.text, 72),
                            ]
                        })
                        .collect::<Vec<_>>(),
                );
            }
        },
        || {
            serde_json::json!({
                "query": query,
                "results": records.iter().map(|r| serde_json::json!({
                    "id": r.id,
                    "layer": r.layer.table(),
                    "similarity": r.similarity(),
                    "text": r.text,
                })).collect::<Vec<_>>(),
            })
        },
    );
}

pub(crate) async fn list(
    path: &Path,
    agent: &str,
    layer: Option<Layer>,
    limit: usize,
    include_invalid: bool,
    format: Format,
) -> Result<(), CliError> {
    let (engine, agent_id) = open_engine(path, agent).await?;
    let records = engine
        .list_memories(&agent_id, layer.map(memory_layer), limit, include_invalid, now_unix())
        .await?;

    format.print(
        || {
            if records.is_empty() {
                crate::ui::render::info("no memories found for this agent");
            } else {
                crate::ui::render::section(&format!("{} memory(ies) for agent '{agent}'", records.len()));
                crate::ui::table::print_table(
                    &["id", "layer", "status", "valid_from", "content"],
                    records
                        .iter()
                        .map(|r| {
                            let status = if r.valid_until.is_some() {
                                "invalidated"
                            } else {
                                "valid"
                            };
                            vec![
                                r.id.clone(),
                                theme::layer(r.layer.table(), Stream::Stdout),
                                status.to_string(),
                                r.valid_from.to_string(),
                                crate::ui::table::wrap_excerpt(&r.content, 68),
                            ]
                        })
                        .collect::<Vec<_>>(),
                );
            }
        },
        || {
            serde_json::json!({
                "agent": agent,
                "memories": records.iter().map(|r| serde_json::json!({
                    "id": r.id,
                    "layer": r.layer.table(),
                    "text": r.content,
                    "valid_from": r.valid_from,
                    "valid_until": r.valid_until,
                })).collect::<Vec<_>>(),
            })
        },
    );
    Ok(())
}

pub(crate) async fn forget(path: &Path, agent: &str, id: &str, format: Format) -> Result<(), CliError> {
    let (engine, agent_id) = open_engine(path, agent).await?;
    engine.forget(&agent_id, id).await?;
    format.print(
        || crate::ui::render::success(&format!("forgot {id}")),
        || serde_json::json!({ "id": id, "action": "forget" }),
    );
    Ok(())
}

pub(crate) async fn invalidate(path: &Path, agent: &str, id: &str, format: Format) -> Result<(), CliError> {
    let (engine, agent_id) = open_engine(path, agent).await?;
    engine.invalidate(&agent_id, id, now_unix()).await?;
    format.print(
        || crate::ui::render::success(&format!("invalidated {id}")),
        || serde_json::json!({ "id": id, "action": "invalidate" }),
    );
    Ok(())
}

pub(crate) async fn purge(path: &Path, agent: &str, yes: bool, format: Format) -> Result<(), CliError> {
    if !yes {
        return Err(CliError::ConfirmationRequired(
            "purge is irreversible: re-run with --yes to confirm",
        ));
    }
    let (engine, agent_id) = open_engine(path, agent).await?;
    engine.purge_agent(&agent_id).await?;
    format.print(
        || crate::ui::render::success(&format!("purged all data for agent '{agent}'")),
        || serde_json::json!({ "agent": agent, "action": "purge" }),
    );
    Ok(())
}

pub(crate) async fn export(path: &Path, agent: &str, out: Option<String>, format: Format) -> Result<(), CliError> {
    if out.is_none() && format == Format::Json {
        return Err(CliError::MutuallyExclusive(
            "export writes JSONL to stdout; use --out with --format json so stdout remains one JSON object",
        ));
    }
    let memory = open_memory(path, agent).await?;
    let jsonl = memory.export_jsonl().await?;
    match out {
        Some(out_path) => {
            std::fs::write(&out_path, &jsonl)?;
            format.print(
                || crate::ui::render::success(&format!("exported agent '{agent}' to {out_path}")),
                || serde_json::json!({ "agent": agent, "out": out_path }),
            );
        }
        None => print!("{jsonl}"),
    }
    Ok(())
}

pub(crate) async fn import(
    path: &Path,
    agent: &str,
    file: &str,
    trusted: bool,
    format: Format,
) -> Result<(), CliError> {
    let memory = open_memory(path, agent).await?;
    let jsonl = read_input(file)?;
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Importing JSONL snapshot...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };
    let report = memory.import_jsonl_with_options(&jsonl, trusted).await?;
    spinner.finish_and_clear();
    format.print(
        || {
            crate::ui::render::section("Import report");
            crate::ui::table::print_table(
                &["Metric", "Value"],
                vec![
                    vec!["memories".to_string(), report.memories.to_string()],
                    vec!["memories_skipped".to_string(), report.memories_skipped.to_string()],
                    vec!["entities".to_string(), report.entities.to_string()],
                    vec!["entities_skipped".to_string(), report.entities_skipped.to_string()],
                    vec!["edges".to_string(), report.edges.to_string()],
                    vec!["edges_skipped".to_string(), report.edges_skipped.to_string()],
                ],
            );
        },
        || {
            serde_json::json!({
                "memories": report.memories,
                "memories_skipped": report.memories_skipped,
                "entities": report.entities,
                "entities_skipped": report.entities_skipped,
                "edges": report.edges,
                "edges_skipped": report.edges_skipped,
            })
        },
    );
    Ok(())
}

/// Lit `-` depuis stdin, sinon le fichier au chemin donné.
fn read_input(path: &str) -> Result<String, CliError> {
    if path == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else {
        Ok(std::fs::read_to_string(path)?)
    }
}
