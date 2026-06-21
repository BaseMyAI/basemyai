//! Sortie text/JSON. Chaque commande construit sa propre `serde_json::Value`
//! pour le mode `json` (pas de `derive(Serialize)` ajouté dans la lib
//! `basemyai` — zéro risque sur son API publique) et imprime du texte humain
//! sinon. Mode résolu une fois (flag CLI > `BASEMYAI_FORMAT` > `text`).

use clap::ValueEnum;

#[derive(Copy, Clone, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum Format {
    #[default]
    Text,
    Json,
}

impl Format {
    /// Résout le format effectif : flag explicite, sinon `BASEMYAI_FORMAT`, sinon texte.
    #[must_use]
    pub(crate) fn resolve(explicit: Option<Format>) -> Self {
        if let Some(f) = explicit {
            return f;
        }
        match std::env::var("BASEMYAI_FORMAT").as_deref() {
            Ok("json") => Format::Json,
            _ => Format::Text,
        }
    }

    /// Imprime soit le résultat humain (via `human`), soit `value` sérialisé.
    pub(crate) fn print(self, human: impl FnOnce(), value: impl FnOnce() -> serde_json::Value) {
        match self {
            Format::Text => human(),
            Format::Json => println!("{}", value()),
        }
    }

    /// Imprime une erreur sur stderr quel que soit le format : texte
    /// (comme avant) ou JSON (`{"error": "..."}`, code de sortie inchangé).
    pub(crate) fn print_error(self, message: &str) {
        match self {
            Format::Text => eprintln!("error: {message}"),
            Format::Json => eprintln!("{}", serde_json::json!({ "error": message })),
        }
    }
}
