//! Façade mémoire. Injecte les primitives du core (`Store`, `VectorIndex`,
//! `Embedder`) — testable en isolation via des doubles. Applique l'isolation
//! par agent et le RAG temporel par-dessus.

use basemyai_core::libsql;
use basemyai_core::{Embedder, Filter, Store, Value};
use uuid::Uuid;

use crate::temporal::Validity;
use crate::{AgentId, Result, now_unix, schema};

/// Les 4 couches mémoire (ADR-004). Chacune a son mode d'accès et sa durée de vie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLayer {
    /// Contexte de travail de la session (TTL court).
    ShortTerm,
    /// Ce qui s'est passé et quand.
    Episodic,
    /// Procédures/compétences apprises.
    Procedural,
    /// Faits recherchables vectoriellement.
    Semantic,
}

impl MemoryLayer {
    /// Nom de couche stocké dans la colonne `layer`.
    #[must_use]
    pub fn table(self) -> &'static str {
        match self {
            Self::ShortTerm => "short_term",
            Self::Episodic => "episodic",
            Self::Procedural => "procedural",
            Self::Semantic => "semantic",
        }
    }

    /// Reconstruit une couche depuis son nom stocké.
    ///
    /// # Errors
    /// [`MemoryError::UnknownLayer`](crate::MemoryError::UnknownLayer) si le
    /// nom ne correspond à aucune couche connue.
    pub fn from_table(name: &str) -> Result<Self> {
        match name {
            "short_term" => Ok(Self::ShortTerm),
            "episodic" => Ok(Self::Episodic),
            "procedural" => Ok(Self::Procedural),
            "semantic" => Ok(Self::Semantic),
            other => Err(crate::MemoryError::UnknownLayer(other.to_string())),
        }
    }
}

/// Une mémoire retournée par `recall`.
#[derive(Debug, Clone)]
pub struct Record {
    pub id: String,
    pub text: String,
    pub layer: MemoryLayer,
    pub score: f32,
}

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

    /// Ouvre une mémoire : applique le schéma (`memory` + index vecteur natif)
    /// puis renvoie la façade scellée par `agent`.
    ///
    /// # Errors
    /// [`MemoryError::Core`](crate::MemoryError::Core) si la migration échoue.
    pub async fn open(store: Store, embedder: Box<dyn Embedder>, agent: AgentId) -> Result<Self> {
        store.migrate(&schema()).await?;
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
                .query("SELECT content, layer FROM memory WHERE id = ?1", libsql::params![n.id.clone()])
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            if let Some(row) = rows.next().await.map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))? {
                let content: String = row.get(0).map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                let layer: String = row.get(1).map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                out.push(Record {
                    id: n.id,
                    text: content,
                    layer: MemoryLayer::from_table(&layer)?,
                    score: n.distance,
                });
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
