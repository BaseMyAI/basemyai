//! Données de test répétées par plusieurs suites (bornes de validation,
//! payloads de référence).

/// `agent_id` dépassant `MAX_AGENT_ID_LEN` (128) d'un caractère.
#[must_use]
pub(crate) fn overlong_agent_id() -> String {
    "a".repeat(129)
}

/// `text` dépassant la limite `remember` (65 536 caractères) d'un caractère.
#[must_use]
pub(crate) fn overlong_text() -> String {
    "x".repeat(65_537)
}
