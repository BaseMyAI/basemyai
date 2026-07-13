// SPDX-License-Identifier: BUSL-1.1
//! Export/import JSONL (ADR-032) : lignes brutes du moteur natif, fidélité
//! totale (`importance`/`last_access` préservés, contrairement à
//! `put_memory_batch` qui applique les défauts d'un souvenir neuf).

use basemyai_engine::NewMemoryRecord;

use super::{NativeInner, NativeMemoryStore, storage};
use crate::{AgentId, MemoryLayer, Result};

/// Lignes brutes d'un export natif ([`NativeMemoryStore::export_rows`]) —
/// tout ce qui appartient à un agent, dans l'ordre de sérialisation JSONL.
pub struct NativeExportRows {
    pub memories: Vec<(String, basemyai_engine::MemoryRecord)>,
    pub entities: Vec<(String, basemyai_engine::GraphEntity)>,
    pub edges: Vec<(String, String, String, basemyai_engine::GraphEdgeMeta)>,
}

/// Un souvenir complet à importer ([`NativeMemoryStore::import_rows`]) —
/// fidélité totale (`importance`/`last_access`), vecteur déjà recalculé par
/// l'embedder de la mémoire cible.
pub(crate) struct NativeImportMemory {
    pub id: String,
    pub layer: MemoryLayer,
    pub content: String,
    pub source: String,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
    pub importance: f64,
    pub last_access: Option<i64>,
    pub vector: Vec<f32>,
}

/// Une entité de graphe à importer. Pas d'équivalent `importance` dans
/// `GraphEntity:1` — champ jamais écrit par le contrat `MemoryStore`, perdu
/// à l'import natif (écart documenté, ADR-033 §3).
pub(crate) struct NativeImportEntity {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
}

/// Une arête de graphe à importer, méta complète de l'export préservée.
pub(crate) struct NativeImportEdge {
    pub src: String,
    pub dst: String,
    pub relation: String,
    pub weight: f64,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
}

impl NativeMemoryStore {
    /// Tout ce qui appartient à `agent`, en lignes brutes du moteur — la
    /// brique de l'export JSONL (ADR-032). Les tris reproduisent les
    /// Tri déterministe pour l'export JSONL (souvenirs par `(valid_from, id)`,
    /// entités par `id`, arêtes par `(src, dst, relation)`) pour qu'un même
    /// contenu produise un export identique octet pour octet quel que soit
    /// le backend.
    pub async fn export_rows(&self, agent: &AgentId) -> Result<NativeExportRows> {
        let agent = agent.clone();
        self.with_inner_read(move |inner| {
            let NativeInner {
                engine, memory, graph, ..
            } = inner;
            let mut memories = memory.scan_agent(engine, agent.as_str()).map_err(storage)?;
            memories.sort_by(|(a_id, a), (b_id, b)| (a.valid_from, a_id).cmp(&(b.valid_from, b_id)));
            // Le scan structurel rend déjà les entités par id ascendant
            // (ordre des octets de clé) — équivalent du `ORDER BY id`.
            let entities = graph.entities(engine, agent.as_str()).map_err(storage)?;
            let mut edges = graph.edges(engine, agent.as_str()).map_err(storage)?;
            edges.sort_by(|(a_src, a_rel, a_dst, _), (b_src, b_rel, b_dst, _)| {
                (a_src, a_dst, a_rel).cmp(&(b_src, b_dst, b_rel))
            });
            Ok(NativeExportRows {
                memories,
                entities,
                edges,
            })
        })
        .await
    }

    /// Import idempotent de lignes complètes (ADR-032) : les souvenirs
    /// nouveaux partent en **un seul** batch WAL tout-ou-rien
    /// (`put_many`, N5.5) avec leur fidélité complète
    /// (`importance`/`last_access` préservés — contrairement à
    /// `put_memory_batch` qui applique les défauts d'un souvenir neuf) ;
    /// les ids déjà présents (dans le store **ou** plus haut dans le même
    /// fichier) sont comptés `*_skipped` et laissés intacts — la sémantique
    /// Sémantique insert-or-ignore sur la méta consommateur. Entités et arêtes suivent en
    /// upserts individuels durables : l'import natif est **idempotent et
    /// reprennable**, pas globalement atomique (écart assumé, ADR-032 §3 —
    /// même classe que `purge_agent`, ADR-027 §6).
    pub(crate) async fn import_rows(
        &self,
        agent: &AgentId,
        memories: Vec<NativeImportMemory>,
        entities: Vec<NativeImportEntity>,
        edges: Vec<NativeImportEdge>,
    ) -> Result<crate::ImportReport> {
        let agent = agent.clone();
        self.with_inner(move |inner| {
            let mut report = crate::ImportReport::default();

            let NativeInner {
                engine,
                vectors,
                memory,
                graph,
                fts,
            } = &mut *inner;

            // ── Souvenirs : filtre des présents, un batch pour les neufs ──
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            let mut fresh: Vec<&NativeImportMemory> = Vec::new();
            for m in &memories {
                let exists = memory.get(engine, agent.as_str(), &m.id).map_err(storage)?.is_some();
                if exists || !seen.insert(m.id.as_str()) {
                    report.memories_skipped += 1;
                } else {
                    fresh.push(m);
                }
            }
            let entries: Vec<(&str, NewMemoryRecord<'_>, Vec<f32>)> = fresh
                .iter()
                .map(|m| {
                    (
                        m.id.as_str(),
                        NewMemoryRecord {
                            layer: m.layer.table(),
                            content: &m.content,
                            source: &m.source,
                            valid_from: m.valid_from,
                            valid_until: m.valid_until,
                            importance: m.importance,
                            last_access: m.last_access.unwrap_or(m.valid_from),
                        },
                        m.vector.clone(),
                    )
                })
                .collect();
            if !entries.is_empty() {
                memory
                    .put_many(engine, vectors, fts, agent.as_str(), &entries)
                    .map_err(storage)?;
                report.memories += entries.len();
            }

            // ── Entités : `INSERT OR IGNORE` — jamais d'écrasement ────────
            for e in entities {
                if graph.entity(engine, agent.as_str(), &e.id).map_err(storage)?.is_some() {
                    report.entities_skipped += 1;
                    continue;
                }
                graph
                    .upsert_entity(
                        engine,
                        agent.as_str(),
                        &e.id,
                        basemyai_engine::GraphEntity {
                            kind: e.kind,
                            label: e.label,
                            valid_from: e.valid_from,
                            valid_until: e.valid_until,
                        },
                    )
                    .map_err(storage)?;
                report.entities += 1;
            }

            // ── Arêtes : idem, la méta complète de l'export est préservée ─
            for e in edges {
                if graph
                    .edge_meta(engine, agent.as_str(), &e.src, &e.relation, &e.dst)
                    .map_err(storage)?
                    .is_some()
                {
                    report.edges_skipped += 1;
                    continue;
                }
                graph
                    .upsert_edge(
                        engine,
                        agent.as_str(),
                        &e.src,
                        &e.relation,
                        &e.dst,
                        basemyai_engine::GraphEdgeMeta {
                            weight: e.weight,
                            valid_from: e.valid_from,
                            valid_until: e.valid_until,
                        },
                    )
                    .map_err(storage)?;
                report.edges += 1;
            }

            Ok(report)
        })
        .await
    }
}
