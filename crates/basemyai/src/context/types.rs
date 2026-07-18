// SPDX-License-Identifier: BUSL-1.1
//! Contrats publics du Context Engine.

use crate::{MemoryLayer, TrustLevel, Validity};

/// Nombre maximal de resultats que le compiler accepte en entree.
pub const MAX_CONTEXT_CANDIDATES: usize = 256;
/// Nombre maximal d'evenements conserves dans une trace detaillee.
pub const MAX_CONTEXT_TRACE_EVENTS: usize = 128;
const DEFAULT_CONTEXT_CANDIDATES: usize = 64;

/// Profil de compilation. Un profil modifie uniquement poids et quotas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ContextProfile {
    /// Compromis generaliste.
    #[default]
    Balanced,
    /// Favorise le contexte court et les evenements conversationnels.
    Conversation,
    /// Favorise contraintes, procedures et references techniques.
    Coding,
    /// Favorise contraintes et procedures directement actionnables.
    Execution,
    /// Minimise le poids des donnees incertaines sans jamais les interdire.
    SafetyCritical,
}

impl ContextProfile {
    /// Nom wire stable du profil.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Conversation => "conversation",
            Self::Coding => "coding",
            Self::Execution => "execution",
            Self::SafetyCritical => "safety_critical",
        }
    }
}

/// Format du rendu directement consommable par un agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ContextRenderFormat {
    /// Texte structure sans syntaxe Markdown.
    Text,
    /// Markdown avec citations inline.
    #[default]
    Markdown,
    /// JSON compact et deterministe.
    Json,
}

impl ContextRenderFormat {
    /// Nom wire stable du format.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

/// Niveau d'explicabilite conserve dans le bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ContextTraceLevel {
    /// Compteurs stables sans evenements individuels.
    #[default]
    Compact,
    /// Evenements individuels, bornes par [`MAX_CONTEXT_TRACE_EVENTS`].
    Detailed,
}

/// Politique de provenance appliquee apres le recall.
///
/// Une provenance n'est jamais une certification de securite du contenu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ContextSourcePolicy {
    /// Conserve toutes les provenances, y compris inconnues.
    AllowAll,
    /// Exclut les souvenirs reimportes, comportement par defaut.
    #[default]
    ExcludeImported,
    /// Ne conserve que les ecritures directes et les faits consolides.
    UserAndConsolidationOnly,
}

/// Requete de compilation pour une [`crate::Memory`] deja scellee par agent.
#[derive(Debug, Clone)]
pub struct ContextRequest<'a> {
    pub(super) query: &'a str,
    pub(super) token_budget: usize,
    pub(super) candidate_limit: usize,
    pub(super) include_procedural: bool,
    pub(super) source_policy: ContextSourcePolicy,
    pub(super) profile: ContextProfile,
    pub(super) render_format: ContextRenderFormat,
    pub(super) trace_level: ContextTraceLevel,
}

impl<'a> ContextRequest<'a> {
    /// Cree une requete avec un pool de 64 candidats, sans procedure et en
    /// excluant les souvenirs importes.
    #[must_use]
    pub fn new(query: &'a str, token_budget: usize) -> Self {
        Self {
            query,
            token_budget,
            candidate_limit: DEFAULT_CONTEXT_CANDIDATES,
            include_procedural: false,
            source_policy: ContextSourcePolicy::default(),
            profile: ContextProfile::default(),
            render_format: ContextRenderFormat::default(),
            trace_level: ContextTraceLevel::default(),
        }
    }

    /// Configure la taille du pool de recall, validee a la compilation.
    #[must_use]
    pub fn candidate_limit(mut self, candidate_limit: usize) -> Self {
        self.candidate_limit = candidate_limit;
        self
    }

    /// Inclut explicitement la couche procedurale dans le recall.
    #[must_use]
    pub fn include_procedural(mut self) -> Self {
        self.include_procedural = true;
        self
    }

    /// Configure le filtrage de provenance.
    #[must_use]
    pub fn source_policy(mut self, source_policy: ContextSourcePolicy) -> Self {
        self.source_policy = source_policy;
        self
    }

    /// Configure le profil de poids et quotas.
    #[must_use]
    pub fn profile(mut self, profile: ContextProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Selectionne le profil conversationnel.
    #[must_use]
    pub fn conversation_profile(self) -> Self {
        self.profile(ContextProfile::Conversation)
    }

    /// Selectionne le profil de programmation.
    #[must_use]
    pub fn coding_profile(self) -> Self {
        self.profile(ContextProfile::Coding)
    }

    /// Selectionne le profil d'execution.
    #[must_use]
    pub fn execution_profile(self) -> Self {
        self.profile(ContextProfile::Execution)
    }

    /// Selectionne le profil critique.
    #[must_use]
    pub fn safety_critical_profile(self) -> Self {
        self.profile(ContextProfile::SafetyCritical)
    }

    /// Configure le format de rendu.
    #[must_use]
    pub fn render_format(mut self, render_format: ContextRenderFormat) -> Self {
        self.render_format = render_format;
        self
    }

    /// Demande un rendu texte.
    #[must_use]
    pub fn render_text(self) -> Self {
        self.render_format(ContextRenderFormat::Text)
    }

    /// Demande un rendu Markdown.
    #[must_use]
    pub fn render_markdown(self) -> Self {
        self.render_format(ContextRenderFormat::Markdown)
    }

    /// Demande un rendu JSON.
    #[must_use]
    pub fn render_json(self) -> Self {
        self.render_format(ContextRenderFormat::Json)
    }

    /// Configure le niveau de trace.
    #[must_use]
    pub fn trace_level(mut self, trace_level: ContextTraceLevel) -> Self {
        self.trace_level = trace_level;
        self
    }

    /// Conserve une trace detaillee bornee.
    #[must_use]
    pub fn detailed_trace(self) -> Self {
        self.trace_level(ContextTraceLevel::Detailed)
    }

    /// Alias historique de [`Self::detailed_trace`].
    #[must_use]
    pub fn explain(self) -> Self {
        self.detailed_trace()
    }

    /// Requete transmise au recall hybride.
    #[must_use]
    pub fn query(&self) -> &str {
        self.query
    }

    /// Budget maximal estime du rendu final.
    #[must_use]
    pub fn token_budget(&self) -> usize {
        self.token_budget
    }

    /// Nombre maximal de candidats demandes au recall.
    #[must_use]
    pub fn candidates(&self) -> usize {
        self.candidate_limit
    }

    /// Profil de compilation demande.
    #[must_use]
    pub fn compilation_profile(&self) -> ContextProfile {
        self.profile
    }

    /// Format de rendu demande.
    #[must_use]
    pub fn requested_render_format(&self) -> ContextRenderFormat {
        self.render_format
    }

    /// Niveau de trace demande.
    #[must_use]
    pub fn requested_trace_level(&self) -> ContextTraceLevel {
        self.trace_level
    }
}

/// Fonction semantique d'un item dans le contexte compile.
///
/// La derivation repose exclusivement sur la couche et la provenance typee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ContextRole {
    /// Fait semantique direct ou consolide.
    Fact,
    /// Contrainte de contexte court.
    Constraint,
    /// Procedure explicite.
    Procedure,
    /// Evenement episodique.
    Event,
    /// Reference semantique importee.
    Reference,
    /// Donnee semantique de provenance inconnue.
    UncertainData,
}

impl ContextRole {
    /// Derive un role sans examiner le texte libre.
    #[must_use]
    pub const fn derive(layer: MemoryLayer, trust: TrustLevel) -> Self {
        match layer {
            MemoryLayer::ShortTerm => Self::Constraint,
            MemoryLayer::Procedural => Self::Procedure,
            MemoryLayer::Episodic => Self::Event,
            MemoryLayer::Semantic => match trust {
                TrustLevel::Import => Self::Reference,
                TrustLevel::Unknown => Self::UncertainData,
                TrustLevel::User | TrustLevel::Consolidation => Self::Fact,
            },
        }
    }

    /// Nom wire stable du role.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Constraint => "constraint",
            Self::Procedure => "procedure",
            Self::Event => "event",
            Self::Reference => "reference",
            Self::UncertainData => "uncertain_data",
        }
    }
}

/// Section semantique du bundle final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ContextSectionKind {
    /// Contexte de travail a courte duree de vie.
    WorkingContext,
    /// Faits semantiques actuellement valides.
    CurrentFacts,
    /// Procedures explicitement demandees.
    Procedures,
    /// Episodes pertinents.
    RecentEvents,
}

/// Etat d'une fenetre temporelle au moment de la compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContextTemporalStatus {
    /// La fenetre contient l'instant de compilation.
    Current,
    /// La memoire ne sera valide que dans le futur.
    Scheduled,
    /// La fin de validite est atteinte ou depassee.
    Expired,
}

impl ContextSectionKind {
    pub(super) fn from_layer(layer: MemoryLayer) -> Self {
        match layer {
            MemoryLayer::ShortTerm => Self::WorkingContext,
            MemoryLayer::Semantic => Self::CurrentFacts,
            MemoryLayer::Procedural => Self::Procedures,
            MemoryLayer::Episodic => Self::RecentEvents,
        }
    }

    pub(super) const fn order(self) -> u8 {
        match self {
            Self::WorkingContext => 0,
            Self::CurrentFacts => 1,
            Self::Procedures => 2,
            Self::RecentEvents => 3,
        }
    }

    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::WorkingContext => "Working context",
            Self::CurrentFacts => "Current facts",
            Self::Procedures => "Procedures",
            Self::RecentEvents => "Recent events",
        }
    }
}

/// Item de contexte selectionne.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ContextItem {
    /// Texte normalise sur une ligne pour un rendu stable.
    pub text: String,
    /// Tous les souvenirs representes par cet item.
    pub source_memory_ids: Vec<String>,
    /// Couche du representant conserve.
    pub layer: MemoryLayer,
    /// Provenance du representant conserve.
    pub trust: TrustLevel,
    /// Role derive uniquement de la couche et de la provenance.
    pub role: ContextRole,
    /// Fenetre temporelle persistante du representant.
    pub validity: Validity,
    /// Etat de cette fenetre au moment de la compilation.
    pub temporal_status: ContextTemporalStatus,
    /// Score brut du recall hybride.
    pub retrieval_score: f32,
    /// Rang zero-based dans le recall hybride avant compilation.
    pub retrieval_rank: usize,
    /// Contributions de retrieval du representant et de ses doublons exacts.
    pub retrieval_contributions: Vec<RetrievalContribution>,
    /// Cout conservateur du document mono-item dans le format demande.
    pub estimated_tokens: usize,
    /// Utilite de compilation normalisee, distincte du score de retrieval.
    pub utility_score: f64,
    /// Utilite rapportee au cout du document mono-item.
    pub value_per_token: f64,
    /// Facteur temporel borne utilise dans l'utilite.
    pub freshness_score: f64,
    /// Decision deterministe ayant introduit l'item dans la selection.
    pub inclusion_reason: InclusionReason,
}

/// Contribution d'un souvenir au candidat compile.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RetrievalContribution {
    /// Identifiant du souvenir rappele.
    pub memory_id: String,
    /// Rang zero-based avant filtrage et deduplication.
    pub retrieval_rank: usize,
    /// Score brut fini utilise par le compiler, ou zero apres sanitation.
    pub retrieval_score: f32,
}

/// Raison deterministe d'inclusion d'un item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InclusionReason {
    /// Meilleur candidat reserve pour representer une section.
    SectionReservation,
    /// Ajout lors du remplissage par utilite rapportee au cout.
    ValuePerToken,
    /// Ajout par remplacement local augmentant l'utilite.
    LocalReplacement,
}

/// Groupe ordonne d'items de meme fonction dans le prompt.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ContextSection {
    /// Nature de la section.
    pub kind: ContextSectionKind,
    /// Items dans l'ordre du recall.
    pub items: Vec<ContextItem>,
}

/// Lien entre un fragment du bundle et un souvenir persiste.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContextCitation {
    /// Identifiant persiste du souvenir.
    pub memory_id: String,
    /// Section dans laquelle il est utilise.
    pub section: ContextSectionKind,
}

/// Raison deterministe pour laquelle un candidat n'est pas rendu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExclusionReason {
    /// La provenance ne respecte pas la politique demandee.
    SourceFiltered,
    /// La fenetre temporelle ne contient pas l'instant de compilation.
    NotCurrentlyValid,
    /// L'ajout aurait depasse le budget estime.
    TokenBudget,
    /// Le quota non nul du role pour le profil est atteint.
    ProfileQuota,
}

/// Candidat exclu, present uniquement en mode explicatif.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ExcludedMemory {
    /// Identifiant du candidat.
    pub memory_id: String,
    /// Motif d'exclusion.
    pub reason: ExclusionReason,
    /// Etat temporel du candidat au moment de la compilation.
    pub temporal_status: ContextTemporalStatus,
    /// Role derive des metadonnees explicites.
    pub role: ContextRole,
    /// Contribution de retrieval disponible pour expliquer la decision.
    pub retrieval_contribution: RetrievalContribution,
}

/// Trace d'un souvenir fusionne dans un representant de memes metadonnees.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MergedMemory {
    /// Identifiant du souvenir absorbe par la deduplication exacte.
    pub memory_id: String,
    /// Identifiant du representant qui porte le texte dans le bundle.
    pub representative_memory_id: String,
}

/// Cluster complet produit par la deduplication exacte.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DedupCluster {
    /// Identifiant portant le texte dans le candidat compile.
    pub representative_memory_id: String,
    /// Tous les identifiants du cluster, representant inclus.
    pub memory_ids: Vec<String>,
}

/// Avertissement conservateur produit uniquement depuis des metadonnees.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContextWarning {
    /// Un meme contenu normalise porte des metadonnees incompatibles.
    ///
    /// Le compiler ne choisit pas une verite et ne deduit aucune contradiction
    /// semantique depuis le texte.
    IncompatibleMetadata { memory_ids: Vec<String> },
}

/// Resume toujours present de la compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContextTraceSummary {
    /// Nombre d'items finaux.
    pub included_items: usize,
    /// Nombre d'identifiants sources inclus.
    pub included_memories: usize,
    /// Nombre de souvenirs exclus.
    pub excluded_memories: usize,
    /// Nombre de clusters ayant absorbe au moins un doublon.
    pub dedup_clusters: usize,
    /// Nombre d'avertissements conservateurs.
    pub warnings: usize,
}

/// Evenement individuel d'une trace detaillee.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ContextTraceEvent {
    /// Item retenu avec sa raison et ses contributions.
    Included {
        memory_id: String,
        role: ContextRole,
        reason: InclusionReason,
        contributions: Vec<RetrievalContribution>,
    },
    /// Souvenir exclu.
    Excluded(ExcludedMemory),
    /// Fusion exacte.
    Deduplicated(DedupCluster),
    /// Avertissement non resolu.
    Warning(ContextWarning),
}

/// Trace compacte ou detaillee, avec taille detaillee strictement bornee.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ContextTrace {
    /// Niveau demande.
    pub level: ContextTraceLevel,
    /// Compteurs calcules avant troncature.
    pub summary: ContextTraceSummary,
    /// Evenements individuels en mode detaille uniquement.
    pub events: Vec<ContextTraceEvent>,
    /// Nombre total d'evenements avant troncature.
    pub total_events: usize,
    /// Indique que des evenements ont ete omis.
    pub truncated: bool,
}

/// Contexte compile, directement consommable et inspectable.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ContextBundle {
    /// Sections structures dans leur ordre de rendu.
    pub sections: Vec<ContextSection>,
    /// Rendu Markdown final.
    pub rendered: String,
    /// Cout du rendu complet selon l'estimateur utilise.
    pub estimated_tokens: usize,
    /// Profil effectivement applique.
    pub profile: ContextProfile,
    /// Format effectivement rendu.
    pub render_format: ContextRenderFormat,
    /// Timestamp Unix UTC utilise pour les decisions temporelles.
    pub compiled_at: i64,
    /// Somme des utilites des items retenus.
    pub total_utility: f64,
    /// Citations de tous les IDs sources inclus.
    pub citations: Vec<ContextCitation>,
    /// Fusions exactes, separees des exclusions reelles.
    pub merged: Vec<MergedMemory>,
    /// Exclusions detaillees lorsque `ContextRequest::explain` est actif.
    pub excluded: Vec<ExcludedMemory>,
    /// Clusters complets de deduplication exacte.
    pub dedup_clusters: Vec<DedupCluster>,
    /// Avertissements fondes uniquement sur les metadonnees explicites.
    pub warnings: Vec<ContextWarning>,
    /// Trace compacte par defaut ou detaillee et bornee.
    pub trace: ContextTrace,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_derived_only_from_layer_and_provenance() {
        assert_eq!(
            ContextRole::derive(MemoryLayer::Semantic, TrustLevel::User),
            ContextRole::Fact
        );
        assert_eq!(
            ContextRole::derive(MemoryLayer::ShortTerm, TrustLevel::Unknown),
            ContextRole::Constraint
        );
        assert_eq!(
            ContextRole::derive(MemoryLayer::Procedural, TrustLevel::Import),
            ContextRole::Procedure
        );
        assert_eq!(
            ContextRole::derive(MemoryLayer::Episodic, TrustLevel::Unknown),
            ContextRole::Event
        );
        assert_eq!(
            ContextRole::derive(MemoryLayer::Semantic, TrustLevel::Import),
            ContextRole::Reference
        );
        assert_eq!(
            ContextRole::derive(MemoryLayer::Semantic, TrustLevel::Unknown),
            ContextRole::UncertainData
        );
    }

    #[test]
    fn request_defaults_remain_balanced_markdown_and_compact() {
        let request = ContextRequest::new("query", 100);
        assert_eq!(request.compilation_profile(), ContextProfile::Balanced);
        assert_eq!(request.requested_render_format(), ContextRenderFormat::Markdown);
        assert_eq!(request.requested_trace_level(), ContextTraceLevel::Compact);
    }
}
