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

use basemyai_core::CoreError;
use basemyai_core::libsql;
use serde::{Deserialize, Serialize};

use super::inference::LlmInference;
use crate::{Graph, Memory, MemoryError, MemoryLayer, Result, now_unix};

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

/// Résultat d'extraction : faits durables + entités + relations.
///
/// C'est le schéma JSON produit par le LLM (autonome) **ou** par l'agent lui-même
/// (consolidation pilotée par l'agent, ADR-018). Champs absents tolérés (`default`).
/// Sérialisable pour permettre aux consommateurs (serveur MCP) de le transporter.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Extraction {
    /// Faits durables à promouvoir en couche `semantic`.
    #[serde(default)]
    pub facts: Vec<String>,
    /// Entités du graphe (nœuds).
    #[serde(default)]
    pub entities: Vec<ExtractedEntity>,
    /// Relations du graphe (arêtes), référençant les `id` des entités.
    #[serde(default)]
    pub relations: Vec<ExtractedRelation>,
}

/// Une entité extraite (nœud du graphe).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    /// Identifiant stable de l'entité (référencé par les relations).
    pub id: String,
    /// Type/catégorie de l'entité (ex. `"person"`, `"project"`).
    pub kind: String,
    /// Libellé lisible.
    pub label: String,
}

/// Une relation extraite (arête du graphe).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedRelation {
    /// `id` de l'entité source.
    pub src: String,
    /// Type de relation (ex. `"works_on"`).
    pub relation: String,
    /// `id` de l'entité destination.
    pub dst: String,
}

/// Entrée de consolidation : les épisodes bruts + le prompt d'extraction prêt à
/// soumettre. Produit par [`consolidation_prompt`].
///
/// Deux usages :
/// - **autonome** : passer `prompt` à un [`LlmInference`] (cf. [`consolidate`]) ;
/// - **piloté par l'agent** (ADR-018) : remettre `episodes` à l'agent appelant
///   (via le serveur MCP) pour qu'il fasse l'extraction avec son propre LLM, puis
///   applique le résultat via [`apply_extraction`].
#[derive(Debug, Clone)]
pub struct ConsolidationInput {
    /// Contenus des épisodes valides, du plus récent au plus ancien.
    pub episodes: Vec<String>,
    /// Prompt d'extraction complet (consigne + schéma + épisodes).
    pub prompt: String,
}

/// Prépare une passe de consolidation : lit les épisodes valides de l'agent et
/// construit le prompt d'extraction. Retourne `None` s'il n'y a aucun épisode
/// (rien à consolider).
///
/// # Errors
/// [`MemoryError::Core`] en cas d'échec de lecture.
pub async fn consolidation_prompt(memory: &Memory) -> Result<Option<ConsolidationInput>> {
    let episodes = recent_episodes(memory, MAX_EPISODES).await?;
    if episodes.is_empty() {
        return Ok(None);
    }
    let prompt = build_prompt(&episodes);
    Ok(Some(ConsolidationInput { episodes, prompt }))
}

/// Parse la sortie JSON d'une extraction (tolère les fences/espaces autour).
///
/// # Errors
/// [`MemoryError::Extraction`] si la sortie n'est pas le JSON attendu.
pub fn parse_extraction(raw: &str) -> Result<Extraction> {
    serde_json::from_str(strip_json_fences(raw))
        .map_err(|e| MemoryError::Extraction(format!("JSON d'extraction invalide : {e}")))
}

/// Applique une extraction (déjà parsée) à la mémoire : peuple le graphe
/// (idempotent, `ON CONFLICT`) puis promeut les faits en `semantic` (dédupliqués
/// par contenu exact). Réutilisable quel que soit le producteur de l'extraction
/// (LLM autonome, agent MCP, import).
///
/// `episodes_seen` du rapport est laissé à 0 : l'appelant qui connaît le nombre
/// d'épisodes (cf. [`consolidate`]) peut le renseigner.
///
/// # Errors
/// [`MemoryError::Core`] en cas d'échec de stockage/embedding.
pub async fn apply_extraction(memory: &Memory, extraction: &Extraction) -> Result<ConsolidationReport> {
    // Graphe : upserts idempotents (ON CONFLICT) — relancer ne duplique pas.
    let graph = Graph::new(memory.store(), memory.agent().clone());
    for e in &extraction.entities {
        graph.add_entity(&e.id, &e.kind, &e.label).await?;
    }
    for r in &extraction.relations {
        graph.add_edge(&r.src, &r.relation, &r.dst, 1.0).await?;
    }

    let mut report = ConsolidationReport {
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

/// Exécute une passe de consolidation **autonome** pour l'agent de `memory`, en
/// s'appuyant sur le fournisseur d'inférence `llm`.
///
/// Compose [`consolidation_prompt`] → `llm.complete` → [`parse_extraction`] →
/// [`apply_extraction`]. Aucune écriture si aucun épisode.
///
/// Pour la consolidation **pilotée par l'agent** (le LLM du client MCP fait
/// l'extraction), voir [`consolidation_prompt`] + [`apply_extraction`] (ADR-018).
///
/// # Errors
/// - [`MemoryError::Inference`] si l'appel LLM échoue.
/// - [`MemoryError::Extraction`] si la sortie n'est pas le JSON attendu.
/// - [`MemoryError::Core`] en cas d'échec de stockage/embedding.
pub async fn consolidate(memory: &Memory, llm: &dyn LlmInference) -> Result<ConsolidationReport> {
    let Some(input) = consolidation_prompt(memory).await? else {
        return Ok(ConsolidationReport::default());
    };

    let raw = llm.complete(&input.prompt).await?;
    let extraction = parse_extraction(&raw)?;

    let mut report = apply_extraction(memory, &extraction).await?;
    report.episodes_seen = input.episodes.len();
    Ok(report)
}

/// Retire d'éventuelles fences Markdown (```json … ```) autour d'un JSON et trim.
fn strip_json_fences(raw: &str) -> &str {
    let s = raw.trim();
    let s = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")).unwrap_or(s);
    s.strip_suffix("```").unwrap_or(s).trim()
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
