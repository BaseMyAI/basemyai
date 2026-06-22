//! Commandes de cycle de vie mémoire : `remember`, `recall`, `list`, `forget`,
//! `invalidate`, `purge`, `export`, `import`. Chacune appelle une méthode déjà
//! existante de `basemyai::Memory` — pas de nouvelle logique métier ici.

use std::io::Read as _;
use std::path::Path;

use basemyai::{Memory, Record};

use crate::cli::Layer;
use crate::context::{memory_layer, now_unix, open_engine, open_memory, open_store};
use crate::error::CliError;
use crate::output::Format;

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
            let id = memory.remember(&text, layer).await?;
            format.print(
                || println!("remembered {id} in layer {}", layer.table()),
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
            let ids = memory.remember_batch(&texts, layer).await?;
            format.print(
                || println!("remembered {} item(s) in layer {}", ids.len(), layer.table()),
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
    let records: Vec<Record> = match (hybrid, layer, graph) {
        (true, None, false) => memory.recall_hybrid(query, k).await?,
        (false, Some(l), false) => memory.recall_by_layer(query, memory_layer(l), k).await?,
        (false, None, true) => memory.search_graph(query, k).await?,
        (false, None, false) => memory.recall(query, k).await?,
        _ => {
            return Err(CliError::MutuallyExclusive(
                "--hybrid, --layer and --graph are mutually exclusive",
            ));
        }
    };
    print_records(&records, query, format);
    Ok(())
}

fn print_records(records: &[Record], query: &str, format: Format) {
    format.print(
        || {
            if records.is_empty() {
                println!("(no memories matched)");
            } else {
                println!("{} result(s) for \"{query}\":", records.len());
                for (i, r) in records.iter().enumerate() {
                    println!(
                        "  {}. [{}] [{:.3}] ({}) {}",
                        i + 1,
                        r.id,
                        r.similarity(),
                        r.layer.table(),
                        r.text
                    );
                }
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
    basemyai::AgentId::new(agent).ok_or(CliError::InvalidAgent)?;

    let store = open_store(path).await?;
    let conn = store.connect();

    let mut sql = String::from("SELECT id, layer, content, valid_from, valid_until FROM memory WHERE agent_id = ?1");
    if !include_invalid {
        sql.push_str(" AND (valid_until IS NULL OR valid_until > unixepoch())");
    }
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut params: Vec<basemyai_core::libsql::Value> = vec![
        basemyai_core::libsql::Value::Text(agent.to_string()),
        basemyai_core::libsql::Value::Integer(limit_i64),
    ];
    if let Some(l) = layer {
        sql.push_str(" AND layer = ?3");
        params.push(basemyai_core::libsql::Value::Text(memory_layer(l).table().to_string()));
    }
    sql.push_str(" ORDER BY valid_from DESC LIMIT ?2");

    let mut rows = conn.query(&sql, params).await?;

    let mut items = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let layer: String = row.get(1)?;
        let content: String = row.get(2)?;
        let valid_from: i64 = row.get(3)?;
        let valid_until: Option<i64> = row.get(4)?;
        items.push((id, layer, content, valid_from, valid_until));
    }

    format.print(
        || {
            if items.is_empty() {
                println!("(no memories)");
            } else {
                println!("{} memory(ies) for agent '{agent}':", items.len());
                for (id, layer, content, valid_from, valid_until) in &items {
                    let status = match valid_until {
                        Some(_) => "invalidated",
                        None => "valid",
                    };
                    println!("  [{id}] ({layer}, {status}, since {valid_from}) {content}");
                }
            }
        },
        || {
            serde_json::json!({
                "agent": agent,
                "memories": items.iter().map(|(id, layer, content, valid_from, valid_until)| serde_json::json!({
                    "id": id,
                    "layer": layer,
                    "text": content,
                    "valid_from": valid_from,
                    "valid_until": valid_until,
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
        || println!("forgot {id}"),
        || serde_json::json!({ "id": id, "action": "forget" }),
    );
    Ok(())
}

pub(crate) async fn invalidate(path: &Path, agent: &str, id: &str, format: Format) -> Result<(), CliError> {
    let (engine, agent_id) = open_engine(path, agent).await?;
    engine.invalidate(&agent_id, id, now_unix()).await?;
    format.print(
        || println!("invalidated {id}"),
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
        || println!("purged all data for agent '{agent}'"),
        || serde_json::json!({ "agent": agent, "action": "purge" }),
    );
    Ok(())
}

pub(crate) async fn export(path: &Path, agent: &str, out: Option<String>, format: Format) -> Result<(), CliError> {
    let memory = open_memory(path, agent).await?;
    let jsonl = memory.export_jsonl().await?;
    match out {
        Some(out_path) => {
            std::fs::write(&out_path, &jsonl)?;
            format.print(
                || println!("exported agent '{agent}' to {out_path}"),
                || serde_json::json!({ "agent": agent, "out": out_path }),
            );
        }
        None => print!("{jsonl}"),
    }
    Ok(())
}

pub(crate) async fn import(path: &Path, agent: &str, file: &str, format: Format) -> Result<(), CliError> {
    let memory = open_memory(path, agent).await?;
    let jsonl = read_input(file)?;
    let report = memory.import_jsonl(&jsonl).await?;
    format.print(
        || {
            println!(
                "imported: {} memories ({} skipped), {} entities ({} skipped), {} edges ({} skipped)",
                report.memories,
                report.memories_skipped,
                report.entities,
                report.entities_skipped,
                report.edges,
                report.edges_skipped
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
