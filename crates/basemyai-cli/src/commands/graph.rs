// SPDX-License-Identifier: BUSL-1.1
//! Commandes graphe entités/relations : `graph add-entity`, `graph add-edge`,
//! `graph traverse`. Chacune appelle une méthode déjà existante de `basemyai::Graph`.

use std::path::Path;

use basemyai::Graph;

use crate::context::open_engine;
use crate::error::CliError;
use crate::output::Format;

pub(crate) async fn add_entity(
    path: &Path,
    agent: &str,
    id: &str,
    kind: &str,
    label: &str,
    format: Format,
) -> Result<(), CliError> {
    let (engine, agent_id) = open_engine(path, agent).await?;
    Graph::new(engine, agent_id).add_entity(id, kind, label).await?;
    format.print(
        || crate::ui::render::success(&format!("entity '{id}' ({kind}) upserted")),
        || serde_json::json!({ "id": id, "kind": kind, "label": label }),
    );
    Ok(())
}

pub(crate) async fn add_edge(
    path: &Path,
    agent: &str,
    src: &str,
    relation: &str,
    dst: &str,
    weight: f64,
    format: Format,
) -> Result<(), CliError> {
    let (engine, agent_id) = open_engine(path, agent).await?;
    Graph::new(engine, agent_id)
        .add_edge(src, relation, dst, weight)
        .await?;
    format.print(
        || {
            crate::ui::render::success(&format!(
                "edge '{src}' -[{relation}]-> '{dst}' (weight {weight}) upserted"
            ))
        },
        || serde_json::json!({ "src": src, "relation": relation, "dst": dst, "weight": weight }),
    );
    Ok(())
}

pub(crate) async fn traverse(
    path: &Path,
    agent: &str,
    start: &str,
    depth: u32,
    format: Format,
) -> Result<(), CliError> {
    let (engine, agent_id) = open_engine(path, agent).await?;
    let reached = Graph::new(engine, agent_id).traverse(start, depth).await?;
    format.print(
        || {
            if reached.is_empty() {
                crate::ui::render::info(&format!("no entities reachable from '{start}' within {depth} hop(s)"));
                crate::ui::render::hint("add graph edges with `basemyai graph add-edge ...`");
            } else {
                crate::ui::render::section(&format!("{} entity(ies) reachable from '{start}'", reached.len()));
                crate::ui::table::print_table(
                    &["id", "kind", "label", "depth"],
                    reached
                        .iter()
                        .map(|r| vec![r.id.clone(), r.kind.clone(), r.label.clone(), r.depth.to_string()])
                        .collect::<Vec<_>>(),
                );
            }
        },
        || {
            serde_json::json!({
                "start": start,
                "reached": reached.iter().map(|r| serde_json::json!({
                    "id": r.id,
                    "kind": r.kind,
                    "label": r.label,
                    "depth": r.depth,
                })).collect::<Vec<_>>(),
            })
        },
    );
    Ok(())
}
