//! Export/import de la mémoire d'un agent (portabilité, backup, migration).
//!
//! Format **JSONL versionné** : une ligne d'en-tête puis une ligne JSON par
//! souvenir, entité et relation. Les **embeddings ne sont pas exportés** : ils
//! sont re-calculés à l'import par l'embedder de la mémoire cible — c'est ce
//! qui fait de l'export le chemin de migration de modèle d'embedding (on
//! change de modèle, on réimporte, tout est ré-encodé).
//!
//! L'import est **atomique** (une seule [`basemyai_core::WriteTxn`]) et
//! **idempotent** (`INSERT OR IGNORE` : réimporter le même fichier ne
//! duplique rien). Les lignes importées sont rattachées à l'agent de la
//! mémoire **cible** — l'`agent_id` de l'en-tête est informatif.

use serde::{Deserialize, Serialize};

use basemyai_core::libsql;

use super::{Memory, MemoryLayer, storage, to_vec_literal};
use crate::{MemoryError, Result, now_unix};

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
        let conn = self.store.connect();
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

        let mut rows = conn
            .query(
                "SELECT id, layer, content, valid_from, valid_until, importance, last_access \
                 FROM memory WHERE agent_id = ?1 ORDER BY valid_from, id",
                libsql::params![self.agent.as_str()],
            )
            .await
            .map_err(storage)?;
        while let Some(row) = rows.next().await.map_err(storage)? {
            push_line(
                &mut out,
                &ExportLine::Memory {
                    id: text(&row, 0)?,
                    layer: text(&row, 1)?,
                    content: text(&row, 2)?,
                    valid_from: integer(&row, 3)?,
                    valid_until: integer_opt(&row, 4)?,
                    importance: real(&row, 5)?,
                    last_access: integer_opt(&row, 6)?,
                },
            )?;
        }

        let mut rows = conn
            .query(
                "SELECT id, kind, label, valid_from, valid_until, importance \
                 FROM entity WHERE agent_id = ?1 ORDER BY id",
                libsql::params![self.agent.as_str()],
            )
            .await
            .map_err(storage)?;
        while let Some(row) = rows.next().await.map_err(storage)? {
            push_line(
                &mut out,
                &ExportLine::Entity {
                    id: text(&row, 0)?,
                    kind: text(&row, 1)?,
                    label: text(&row, 2)?,
                    valid_from: integer(&row, 3)?,
                    valid_until: integer_opt(&row, 4)?,
                    importance: real(&row, 5)?,
                },
            )?;
        }

        let mut rows = conn
            .query(
                "SELECT src, dst, relation, weight, valid_from, valid_until \
                 FROM edge WHERE agent_id = ?1 ORDER BY src, dst, relation",
                libsql::params![self.agent.as_str()],
            )
            .await
            .map_err(storage)?;
        while let Some(row) = rows.next().await.map_err(storage)? {
            push_line(
                &mut out,
                &ExportLine::Edge {
                    src: text(&row, 0)?,
                    dst: text(&row, 1)?,
                    relation: text(&row, 2)?,
                    weight: real(&row, 3)?,
                    valid_from: integer(&row, 4)?,
                    valid_until: integer_opt(&row, 5)?,
                },
            )?;
        }

        Ok(out)
    }

    /// Importe un export JSONL dans **cette** mémoire (l'agent cible est celui
    /// de la façade, quel que soit l'`agent_id` d'origine de l'export).
    ///
    /// Les souvenirs sont **ré-embeddés** par l'embedder courant (une passe
    /// `embed_batch` par lots de 128), puis tout est inséré dans **une seule
    /// transaction** : l'import aboutit entièrement ou pas du tout.
    /// Idempotent : les lignes dont l'identifiant existe déjà sont comptées
    /// en `*_skipped` et laissées intactes.
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
                    importance,
                } => entities.push((id, kind, label, valid_from, valid_until, importance)),
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
            return Err(MemoryError::Porting("en-tête absent : ce n'est pas un export BaseMyAI".into()));
        }

        // ── Ré-embedding par lots (hors transaction : CPU-bound) ─────────────
        let contents: Vec<String> = memories.iter().map(|m| m.2.clone()).collect();
        let mut vectors = Vec::with_capacity(contents.len());
        for chunk in contents.chunks(EMBED_CHUNK) {
            vectors.extend(self.embedder.embed_batch(chunk)?);
        }

        // ── Écriture atomique ─────────────────────────────────────────────────
        let mut report = ImportReport::default();
        let txn = self.store.begin_write().await?;

        for ((id, layer, content, valid_from, valid_until, importance, last_access), vector) in
            memories.iter().zip(&vectors)
        {
            let inserted = txn
                .execute(
                    "INSERT OR IGNORE INTO memory \
                     (id, agent_id, layer, content, valid_from, valid_until, importance, last_access, emb) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, vector(?9))",
                    libsql::params![
                        id.as_str(),
                        self.agent.as_str(),
                        layer.table(),
                        content.as_str(),
                        *valid_from,
                        *valid_until,
                        *importance,
                        *last_access,
                        to_vec_literal(vector),
                    ],
                )
                .await
                .map_err(storage)?;
            if inserted > 0 {
                txn.execute(
                    "INSERT INTO memory_fts (id, agent_id, content) VALUES (?1, ?2, ?3)",
                    libsql::params![id.as_str(), self.agent.as_str(), content.as_str()],
                )
                .await
                .map_err(storage)?;
                report.memories += 1;
            } else {
                report.memories_skipped += 1;
            }
        }

        for (id, kind, label, valid_from, valid_until, importance) in &entities {
            let inserted = txn
                .execute(
                    "INSERT OR IGNORE INTO entity (id, agent_id, kind, label, valid_from, valid_until, importance) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    libsql::params![
                        id.as_str(),
                        self.agent.as_str(),
                        kind.as_str(),
                        label.as_str(),
                        *valid_from,
                        *valid_until,
                        *importance,
                    ],
                )
                .await
                .map_err(storage)?;
            if inserted > 0 {
                report.entities += 1;
            } else {
                report.entities_skipped += 1;
            }
        }

        for (src, dst, relation, weight, valid_from, valid_until) in &edges {
            let inserted = txn
                .execute(
                    "INSERT OR IGNORE INTO edge (src, dst, agent_id, relation, weight, valid_from, valid_until) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    libsql::params![
                        src.as_str(),
                        dst.as_str(),
                        self.agent.as_str(),
                        relation.as_str(),
                        *weight,
                        *valid_from,
                        *valid_until,
                    ],
                )
                .await
                .map_err(storage)?;
            if inserted > 0 {
                report.edges += 1;
            } else {
                report.edges_skipped += 1;
            }
        }

        txn.commit().await?;
        Ok(report)
    }
}

/// Sérialise une ligne JSONL et l'ajoute au tampon.
fn push_line(out: &mut String, line: &ExportLine) -> Result<()> {
    let json = serde_json::to_string(line).map_err(|e| MemoryError::Porting(format!("sérialisation : {e}")))?;
    out.push_str(&json);
    out.push('\n');
    Ok(())
}

// ── Lecture typée des colonnes libSQL (erreurs → Storage) ────────────────────

fn text(row: &libsql::Row, idx: i32) -> Result<String> {
    row.get::<String>(idx).map_err(storage)
}

fn integer(row: &libsql::Row, idx: i32) -> Result<i64> {
    row.get::<i64>(idx).map_err(storage)
}

fn integer_opt(row: &libsql::Row, idx: i32) -> Result<Option<i64>> {
    match row.get_value(idx).map_err(storage)? {
        libsql::Value::Null => Ok(None),
        libsql::Value::Integer(i) => Ok(Some(i)),
        other => Err(basemyai_core::CoreError::Storage(format!("colonne {idx} : entier attendu, reçu {other:?}")).into()),
    }
}

fn real(row: &libsql::Row, idx: i32) -> Result<f64> {
    match row.get_value(idx).map_err(storage)? {
        libsql::Value::Real(r) => Ok(r),
        // SQLite peut rendre l'affinité entière pour un littéral REAL rond.
        #[allow(clippy::cast_precision_loss)]
        libsql::Value::Integer(i) => Ok(i as f64),
        other => Err(basemyai_core::CoreError::Storage(format!("colonne {idx} : réel attendu, reçu {other:?}")).into()),
    }
}
