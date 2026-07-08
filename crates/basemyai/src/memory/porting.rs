// SPDX-License-Identifier: BUSL-1.1
//! Export/import de la mémoire d'un agent (portabilité, backup, migration de
//! modèle d'embedding).
//!
//! Format **JSONL versionné** : une ligne d'en-tête puis une ligne JSON par
//! souvenir, entité et relation. Les **embeddings ne sont pas exportés** : ils
//! sont re-calculés à l'import par l'embedder de la mémoire cible — c'est ce
//! qui fait de l'export le chemin de migration de modèle d'embedding (on
//! change de modèle, on réimporte, tout est ré-encodé).
//!
//! Idempotent (les ids déjà présents sont comptés `*_skipped` et laissés
//! intacts). **Écart d'atomicité assumé** : les souvenirs neufs s'insèrent en
//! un seul batch WAL tout-ou-rien (`put_many`, N5.5), mais entités/arêtes
//! suivent en upserts individuels durables — l'import est idempotent et
//! reprennable, pas globalement atomique (ADR-027 §6, ADR-032 §3).

use serde::{Deserialize, Serialize};

use super::Memory;
use crate::storage::{NativeImportEdge, NativeImportEntity, NativeImportMemory};
use crate::{MemoryError, MemoryLayer, Result, now_unix};

/// Identifiant de format de l'en-tête JSONL.
const FORMAT: &str = "basemyai-export";
/// Version du format (bump = nouvelle variante de lecture, jamais de réécriture).
const VERSION: u32 = 1;
/// Taille des lots passés à `embed_batch` à l'import (borne la mémoire de travail).
const EMBED_CHUNK: usize = 128;

/// Une ligne du flux JSONL, discriminée par le champ `type`.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ExportLine {
    Header {
        format: String,
        version: u32,
        agent_id: String,
        embedding_model: String,
        embedding_dim: usize,
        exported_at: i64,
    },
    Memory {
        id: String,
        layer: String,
        content: String,
        valid_from: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        valid_until: Option<i64>,
        #[serde(default)]
        importance: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_access: Option<i64>,
    },
    Entity {
        id: String,
        kind: String,
        label: String,
        valid_from: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        valid_until: Option<i64>,
        #[serde(default)]
        importance: f64,
    },
    Edge {
        src: String,
        dst: String,
        relation: String,
        #[serde(default = "default_weight")]
        weight: f64,
        valid_from: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        valid_until: Option<i64>,
    },
}

fn default_weight() -> f64 {
    1.0
}

/// Bilan d'un [`Memory::import_jsonl`] : lignes insérées vs déjà présentes
/// (idempotence — un ré-import compte tout en `*_skipped`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Souvenirs insérés.
    pub memories: usize,
    /// Souvenirs déjà présents (id identique), laissés intacts.
    pub memories_skipped: usize,
    /// Entités du graphe insérées.
    pub entities: usize,
    /// Entités déjà présentes, laissées intactes.
    pub entities_skipped: usize,
    /// Relations du graphe insérées.
    pub edges: usize,
    /// Relations déjà présentes, laissées intactes.
    pub edges_skipped: usize,
}

impl Memory {
    /// Exporte **tout** ce qui appartient à cet agent — souvenirs (y compris
    /// invalidés/expirés : c'est un backup, la validité est préservée),
    /// entités et relations du graphe — en JSONL versionné.
    ///
    /// Les embeddings sont volontairement exclus (re-calculables) : le fichier
    /// est portable entre machines et entre modèles d'embedding.
    ///
    /// # Errors
    /// Propage les erreurs de stockage/sérialisation.
    pub async fn export_jsonl(&self) -> Result<String> {
        let mut out = String::new();
        push_line(
            &mut out,
            &ExportLine::Header {
                format: FORMAT.to_string(),
                version: VERSION,
                agent_id: self.agent.as_str().to_string(),
                embedding_model: self.embedder.model_id().to_string(),
                embedding_dim: self.embedder.dim(),
                exported_at: now_unix(),
            },
        )?;

        let rows = self.native_engine().export_rows(&self.agent).await?;

        for (id, record) in rows.memories {
            push_line(
                &mut out,
                &ExportLine::Memory {
                    id,
                    layer: record.layer,
                    content: record.content,
                    valid_from: record.valid_from,
                    valid_until: record.valid_until,
                    importance: record.importance,
                    last_access: Some(record.last_access),
                },
            )?;
        }

        for (id, entity) in rows.entities {
            push_line(
                &mut out,
                &ExportLine::Entity {
                    id,
                    kind: entity.kind,
                    label: entity.label,
                    valid_from: entity.valid_from,
                    valid_until: entity.valid_until,
                    importance: default_weight(),
                },
            )?;
        }

        for (src, relation, dst, meta) in rows.edges {
            push_line(
                &mut out,
                &ExportLine::Edge {
                    src,
                    dst,
                    relation,
                    weight: meta.weight,
                    valid_from: meta.valid_from,
                    valid_until: meta.valid_until,
                },
            )?;
        }

        Ok(out)
    }

    /// Importe un export JSONL dans **cette** mémoire (l'agent cible est celui
    /// de la façade, quel que soit l'`agent_id` d'origine de l'export).
    ///
    /// Les souvenirs sont **ré-embeddés** par l'embedder courant (une passe
    /// `embed_batch` par lots de 128), puis les souvenirs neufs partent en un
    /// seul batch WAL tout-ou-rien (N5.5). Idempotent : les lignes dont
    /// l'identifiant existe déjà sont comptées en `*_skipped` et laissées
    /// intactes.
    ///
    /// # Errors
    /// [`MemoryError::Porting`] si l'en-tête manque/diverge ou si une ligne est
    /// malformée ; [`MemoryError::UnknownLayer`] si une couche est inconnue ;
    /// propage les erreurs d'embedding/stockage.
    pub async fn import_jsonl(&self, jsonl: &str) -> Result<ImportReport> {
        // ── Parse + validation, AVANT toute écriture (fail fast) ──────────────
        let mut header_seen = false;
        let mut memories = Vec::new();
        let mut entities = Vec::new();
        let mut edges = Vec::new();

        for (i, raw) in jsonl.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let parsed: ExportLine = serde_json::from_str(line)
                .map_err(|e| MemoryError::Porting(format!("ligne {} malformée : {e}", i + 1)))?;
            if !header_seen && !matches!(parsed, ExportLine::Header { .. }) {
                return Err(MemoryError::Porting(format!(
                    "ligne {} : données avant l'en-tête (fichier tronqué ?)",
                    i + 1
                )));
            }
            match parsed {
                ExportLine::Header { format, version, .. } => {
                    if header_seen {
                        return Err(MemoryError::Porting(format!("en-tête dupliqué (ligne {})", i + 1)));
                    }
                    if format != FORMAT {
                        return Err(MemoryError::Porting(format!("format inconnu : {format:?}")));
                    }
                    if version != VERSION {
                        return Err(MemoryError::Porting(format!(
                            "version d'export {version} non supportée (max {VERSION})"
                        )));
                    }
                    header_seen = true;
                }
                ExportLine::Memory {
                    id,
                    layer,
                    content,
                    valid_from,
                    valid_until,
                    importance,
                    last_access,
                } => {
                    // Valide la couche maintenant : aucune écriture partielle possible.
                    let layer = MemoryLayer::from_table(&layer)?;
                    memories.push((id, layer, content, valid_from, valid_until, importance, last_access));
                }
                ExportLine::Entity {
                    id,
                    kind,
                    label,
                    valid_from,
                    valid_until,
                    ..
                } => entities.push((id, kind, label, valid_from, valid_until)),
                ExportLine::Edge {
                    src,
                    dst,
                    relation,
                    weight,
                    valid_from,
                    valid_until,
                } => edges.push((src, dst, relation, weight, valid_from, valid_until)),
            }
        }
        if !header_seen {
            return Err(MemoryError::Porting(
                "en-tête absent : ce n'est pas un export BaseMyAI".into(),
            ));
        }

        // ── Ré-embedding par lots (CPU-bound) ─────────────────────────────────
        let contents: Vec<String> = memories.iter().map(|m| m.2.clone()).collect();
        let mut vectors = Vec::with_capacity(contents.len());
        for chunk in contents.chunks(EMBED_CHUNK) {
            vectors.extend(self.embedder.embed_batch(chunk)?);
        }

        let import_memories: Vec<NativeImportMemory> = memories
            .into_iter()
            .zip(vectors)
            .map(
                |((id, layer, content, valid_from, valid_until, importance, last_access), vector)| NativeImportMemory {
                    id,
                    layer,
                    content,
                    source: super::SOURCE_USER.to_string(),
                    valid_from,
                    valid_until,
                    importance,
                    last_access,
                    vector,
                },
            )
            .collect();
        let import_entities: Vec<NativeImportEntity> = entities
            .into_iter()
            .map(|(id, kind, label, valid_from, valid_until)| NativeImportEntity {
                id,
                kind,
                label,
                valid_from,
                valid_until,
            })
            .collect();
        let import_edges: Vec<NativeImportEdge> = edges
            .into_iter()
            .map(
                |(src, dst, relation, weight, valid_from, valid_until)| NativeImportEdge {
                    src,
                    dst,
                    relation,
                    weight,
                    valid_from,
                    valid_until,
                },
            )
            .collect();

        self.native_engine()
            .import_rows(&self.agent, import_memories, import_entities, import_edges)
            .await
    }
}

/// Sérialise une ligne JSONL et l'ajoute au tampon.
fn push_line(out: &mut String, line: &ExportLine) -> Result<()> {
    let json = serde_json::to_string(line).map_err(|e| MemoryError::Porting(format!("sérialisation : {e}")))?;
    out.push_str(&json);
    out.push('\n');
    Ok(())
}
