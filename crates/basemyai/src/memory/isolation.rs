//! Isolation multi-agent. Chaque ligne porte un `agent_id` ; **toute** lecture
//! et écriture sont filtrées par lui **au niveau SQL** (ADR-006). Une fuite
//! cross-agent est un incident de sécurité, pas un bug fonctionnel.

/// Identifiant d'agent (tenant logique d'une mémoire).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentId(String);

impl AgentId {
    /// Construit un `AgentId`. Vide => `None` (un agent valide est requis).
    #[must_use]
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        if id.is_empty() { None } else { Some(Self(id)) }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
