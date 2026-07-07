// SPDX-License-Identifier: BUSL-1.1
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
    /// (`error: <message>`) ou JSON (`{"error": {"code", "message"}}` — `code`
    /// est le contrat stable, documenté dans `docs/cli.md` ; ne pas parser
    /// `message`, qui peut changer de formulation).
    pub(crate) fn print_error(self, err: &crate::error::CliError) {
        match self {
            Format::Text => eprintln!("error: {err}"),
            Format::Json => {
                eprintln!(
                    "{}",
                    serde_json::json!({ "error": { "code": err.code(), "message": err.to_string() } })
                );
            }
        }
    }
}
