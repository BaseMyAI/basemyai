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
        /// Crée le wrap passphrase Argon2id avec le profil contraint
        /// 19 MiB/t2/p1. Ce choix est explicite et n'affecte pas les défauts.
        #[arg(long)]
        low_memory: bool,
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
    /// Compile un recall hybride en contexte borné et traçable, prêt pour un
    /// agent (Context Engine — déterministe, sans LLM).
    Context {
        /// Texte de la requête.
        query: String,
        /// Budget de tokens estimé, dur (jamais dépassé).
        #[arg(long)]
        token_budget: usize,
        /// Taille du pool de candidats du recall hybride sous-jacent.
        #[arg(long, default_value_t = 64)]
        candidate_limit: usize,
        /// Inclut explicitement la couche procédurale dans le recall.
        #[arg(long)]
        include_procedural: bool,
        /// Politique de filtrage de provenance appliquée après le recall.
        #[arg(long, value_enum, default_value_t = ContextSourcePolicyArg::ExcludeImported)]
        source_policy: ContextSourcePolicyArg,
        /// Profil de compilation : poids et quotas par rôle, jamais des permissions.
        #[arg(long, value_enum, default_value_t = ContextProfileArg::Balanced)]
        profile: ContextProfileArg,
        /// Format du contenu compilé (indépendant de `--format`, qui contrôle
        /// la sortie de la CLI elle-même).
        #[arg(long = "render", value_enum, default_value_t = ContextRenderFormatArg::Markdown)]
        render_format: ContextRenderFormatArg,
        /// Conserve une trace détaillée et bornée (raisons d'inclusion/
        /// exclusion, contributions de retrieval, clusters de déduplication,
        /// avertissements).
        #[arg(long)]
        explain: bool,
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
        /// Nouveau secret (sinon `BASEMYAI_DB_KEY` / résolution ADR-034).
        #[arg(long)]
        new_key: Option<String>,
        /// Interprète le nouveau secret comme une passphrase Argon2id.
        #[arg(long)]
        passphrase: bool,
        /// Utilise le profil Argon2id contraint 19 MiB/t2/p1. À répéter à
        /// chaque rotation qui doit conserver ce profil.
        #[arg(long, requires = "passphrase")]
        low_memory: bool,
        /// Ré-encrypte toutes les données sous une nouvelle DEK (O(taille)).
        #[arg(long)]
        full: bool,
    },
    /// Graphe entités/relations d'un agent.
    Graph {
        #[command(subcommand)]
        action: GraphAction,
    },
    /// Consolidation épisodes → faits + graphe, via le meilleur LLM local détecté.
    Consolidate,
    /// Oubli adaptatif (VISION §5.2, ADR-037) : évince physiquement les
    /// souvenirs actifs les moins bien notés (`importance + H/(H+age)`)
    /// au-delà d'une capacité par agent. Une passe manuelle, ponctuelle — la
    /// même politique tourne en tâche de fond via `AdaptiveForgettingTask`
    /// (bindings/surfaces qui font tourner un `MaintenanceWorker`).
    ForgetAdaptive {
        /// Nombre maximum de souvenirs actifs conservés pour l'agent ; le
        /// reste est évincé, du moins bien noté au mieux noté.
        #[arg(long)]
        capacity: usize,
        /// Demi-vie de récence en secondes (`H` dans le score de rétention).
        #[arg(long, default_value_t = 86_400)]
        half_life_secs: i64,
        /// N'évince rien : calcule et affiche ce qui serait évincé.
        #[arg(long)]
        dry_run: bool,
    },
    /// GC temporel (ADR-038) : supprime physiquement les souvenirs de
    /// l'agent dont `valid_until` est déjà passé (invalidés explicitement ou
    /// expirés par leur fenêtre de validité). Traité par pages bornées.
    /// N'affecte jamais les souvenirs actifs (aucun chevauchement avec
    /// `forget-adaptive`).
    Gc {
        /// Taille de page du scan/de la suppression (bornée, jamais un
        /// balayage non borné en un seul passage).
        #[arg(long, default_value_t = basemyai::maintenance::DEFAULT_GC_PAGE_SIZE)]
        page_size: usize,
        /// Ne supprime rien : calcule et affiche ce qui serait supprimé.
        #[arg(long)]
        dry_run: bool,
    },
    /// Vérifie un `.bmai` : métadonnées de conteneur + intégrité moteur
    /// (ADR-040). Par défaut `Quick` (O(métadonnées)) ; `--physical` décode
    /// chaque bloc de données ; `--logical` (implique `--physical`) vérifie
    /// en plus la cohérence inter-structures (record/vecmap/FTS/graphe).
    Verify {
        /// Décode chaque bloc de données (`VerifyMode::FullPhysical`).
        #[arg(long)]
        physical: bool,
        /// Cohérence inter-structures complète (`VerifyMode::FullLogical`).
        #[arg(long)]
        logical: bool,
    },
    /// Analyse l'intégrité du conteneur et affiche le plan de réparation des
    /// index dérivés (jamais les données primaires) — n'écrit rien
    /// (ADR-040 §3). Sans `--dry-run`, applique le plan si aucune donnée
    /// primaire n'est à risque (sinon refuse : restaurer depuis un export).
    Repair {
        /// N'applique rien : calcule et affiche le plan de réparation.
        #[arg(long)]
        dry_run: bool,
    },
    /// Reconstruit sans condition les index dérivés (vecmap/allocateur, FTS,
    /// graphe DiskANN) depuis les souvenirs primaires (ADR-040 §3). Les
    /// souvenirs dont le vecteur est perdu sont listés pour ré-embedding
    /// plutôt que réinventés — le moteur n'a pas de modèle par design.
    RebuildIndexes,
    /// Compacte le store : fusion complète en un seul SST, tombstones purgés
    /// (`Engine::compact_now`, ADR-040/N9.4).
    Compact,
    /// Recalcule et réécrit des vecteurs via le modèle d'embedding réel
    /// (charge Candle, contrairement aux autres commandes de maintenance
    /// d'intégrité). Sans flag : réembed tous les souvenirs actuellement
    /// signalés `reembedding_required` par `repair`/`rebuild-indexes`
    /// (relance ces deux commandes elle-même — inutile de les enchaîner à la
    /// main), portée = tout le conteneur. Avec `--all` ou `--ids`, réembed
    /// sans condition (ex. changement de modèle) — portée = l'agent résolu
    /// (`--agent`/config), `--ids` seul un sous-ensemble.
    Reembed {
        /// Réembed tous les souvenirs de l'agent résolu, sans condition.
        #[arg(long, conflicts_with = "ids")]
        all: bool,
        /// Réembed ces souvenirs de l'agent résolu, sans condition (liste séparée par des virgules).
        #[arg(long, value_delimiter = ',')]
        ids: Vec<String>,
    },
    /// Applique les migrations de schéma en attente (idempotent).
    Migrate,
    /// Helpers de provisionnement LLM local (consolidation).
    Llm {
        #[command(subcommand)]
        action: LlmAction,
    },
    /// Recall Quality Lab (`basemyai-eval`) : exécute ou compare un dataset
    /// déterministe hors ligne (recall + Context Engine, sans LLM ni réseau).
    /// Nécessite la feature de build `eval-lab` (off par défaut dans les
    /// binaires distribués — active `basemyai/test-util`).
    #[cfg(feature = "eval-lab")]
    Eval {
        #[command(subcommand)]
        action: EvalAction,
    },
    /// Génère les complétions shell (à sourcer dans le profil du shell).
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[cfg(feature = "eval-lab")]
#[derive(Subcommand)]
pub(crate) enum EvalAction {
    /// Exécute un dataset JSONL versionné contre recall et Context Engine.
    Run {
        /// Dataset JSONL (schéma documenté dans `docs/recall-quality-lab.md`).
        dataset: PathBuf,
        /// Rapport JSON agrégé + par cas.
        #[arg(long)]
        output: PathBuf,
        /// Rapport Markdown lisible, en plus du JSON.
        #[arg(long)]
        human: Option<PathBuf>,
        /// Enregistre la latence murale (`latency_micros`) ; absent, le
        /// rapport reste byte-stable d'un run à l'autre.
        #[arg(long)]
        timings: bool,
    },
    /// Compare les métriques agrégées de deux rapports JSON (`eval run --output`).
    Compare {
        /// Rapport de référence.
        baseline: PathBuf,
        /// Rapport courant.
        current: PathBuf,
        /// Écrit la comparaison en JSON.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Écrit la comparaison en Markdown.
        #[arg(long)]
        human: Option<PathBuf>,
        /// Sort en échec si une métrique de qualité régresse ou si le nombre
        /// de cas échoués augmente.
        #[arg(long)]
        fail_on_regression: bool,
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

/// Politique de provenance exposée en CLI (miroir de `basemyai::ContextSourcePolicy`).
#[derive(Copy, Clone, ValueEnum)]
pub(crate) enum ContextSourcePolicyArg {
    AllowAll,
    ExcludeImported,
    UserAndConsolidationOnly,
}

/// Profil de compilation exposé en CLI (miroir de `basemyai::ContextProfile`).
#[derive(Copy, Clone, ValueEnum)]
pub(crate) enum ContextProfileArg {
    Balanced,
    Conversation,
    Coding,
    Execution,
    SafetyCritical,
}

/// Format de rendu exposé en CLI (miroir de `basemyai::ContextRenderFormat`).
#[derive(Copy, Clone, ValueEnum)]
pub(crate) enum ContextRenderFormatArg {
    Text,
    Markdown,
    Json,
}
