// SPDX-License-Identifier: BUSL-1.1
//! Schéma d'arguments (`clap` derive) : structure pure, zéro logique. Le
//! dispatch vers les commandes vit dans `crate::commands`.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};

use crate::output::Format;
use crate::ui::ColorMode;

/// CLI développeur de BaseMyAI — la base mémoire privée pour agents IA.
#[derive(Parser)]
#[command(name = "basemyai", version, about, long_about = None)]
pub(crate) struct Cli {
    /// Chemin du conteneur `.bmai`. Si omis : `BASEMYAI_DB_PATH`, sinon `~/.basemyai/config.toml`.
    #[arg(long, global = true)]
    pub(crate) db: Option<PathBuf>,
    /// Identifiant de l'agent. Si omis : `BASEMYAI_AGENT`, sinon `~/.basemyai/config.toml`.
    #[arg(long, global = true)]
    pub(crate) agent: Option<String>,
    /// Format de sortie. Si omis : `BASEMYAI_FORMAT`, sinon `text`.
    #[arg(long, global = true, value_enum)]
    pub(crate) format: Option<Format>,
    /// Politique couleur (respecte aussi NO_COLOR si `auto`).
    #[arg(long, global = true, value_enum, default_value_t = ColorMode::Auto)]
    pub(crate) color: ColorMode,
    /// Supprime la sortie informative en mode texte.
    #[arg(short, long, global = true, default_value_t = false)]
    pub(crate) quiet: bool,
    /// Niveau de logs de diagnostic sur stderr (`-v`, `-vv`).
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    pub(crate) verbose: u8,
    /// Désactive les spinners/barres de progression.
    #[arg(long, global = true, default_value_t = false)]
    pub(crate) no_progress: bool,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Provisionne le modèle d'embedding baseline (détection matérielle).
    Setup {
        /// Consent explicite au téléchargement du modèle s'il est absent.
        #[arg(long)]
        fetch: bool,
    },
    /// Affiche la config de provisionnement persistée et le matériel détecté.
    Status,
    /// Gère `~/.basemyai/config.toml` (`db-path`, `agent` par défaut).
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Crée un nouveau conteneur `.bmai` chiffré (migrations + métadonnées).
    Init {
        /// Chemin du fichier `.bmai` à créer.
        path: PathBuf,
    },
    /// Inspecte un `.bmai` : métadonnées du conteneur + nombre de souvenirs.
    Inspect,
    /// Statistiques mémoire par agent (souvenirs valides).
    Stats,
    /// Mémorise un texte pour un agent.
    Remember {
        /// Couche mémoire cible.
        #[arg(long, value_enum, default_value_t = Layer::Semantic)]
        layer: Layer,
        /// Texte à mémoriser (incompatible avec `--file`).
        #[arg(required_unless_present = "file")]
        text: Option<String>,
        /// Fichier (une ligne = un souvenir) ou `-` pour stdin ; ingestion en lot.
        #[arg(long, conflicts_with = "text")]
        file: Option<String>,
    },
    /// Rappelle des souvenirs d'un agent par requête sémantique.
    Recall {
        /// Texte de la requête.
        query: String,
        /// Nombre de résultats.
        #[arg(short, long, default_value_t = 5)]
        k: usize,
        /// Rappel hybride (vecteur + BM25 fusionnés par RRF).
        #[arg(long)]
        hybrid: bool,
        /// Filtre sur une seule couche mémoire.
        #[arg(long, value_enum)]
        layer: Option<Layer>,
        /// Limite aux souvenirs mentionnant une entité du graphe.
        #[arg(long)]
        graph: bool,
    },
    /// Liste les souvenirs bruts d'un agent (sans recherche sémantique).
    List {
        /// Filtre sur une seule couche mémoire.
        #[arg(long, value_enum)]
        layer: Option<Layer>,
        /// Nombre maximum de résultats.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Inclut les souvenirs invalidés/expirés.
        #[arg(long)]
        include_invalid: bool,
    },
    /// Suppression physique d'un souvenir (RGPD, droit à l'effacement).
    Forget {
        /// Identifiant du souvenir (affiché par `recall`/`list`).
        id: String,
    },
    /// Invalide un souvenir (soft-delete : `valid_until = now`).
    Invalidate {
        /// Identifiant du souvenir (affiché par `recall`/`list`).
        id: String,
    },
    /// Purge **toutes** les données d'un agent (mémoire + graphe). Irréversible.
    Purge {
        /// Confirmation explicite obligatoire (sinon refusé).
        #[arg(long)]
        yes: bool,
    },
    /// Exporte la mémoire d'un agent en JSONL versionné (backup/migration).
    Export {
        /// Fichier de sortie ; stdout si omis.
        #[arg(long)]
        out: Option<String>,
    },
    /// Importe un export JSONL dans la mémoire d'un agent (ré-embedding, idempotent).
    Import {
        /// Fichier JSONL ou `-` pour stdin.
        #[arg(long)]
        file: String,
        /// Autorise l'import de souvenirs `procedural` (audit memory poisoning).
        #[arg(long)]
        trusted: bool,
    },
    /// Change la clé de chiffrement du conteneur `.bmai` en place (ADR-030).
    RotateKey {
        /// Nouvelle passphrase (sinon `BASEMYAI_DB_KEY` / résolution ADR-034).
        #[arg(long)]
        new_key: Option<String>,
    },
    /// Graphe entités/relations d'un agent.
    Graph {
        #[command(subcommand)]
        action: GraphAction,
    },
    /// Consolidation épisodes → faits + graphe, via le meilleur LLM local détecté.
    Consolidate,
    /// Oubli adaptatif (VISION §5.2, ADR-037) : évince physiquement les
    /// souvenirs les moins bien notés (`importance + H/(H+age)`) au-delà
    /// d'une capacité par agent. Une passe manuelle, ponctuelle — la même
    /// politique tourne en tâche de fond via `AdaptiveForgettingTask`
    /// (bindings/surfaces qui font tourner un `MaintenanceWorker`).
    ForgetAdaptive {
        /// Nombre maximum de souvenirs conservés pour l'agent ; le reste est
        /// évincé, du moins bien noté au mieux noté.
        #[arg(long)]
        capacity: usize,
        /// Demi-vie de récence en secondes (`H` dans le score de rétention).
        #[arg(long, default_value_t = 86_400)]
        half_life_secs: i64,
    },
    /// Vérifie un `.bmai` : conteneur valide, version de format attendue.
    Verify,
    /// Applique les migrations de schéma en attente (idempotent).
    Migrate,
    /// Helpers de provisionnement LLM local (consolidation).
    Llm {
        #[command(subcommand)]
        action: LlmAction,
    },
    /// Génère les complétions shell (à sourcer dans le profil du shell).
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigAction {
    /// Affiche la config effective (fichier + environnement).
    Show,
    /// Définit `db-path` ou `agent` dans `~/.basemyai/config.toml`.
    Set { key: String, value: String },
    /// Retire `db-path` ou `agent` de `~/.basemyai/config.toml`.
    Unset { key: String },
    /// Gestion de la passphrase de chiffrement (`~/.basemyai/key`, ADR-034).
    Key {
        #[command(subcommand)]
        action: ConfigKeyAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigKeyAction {
    /// Génère une passphrase et l'écrit dans `~/.basemyai/key` (jamais affichée).
    Generate {
        /// Remplace un fichier existant.
        #[arg(long)]
        force: bool,
    },
    /// Affiche le chemin du fichier de clé par défaut.
    Path,
    /// Vérifie qu'une passphrase est résolvable (sans l'afficher).
    Check,
}

#[derive(Subcommand)]
pub(crate) enum GraphAction {
    /// Insère ou met à jour une entité (nœud).
    AddEntity { id: String, kind: String, label: String },
    /// Crée ou met à jour une relation orientée `src -[relation]-> dst`.
    AddEdge {
        src: String,
        relation: String,
        dst: String,
        #[arg(long, default_value_t = 1.0)]
        weight: f64,
    },
    /// Traversée multi-sauts depuis une entité de départ (BFS natif, profondeur bornée).
    Traverse {
        start: String,
        #[arg(long, default_value_t = 3)]
        depth: u32,
    },
}

#[derive(Subcommand)]
pub(crate) enum LlmAction {
    /// Détecte les serveurs LLM locaux et le meilleur modèle pour la machine.
    Detect,
    /// Suggère des modèles installables adaptés au matériel.
    Suggest,
}

/// Couches mémoire exposées en CLI (miroir de `basemyai::MemoryLayer`).
#[derive(Copy, Clone, ValueEnum)]
pub(crate) enum Layer {
    ShortTerm,
    Episodic,
    Procedural,
    Semantic,
}
