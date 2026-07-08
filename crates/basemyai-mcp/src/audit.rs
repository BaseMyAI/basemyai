// SPDX-License-Identifier: BUSL-1.1
//! Journal d'audit des appels d'outils MCP.
//!
//! **Invariant de confidentialité** : on ne logue **jamais** le contenu —
//! ni `text`, ni vecteurs, ni résultats bruts. Seulement des métadonnées :
//! nom d'outil, `agent_id`, issue (ok/erreur), durée. C'est suffisant pour
//! l'observabilité et le débogage, sans fuite de données mémoire.

/// Issue d'un appel d'outil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Appel réussi.
    Ok,
    /// Appel en erreur (la nature de l'erreur n'est pas loguée ici).
    Error,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

/// Émet une ligne d'audit structurée. **Ne logue jamais** de contenu mémoire.
pub(crate) fn emit_audit(tool: &str, agent_id: &str, outcome: Outcome, time_ms: u64) {
    tracing::info!(
        target: "basemyai_mcp::audit",
        tool,
        agent_id,
        outcome = outcome.as_str(),
        time_ms,
        "mcp_tool_call"
    );
}
