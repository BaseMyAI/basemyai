//! Codes de sortie stables du CLI. Un script qui appelle `basemyai` peut
//! brancher sur ces codes sans parser de message — c'est le contrat, donc ils
//! ne changent jamais de sens une fois publiés (on en ajoute, on n'en réutilise
//! pas). Documentés dans `docs/cli.md`.

/// Erreur interne / non catégorisée (stockage, embedding, IO...).
pub(crate) const GENERIC: u8 = 1;
/// Combinaison de flags invalide (ex. `--hybrid`/`--layer`/`--graph` ensemble),
/// clé de config inconnue. Même famille que les erreurs d'usage clap (exit 2).
pub(crate) const USAGE: u8 = 2;
/// Clé de chiffrement absente ou rejetée par le conteneur (`BASEMYAI_DB_KEY`).
pub(crate) const KEY_ERROR: u8 = 3;
/// `--db`/`--agent` non résolvable (ni flag, ni env, ni config).
pub(crate) const NOT_CONFIGURED: u8 = 4;
/// Entrée invalide au sens métier (agent vide, texte trop long...).
pub(crate) const VALIDATION: u8 = 5;
/// La cible existe déjà (`init` sur un chemin déjà présent).
pub(crate) const ALREADY_EXISTS: u8 = 6;
/// Action destructive refusée sans confirmation explicite (`purge` sans `--yes`).
pub(crate) const CONFIRMATION_REQUIRED: u8 = 7;
/// Modèle d'embedding non provisionné — `basemyai setup --fetch` requis.
pub(crate) const MODEL_NOT_PROVISIONED: u8 = 8;
/// Aucun backend LLM local détecté — `basemyai llm detect` pour diagnostiquer.
pub(crate) const LLM_NOT_AVAILABLE: u8 = 9;
/// `verify` : le conteneur s'ouvre mais ne respecte pas le format `.bmai` attendu.
pub(crate) const VERIFICATION_FAILED: u8 = 10;
