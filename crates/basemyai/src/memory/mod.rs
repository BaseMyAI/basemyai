//! Façade mémoire. Injecte les primitives du core (`Store`, `VectorIndex`,
//! `Embedder`) — testable en isolation via des doubles. Applique l'isolation
//! par agent et le RAG temporel par-dessus.

mod isolation;
mod layer;
pub(crate) mod schema;

pub use isolation::AgentId;
pub use layer::{AgentStats, MemoryLayer, Record};

use basemyai_core::libsql;
use basemyai_core::{Embedder, Filter, Store, Value};
use uuid::Uuid;

use crate::temporal::Validity;
use crate::{Result, now_unix};

/// Mémoire d'un agent : store (vecteur natif) + embedder, scellés par un
/// [`AgentId`]. Le chiffrement est obligatoire (ADR-007).
pub struct Memory {
    store: Store,
    embedder: Box<dyn Embedder>,
    agent: AgentId,
}

impl Memory {
    /// Assemble une mémoire à partir des primitives du core déjà construites,
    /// **sans** migrer le schéma (à utiliser quand le schéma est déjà en place).
    #[must_use]
    pub fn new(store: Store, embedder: Box<dyn Embedder>, agent: AgentId) -> Self {
        Self { store, embedder, agent }
    }

    /// Ouvre une mémoire : vérifie le chiffrement, applique le schéma
    /// (`memory` + index vecteur natif), puis renvoie la façade scellée par `agent`.
    ///
    /// Le chiffrement est **obligatoire** pour les stores sur fichier (ADR-007) :
    /// un store `:memory:` est éphémère, la règle ne s'y applique pas.
    ///
    /// # Errors
    /// [`crate::MemoryError::EncryptionRequired`] si le store est sur fichier et non chiffré.
    /// [`crate::MemoryError::Core`] si la migration échoue.
    pub async fn open(store: Store, embedder: Box<dyn Embedder>, agent: AgentId) -> Result<Self> {
        if store.path().is_some() && !store.is_encrypted() {
            return Err(crate::MemoryError::EncryptionRequired);
        }
        store.migrate(&schema::schema()).await?;
        Ok(Self { store, embedder, agent })
    }

    /// L'agent propriétaire de cette mémoire.
    #[must_use]
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// Store sous-jacent. `pub(crate)` : la consolidation (même crate) lit les
    /// épisodes et construit un `Graph` dessus, sans exposer le store au public.
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// Mémorise un texte dans une couche, valide dès maintenant et sans
    /// expiration.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/stockage.
    pub async fn remember(&self, text: &str, layer: MemoryLayer) -> Result<()> {
        let now = now_unix();
        self.remember_with(text, layer, Validity::since(now)).await
    }

    /// Mémorise un texte avec une fenêtre de validité explicite.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/stockage.
    pub async fn remember_with(&self, text: &str, layer: MemoryLayer, validity: Validity) -> Result<()> {
        let vector = self.embedder.embed(text)?;
        let id = Uuid::new_v4().to_string();
        let conn = self.store.connect();
        conn.execute(
            "INSERT INTO memory (id, agent_id, layer, content, valid_from, valid_until, emb) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, vector(?7))",
            libsql::params![
                id,
                self.agent.as_str(),
                layer.table(),
                text,
                validity.valid_from,
                validity.valid_until,
                to_vec_literal(&vector),
            ],
        )
        .await
        .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Recall temporel : pertinent ET valide, borné à cet agent.
    ///
    /// Le filtre combine isolation (`agent_id = ?`) ET temporel
    /// (`valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)`) en un
    /// seul [`Filter`] paramétré — le core ne connaît le sens d'aucun.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall(&self, query: &str, k: usize) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();

        let filter = Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)",
            vec![
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
            ],
        );

        let neighbors = self.store.vector_knn("memory", &qvec, k, Some(&filter)).await?;

        let conn = self.store.connect();
        let mut out = Vec::with_capacity(neighbors.len());
        for n in neighbors {
            let mut rows = conn
                .query(
                    "SELECT content, layer FROM memory WHERE id = ?1",
                    libsql::params![n.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?
            {
                let content: String = row
                    .get(0)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                let layer: String = row
                    .get(1)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                out.push(Record {
                    id: n.id,
                    text: content,
                    layer: MemoryLayer::from_table(&layer)?,
                    score: n.distance,
                });
            }
        }
        if !out.is_empty() {
            let now_access = now_unix();
            for record in &out {
                conn.execute(
                    "UPDATE memory SET last_access = ?1 WHERE id = ?2",
                    libsql::params![now_access, record.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            }
        }
        Ok(out)
    }

    /// Recall filtré sur une couche unique. Met à jour `last_access` sur chaque
    /// souvenir retourné (l'oubli adaptatif en dépend).
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall_by_layer(&self, query: &str, layer: MemoryLayer, k: usize) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();

        let filter = Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?) AND layer = ?",
            vec![
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
                Value::Text(layer.table().to_string()),
            ],
        );

        let neighbors = self.store.vector_knn("memory", &qvec, k, Some(&filter)).await?;

        let conn = self.store.connect();
        let mut out = Vec::with_capacity(neighbors.len());
        for n in &neighbors {
            let mut rows = conn
                .query(
                    "SELECT content, layer FROM memory WHERE id = ?1",
                    libsql::params![n.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?
            {
                let content: String = row
                    .get(0)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                let layer_str: String = row
                    .get(1)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                out.push(Record {
                    id: n.id.clone(),
                    text: content,
                    layer: MemoryLayer::from_table(&layer_str)?,
                    score: n.distance,
                });
            }
        }
        if !out.is_empty() {
            let now_access = now_unix();
            for record in &out {
                conn.execute(
                    "UPDATE memory SET last_access = ?1 WHERE id = ?2",
                    libsql::params![now_access, record.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            }
        }
        Ok(out)
    }

    /// Invalide un souvenir en fixant `valid_until = now()`. Il n'apparaît plus
    /// dans les recalls futurs mais reste physiquement en base.
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn invalidate(&self, id: &str) -> Result<()> {
        let now = now_unix();
        let conn = self.store.connect();
        conn.execute(
            "UPDATE memory SET valid_until = ?1 WHERE id = ?2 AND agent_id = ?3",
            libsql::params![now, id, self.agent.as_str()],
        )
        .await
        .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Suppression physique d'un souvenir (RGPD, droit à l'effacement).
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn forget(&self, id: &str) -> Result<()> {
        let conn = self.store.connect();
        conn.execute(
            "DELETE FROM memory WHERE id = ?1 AND agent_id = ?2",
            libsql::params![id, self.agent.as_str()],
        )
        .await
        .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Statistiques des souvenirs valides de cet agent, par couche.
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn stats(&self) -> Result<AgentStats> {
        let now = now_unix();
        let conn = self.store.connect();
        let mut rows = conn
            .query(
                "SELECT layer, COUNT(*) FROM memory \
                 WHERE agent_id = ?1 AND valid_from <= ?2 \
                   AND (valid_until IS NULL OR valid_until > ?2) \
                 GROUP BY layer",
                libsql::params![self.agent.as_str(), now],
            )
            .await
            .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;

        let mut stats = AgentStats::default();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?
        {
            let layer_str: String = row
                .get(0)
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            let count: i64 = row
                .get(1)
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            let n = usize::try_from(count).unwrap_or(0);
            match layer_str.as_str() {
                "short_term" => stats.short_term = n,
                "episodic" => stats.episodic = n,
                "procedural" => stats.procedural = n,
                "semantic" => stats.semantic = n,
                _ => {}
            }
        }
        Ok(stats)
    }

    /// Recall vectoriel limité aux souvenirs dont le contenu mentionne une entité
    /// du graphe (P2). Met à jour `last_access` sur les résultats.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn search_graph(&self, query: &str, k: usize) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();

        let filter = Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?) \
             AND EXISTS (\
               SELECT 1 FROM entity \
               WHERE entity.agent_id = ? \
                 AND (entity.valid_until IS NULL OR entity.valid_until > ?) \
                 AND instr(content, entity.label) > 0\
             )",
            vec![
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
            ],
        );

        let neighbors = self.store.vector_knn("memory", &qvec, k, Some(&filter)).await?;

        let conn = self.store.connect();
        let mut out = Vec::with_capacity(neighbors.len());
        for n in &neighbors {
            let mut rows = conn
                .query(
                    "SELECT content, layer FROM memory WHERE id = ?1",
                    libsql::params![n.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?
            {
                let content: String = row
                    .get(0)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                let layer_str: String = row
                    .get(1)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                out.push(Record {
                    id: n.id.clone(),
                    text: content,
                    layer: MemoryLayer::from_table(&layer_str)?,
                    score: n.distance,
                });
            }
        }
        if !out.is_empty() {
            let now_access = now_unix();
            for record in &out {
                conn.execute(
                    "UPDATE memory SET last_access = ?1 WHERE id = ?2",
                    libsql::params![now_access, record.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            }
        }
        Ok(out)
    }
}

/// Formate un vecteur en littéral SQL `[a,b,c]` consommé par `vector(?)`.
fn to_vec_literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}
