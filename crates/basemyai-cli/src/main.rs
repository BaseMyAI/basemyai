// SPDX-License-Identifier: BUSL-1.1
//! Binaire `basemyai` : CLI développeur de la base mémoire BaseMyAI.
//!
//! Donne accès en ligne de commande au cœur memory database : provisionnement
//! du modèle d'embedding (hardware-aware, sans download silencieux — ADR-010),
//! création/inspection/vérification d'un conteneur `.bmai` chiffré (ADR-019),
//! le cycle de vie complet d'un souvenir (`remember`/`recall`/`list`/`forget`/
//! `invalidate`/`purge`/`export`/`import`), le graphe entités/relations
//! (`graph`), les tâches de maintenance one-shot (`maintenance`) et la
//! consolidation (`consolidate`).
//!
//! ## Chiffrement obligatoire
//!
//! Toute commande qui ouvre un fichier `.bmai` exige la clé via la variable
//! d'environnement `BASEMYAI_DB_KEY` (chiffrement au repos, ADR-007). Aucune
//! commande n'ouvre un fichier en clair.
//!
//! ## Réduction de friction
//!
//! `--db`/`--agent` sont des flags globaux : s'ils sont omis, ils sont résolus
//! via `BASEMYAI_DB_PATH`/`BASEMYAI_AGENT`, sinon via `~/.basemyai/config.toml`
//! (`basemyai config set db-path|agent <value>`). `init` reste positional :
//! créer un conteneur sans dire où serait dangereux.
//!
//! ## Sortie machine-readable
//!
//! `--format json` (ou `BASEMYAI_FORMAT=json`) bascule chaque commande vers une
//! sortie JSON sur stdout — pensé pour qu'un agent IA appelle ce CLI comme un
//! outil sans parser du texte humain.
//!
//! ## Features
//!
//! Le chemin réel exige `crypto` (chiffrement libSQL) et `embed` (embedder
//! Candle) — tous deux dans le set par défaut. Sans eux, le binaire se contente
//! d'afficher l'aide et une erreur explicite.
//!
//! ## Layout
//!
//! `cli` (schéma d'arguments) → `commands::dispatch` (routage) → un module par
//! domaine sous `commands/` (`config`, `container`, `memory`, `graph`,
//! `maintenance`, `provision`), chacun s'appuyant sur les helpers partagés de
//! `context` (ouverture clé/store/mémoire) et sur `error`/`exit` pour le
//! contrat de sortie scriptable. `persisted_config` gère
//! `~/.basemyai/config.toml`.

mod cli;
mod commands;
mod context;
mod error;
mod exit;
mod output;
mod persisted_config;

use clap::Parser;
use cli::Cli;
use output::Format;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start async runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let format = Format::resolve(cli.format);
    match runtime.block_on(commands::dispatch(cli, format)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            format.print_error(&e);
            std::process::ExitCode::from(e.exit_code())
        }
    }
}
