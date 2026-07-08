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
        || println!("entity '{id}' ({kind}) upserted"),
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
        || println!("edge '{src}' -[{relation}]-> '{dst}' (weight {weight}) upserted"),
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
                println!("(nothing reachable from '{start}' within {depth} hop(s))");
            } else {
                println!("{} entity(ies) reachable from '{start}':", reached.len());
                for r in &reached {
                    println!("  [{}] ({}) {} — depth {}", r.id, r.kind, r.label, r.depth);
                }
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
