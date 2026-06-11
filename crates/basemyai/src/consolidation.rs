//! Pipeline de **consolidation** (VISION §5.1, Phase 2) : transforme des
//! **épisodes** bruts (couche `episodic`) en **faits sémantiques** durables et
//! peuple le **graphe** entités/relations (§4.1).
//!
//! Combinaison *LLM + heuristiques* : la couche d'inférence model-agnostic
//! ([`LlmInference`]) extrait et résume (le *modèle* est injecté, jamais codé en
//! dur) ; des heuristiques **dédupliquent** côté `basemyai`. La promotion
//! `episodic → semantic` se fait via [`Memory::remember`], donc avec embedding —
//! les faits consolidés deviennent immédiatement recherchables.
//!
//! Conçue pour tourner **en tâche de fond**, hors chemin critique. L'écriture du
//! graphe est idempotente (`ON CONFLICT`), et les faits déjà présents sont
//! ignorés : relancer la consolidation ne duplique rien.

use basemyai_core::libsql;
use basemyai_core::CoreError;
use serde::Deserialize;

use crate::inference::LlmInference;
use crate::{now_unix, Graph, Memory, MemoryError, MemoryLayer, Result};

/// Borne le nombre d'épisodes envoyés au LLM en une passe (taille de prompt).
const MAX_EPISODES: usize = 50;

/// Compte-rendu d'une passe de consolidation (observabilité / tests).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConsolidationReport {
    /// Épisodes valides lus et soumis à l'extraction.
    pub episodes_seen: usize,
    /// Faits sémantiques nouvellement promus.
    pub facts_added: usize,
    /// Faits ignorés car déjà présents (déduplication).
    pub facts_skipped: usize,
    /// Entités insérées/mises à jour dans le graphe.
    pub entities_upserted: usize,
    /// Relations insérées/mises à jour dans le graphe.
    pub relations_upserted: usize,
}

/// Schéma JSON attendu en sortie du LLM. Champs absents tolérés (`default`).
#[derive(Debug, Deserialize)]
struct RawExtraction {
    #[serde(default)]
    facts: Vec<String>,
    #[serde(default)]
    entities: Vec<RawEntity>,
    #[serde(default)]
    relations: Vec<RawRelation>,
}

#[derive(Debug, Deserialize)]
struct RawEntity {
    id: String,
    kind: String,
    label: String,
}

#[derive(Debug, Deserialize)]
struct RawRelation {
    src: String,
    relation: String,
    dst: String,
}

/// Exécute une passe de consolidation pour l'agent de `memory`, en s'appuyant sur
/// le fournisseur d'inférence `llm`.
///
/// Étapes : lecture des épisodes valides → prompt → extraction LLM (JSON) →
/// peuplement du graphe (idempotent) → promotion des faits en `semantic` (avec
/// déduplication). Aucune écriture si aucun épisode.
///
/// # Errors
/// - [`MemoryError::Inference`] si l'appel LLM échoue.
/// - [`MemoryError::Extraction`] si la sortie n'est pas le JSON attendu.
/// - [`MemoryError::Core`] en cas d'échec de stockage/embedding.
pub async fn consolidate(memory: &Memory, llm: &dyn LlmInference) -> Result<ConsolidationReport> {
    let episodes = recent_episodes(memory, MAX_EPISODES).await?;
    if episodes.is_empty() {
        return Ok(ConsolidationReport::default());
    }

    let prompt = build_prompt(&episodes);
    let raw = llm.complete(&prompt).await?;
    let extraction: RawExtraction = serde_json::from_str(raw.trim())
        .map_err(|e| MemoryError::Extraction(format!("JSON d'extraction invalide : {e}")))?;

    // Graphe : upserts idempotents (ON CONFLICT) — relancer ne duplique pas.
    let graph = Graph::new(memory.store(), memory.agent().clone());
    for e in &extraction.entities {
        graph.add_entity(&e.id, &e.kind, &e.label).await?;
    }
    for r in &extraction.relations {
        graph.add_edge(&r.src, &r.relation, &r.dst, 1.0).await?;
    }

    let mut report = ConsolidationReport {
        episodes_seen: episodes.len(),
        entities_upserted: extraction.entities.len(),
        relations_upserted: extraction.relations.len(),
        ..ConsolidationReport::default()
    };

    // Promotion episodic → semantic, avec déduplication par contenu exact.
    for fact in &extraction.facts {
        if fact_already_known(memory, fact).await? {
            report.facts_skipped += 1;
        } else {
            memory.remember(fact, MemoryLayer::Semantic).await?;
            report.facts_added += 1;
        }
    }

    Ok(report)
}

/// Lit les contenus des épisodes **encore valides** de l'agent, du plus récent au
/// plus ancien, bornés à `limit`.
async fn recent_episodes(memory: &Memory, limit: usize) -> Result<Vec<String>> {
    let now = now_unix();
    let conn = memory.store().connect();
    let mut rows = conn
        .query(
            "SELECT content FROM memory \
             WHERE agent_id = ?1 AND layer = 'episodic' \
               AND valid_from <= ?2 AND (valid_until IS NULL OR valid_until > ?2) \
             ORDER BY valid_from DESC LIMIT ?3",
            libsql::params![memory.agent().as_str(), now, i64::try_from(limit).unwrap_or(i64::MAX)],
        )
        .await
        .map_err(storage)?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage)? {
        out.push(row.get::<String>(0).map_err(storage)?);
    }
    Ok(out)
}

/// `true` si un fait sémantique au **contenu identique** existe déjà pour l'agent.
async fn fact_already_known(memory: &Memory, fact: &str) -> Result<bool> {
    let conn = memory.store().connect();
    let mut rows = conn
        .query(
            "SELECT 1 FROM memory \
             WHERE agent_id = ?1 AND layer = 'semantic' AND content = ?2 LIMIT 1",
            libsql::params![memory.agent().as_str(), fact],
        )
        .await
        .map_err(storage)?;
    Ok(rows.next().await.map_err(storage)?.is_some())
}

/// Construit le prompt d'extraction : consigne + schéma JSON + épisodes.
fn build_prompt(episodes: &[String]) -> String {
    let mut p = String::with_capacity(512 + episodes.iter().map(String::len).sum::<usize>());
    p.push_str(
        "Tu consolides la mémoire d'un agent. À partir des ÉPISODES ci-dessous, \
         extrais les faits durables, les entités et leurs relations.\n\
         Réponds UNIQUEMENT par un objet JSON, sans texte autour, de la forme :\n\
         {\"facts\":[\"...\"],\
         \"entities\":[{\"id\":\"...\",\"kind\":\"...\",\"label\":\"...\"}],\
         \"relations\":[{\"src\":\"<id>\",\"relation\":\"...\",\"dst\":\"<id>\"}]}\n\
         Les `src`/`dst` des relations référencent les `id` des entities.\n\n\
         ÉPISODES :\n",
    );
    for (i, e) in episodes.iter().enumerate() {
        p.push_str(&format!("{}. {e}\n", i + 1));
    }
    p
}

fn storage(e: libsql::Error) -> MemoryError {
    CoreError::Storage(e.to_string()).into()
}
