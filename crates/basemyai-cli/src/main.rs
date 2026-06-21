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

mod cli_config;
mod commands_graph;
mod commands_maintenance;
mod commands_memory;
mod output;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use cli_config::CliConfig;
use output::Format;

/// CLI développeur de BaseMyAI — la base mémoire privée pour agents IA.
#[derive(Parser)]
#[command(name = "basemyai", version, about, long_about = None)]
struct Cli {
    /// Chemin du conteneur `.bmai`. Si omis : `BASEMYAI_DB_PATH`, sinon `~/.basemyai/config.toml`.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Identifiant de l'agent. Si omis : `BASEMYAI_AGENT`, sinon `~/.basemyai/config.toml`.
    #[arg(long, global = true)]
    agent: Option<String>,
    /// Format de sortie. Si omis : `BASEMYAI_FORMAT`, sinon `text`.
    #[arg(long, global = true, value_enum)]
    format: Option<Format>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    },
    /// Graphe entités/relations d'un agent.
    Graph {
        #[command(subcommand)]
        action: GraphAction,
    },
    /// Tâches de maintenance one-shot (GC, oubli adaptatif).
    Maintenance {
        #[command(subcommand)]
        action: MaintenanceAction,
    },
    /// Consolidation épisodes → faits + graphe, via le meilleur LLM local détecté.
    Consolidate,
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
enum ConfigAction {
    /// Affiche la config effective (fichier + environnement).
    Show,
    /// Définit `db-path` ou `agent` dans `~/.basemyai/config.toml`.
    Set { key: String, value: String },
    /// Retire `db-path` ou `agent` de `~/.basemyai/config.toml`.
    Unset { key: String },
}

#[derive(Subcommand)]
enum GraphAction {
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
    /// Traversée multi-sauts depuis une entité de départ (CTE récursive).
    Traverse {
        start: String,
        #[arg(long, default_value_t = 3)]
        depth: u32,
    },
}

#[derive(Subcommand)]
enum MaintenanceAction {
    /// Supprime les souvenirs expirés (`valid_until` passé).
    Gc,
    /// Évince les souvenirs les moins bien notés (importance × récence) au-delà d'un plafond par agent.
    ForgetAdaptive {
        #[arg(long)]
        capacity: usize,
        #[arg(long, default_value_t = 30 * 24 * 3600)]
        half_life_secs: i64,
    },
}

#[derive(Subcommand)]
enum LlmAction {
    /// Détecte les serveurs LLM locaux et le meilleur modèle pour la machine.
    Detect,
    /// Suggère des modèles installables adaptés au matériel.
    Suggest,
}

/// Couches mémoire exposées en CLI (miroir de `basemyai::MemoryLayer`).
#[derive(Copy, Clone, ValueEnum)]
enum Layer {
    ShortTerm,
    Episodic,
    Procedural,
    Semantic,
}

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
    match runtime.block_on(execute(cli, format)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            format.print_error(&e.to_string());
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn execute(cli: Cli, format: Format) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = CliConfig::load();
    let resolve_path = || -> Result<PathBuf, Box<dyn std::error::Error>> {
        cfg.resolve_path(cli.db.clone()).map_err(Into::into)
    };
    let resolve_agent = || -> Result<String, Box<dyn std::error::Error>> {
        cfg.resolve_agent(cli.agent.clone()).map_err(Into::into)
    };

    match cli.command {
        Command::Setup { fetch } => cmd_setup(fetch, format).await,
        Command::Status => cmd_status(format).await,
        Command::Config { action } => cmd_config(action, format, cli.db.clone(), cli.agent.clone()),
        Command::Init { path } => cmd_init(&path, format).await,
        Command::Inspect => cmd_inspect(&resolve_path()?, format).await,
        Command::Verify => cmd_verify(&resolve_path()?, format).await,
        Command::Migrate => cmd_migrate(&resolve_path()?, format).await,
        Command::Stats => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            let mem = open_memory(&path, &agent).await?;
            let s = mem.stats().await?;
            format.print(
                || {
                    println!("Agent '{agent}' — valid memories:");
                    println!("  short_term: {}", s.short_term);
                    println!("  episodic:   {}", s.episodic);
                    println!("  procedural: {}", s.procedural);
                    println!("  semantic:   {}", s.semantic);
                    println!("  total:      {}", s.total());
                },
                || {
                    serde_json::json!({
                        "agent": agent,
                        "short_term": s.short_term,
                        "episodic": s.episodic,
                        "procedural": s.procedural,
                        "semantic": s.semantic,
                        "total": s.total(),
                    })
                },
            );
            Ok(())
        }
        Command::Remember { layer, text, file } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            let mem = open_memory(&path, &agent).await?;
            commands_memory::remember(&mem, layer, text, file, format).await
        }
        Command::Recall { query, k, hybrid, layer, graph } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            let mem = open_memory(&path, &agent).await?;
            commands_memory::recall(&mem, &query, k, hybrid, layer, graph, format).await
        }
        Command::List { layer, limit, include_invalid } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            commands_memory::list(&path, &agent, layer, limit, include_invalid, format).await
        }
        Command::Forget { id } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            commands_memory::forget(&path, &agent, &id, format).await
        }
        Command::Invalidate { id } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            commands_memory::invalidate(&path, &agent, &id, format).await
        }
        Command::Purge { yes } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            commands_memory::purge(&path, &agent, yes, format).await
        }
        Command::Export { out } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            commands_memory::export(&path, &agent, out, format).await
        }
        Command::Import { file } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            commands_memory::import(&path, &agent, &file, format).await
        }
        Command::Graph { action } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            match action {
                GraphAction::AddEntity { id, kind, label } => {
                    commands_graph::add_entity(&path, &agent, &id, &kind, &label, format).await
                }
                GraphAction::AddEdge { src, relation, dst, weight } => {
                    commands_graph::add_edge(&path, &agent, &src, &relation, &dst, weight, format).await
                }
                GraphAction::Traverse { start, depth } => {
                    commands_graph::traverse(&path, &agent, &start, depth, format).await
                }
            }
        }
        Command::Maintenance { action } => {
            let path = resolve_path()?;
            match action {
                MaintenanceAction::Gc => commands_maintenance::gc(&path, format).await,
                MaintenanceAction::ForgetAdaptive { capacity, half_life_secs } => {
                    commands_maintenance::forget_adaptive(&path, capacity, half_life_secs, format).await
                }
            }
        }
        Command::Consolidate => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            commands_maintenance::consolidate(&path, &agent, format).await
        }
        Command::Llm { action } => match action {
            LlmAction::Detect => cmd_llm_detect(format).await,
            LlmAction::Suggest => cmd_llm_suggest(format).await,
        },
        Command::Completions { shell } => {
            cmd_completions(shell);
            Ok(())
        }
    }
}

fn cmd_completions(shell: clap_complete::Shell) {
    let mut cmd = <Cli as clap::CommandFactory>::command();
    clap_complete::generate(shell, &mut cmd, "basemyai", &mut std::io::stdout());
}

fn cmd_config(
    action: ConfigAction,
    format: Format,
    cli_db: Option<PathBuf>,
    cli_agent: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ConfigAction::Show => {
            let cfg = CliConfig::load();
            let effective_path = cfg.resolve_path(cli_db).ok();
            let effective_agent = cfg.resolve_agent(cli_agent).ok();
            format.print(
                || {
                    println!("config file: {}", CliConfig::file_path().map_or_else(
                        || "(unresolvable)".to_string(),
                        |p| p.display().to_string(),
                    ));
                    println!(
                        "db_path: {}",
                        effective_path.as_ref().map_or_else(|| "(unset)".to_string(), |p| p.display().to_string())
                    );
                    println!("agent:   {}", effective_agent.as_deref().unwrap_or("(unset)"));
                },
                || {
                    serde_json::json!({
                        "db_path": effective_path.clone().map(|p| p.display().to_string()),
                        "agent": effective_agent.clone(),
                    })
                },
            );
            Ok(())
        }
        ConfigAction::Set { key, value } => {
            let path = CliConfig::set(&key, &value)?;
            format.print(
                || println!("set {key} = {value} ({})", path.display()),
                || serde_json::json!({ "key": key, "value": value, "file": path.display().to_string() }),
            );
            Ok(())
        }
        ConfigAction::Unset { key } => {
            let path = CliConfig::unset(&key)?;
            format.print(
                || println!("unset {key} ({})", path.display()),
                || serde_json::json!({ "key": key, "file": path.display().to_string() }),
            );
            Ok(())
        }
    }
}

fn memory_layer(layer: Layer) -> basemyai::MemoryLayer {
    use basemyai::MemoryLayer;
    match layer {
        Layer::ShortTerm => MemoryLayer::ShortTerm,
        Layer::Episodic => MemoryLayer::Episodic,
        Layer::Procedural => MemoryLayer::Procedural,
        Layer::Semantic => MemoryLayer::Semantic,
    }
}

/// Clé de chiffrement depuis `BASEMYAI_DB_KEY` (obligatoire, ADR-007).
fn require_key() -> Result<basemyai_core::EncryptionKey, Box<dyn std::error::Error>> {
    let raw = std::env::var("BASEMYAI_DB_KEY")
        .map_err(|_| "BASEMYAI_DB_KEY is required (encryption at rest is mandatory)")?;
    Ok(basemyai_core::EncryptionKey::new(raw))
}

/// Charge l'embedder baseline depuis le cache (sans téléchargement). Guide vers
/// `basemyai setup --fetch` si le modèle est absent.
async fn load_embedder() -> Result<Box<dyn basemyai_core::Embedder>, Box<dyn std::error::Error>> {
    let mp = basemyai::provision(false)
        .await
        .map_err(|e| format!("{e}\nhint: run `basemyai setup --fetch` to provision the baseline model"))?;
    let embedder = basemyai_core::CandleEmbedder::load(&mp.model_path, mp.device)?;
    Ok(Box::new(embedder))
}

/// Ouvre un store chiffré sans embedder (commandes purement structurelles).
async fn open_store(path: &std::path::Path) -> Result<basemyai_core::Store, Box<dyn std::error::Error>> {
    if path.extension().and_then(|e| e.to_str()) != Some("bmai") {
        eprintln!("warning: '{}' does not use the .bmai extension", path.display());
    }
    let key = require_key()?;
    Ok(basemyai_core::Store::open(path, Some(key)).await?)
}

/// Ouvre une mémoire complète (store chiffré + embedder + isolation agent).
async fn open_memory(path: &std::path::Path, agent: &str) -> Result<basemyai::Memory, Box<dyn std::error::Error>> {
    let agent_id = basemyai::AgentId::new(agent).ok_or("agent id must not be empty")?;
    let store = open_store(path).await?;
    let embedder = load_embedder().await?;
    Ok(basemyai::Memory::open(store, embedder, agent_id).await?)
}

/// Ouvre l'accès mémoire bas niveau (store chiffré + migrations), sans
/// embedder — pour les opérations qui ne font aucun embedding (forget,
/// invalidate, purge, graphe). Évite de payer le chargement du modèle
/// Candle pour des mutations purement SQL.
async fn open_engine(
    path: &std::path::Path,
    agent: &str,
) -> Result<(std::sync::Arc<dyn basemyai::storage::MemoryStore>, basemyai::AgentId), Box<dyn std::error::Error>> {
    let agent_id = basemyai::AgentId::new(agent).ok_or("agent id must not be empty")?;
    let store = open_store(path).await?;
    store.migrate(&basemyai::schema()).await?;
    let engine: std::sync::Arc<dyn basemyai::storage::MemoryStore> =
        std::sync::Arc::new(basemyai::storage::LibsqlMemoryStore::new(store));
    Ok((engine, agent_id))
}

/// Temps Unix courant (secondes, UTC). `0` si l'horloge est antérieure à
/// l'epoch, sature à `i64::MAX` en cas de dépassement — même politique que
/// `basemyai::now_unix` (interne à ce crate, non accessible depuis le CLI).
fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Lit la table de métadonnées `bmai_meta`.
async fn read_meta(store: &basemyai_core::Store) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let conn = store.connect();
    let mut rows = conn.query("SELECT key, value FROM bmai_meta ORDER BY key", ()).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let k: String = row.get(0)?;
        let v: String = row.get(1)?;
        out.push((k, v));
    }
    Ok(out)
}

async fn count_memories(store: &basemyai_core::Store) -> Result<i64, Box<dyn std::error::Error>> {
    let conn = store.connect();
    let mut rows = conn.query("SELECT COUNT(*) FROM memory", ()).await?;
    let total = match rows.next().await? {
        Some(row) => row.get::<i64>(0)?,
        None => 0,
    };
    Ok(total)
}

async fn cmd_init(path: &std::path::Path, format: Format) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Err(format!("'{}' already exists", path.display()).into());
    }
    let store = open_store(path).await?;
    store.migrate(&basemyai::schema()).await?;
    format.print(
        || {
            println!("created encrypted .bmai container at {}", path.display());
            println!(
                "format_version={}, embedding_dim={}",
                basemyai::BMAI_FORMAT_VERSION,
                basemyai::EMBEDDING_DIM
            );
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "format_version": basemyai::BMAI_FORMAT_VERSION,
                "embedding_dim": basemyai::EMBEDDING_DIM,
            })
        },
    );
    Ok(())
}

async fn cmd_migrate(path: &std::path::Path, format: Format) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_store(path).await?;
    store.migrate(&basemyai::schema()).await?;
    format.print(
        || println!("migrations applied (idempotent) on {}", path.display()),
        || serde_json::json!({ "path": path.display().to_string(), "status": "migrated" }),
    );
    Ok(())
}

async fn cmd_inspect(path: &std::path::Path, format: Format) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_store(path).await?;
    let meta = read_meta(&store).await?;
    let total = count_memories(&store).await?;
    format.print(
        || {
            println!("Container metadata ({}):", path.display());
            for (k, v) in &meta {
                println!("  {k} = {v}");
            }
            println!("Total memory rows (all agents): {total}");
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "metadata": meta.iter().cloned().collect::<std::collections::BTreeMap<String, String>>(),
                "total_memories": total,
            })
        },
    );
    Ok(())
}

async fn cmd_verify(path: &std::path::Path, format: Format) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_store(path).await?;
    let meta = read_meta(&store).await?;
    let get = |key: &str| meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    let format_field = get("format");
    let version = get("format_version");
    let engine = get("storage_engine");

    let expected_version = basemyai::BMAI_FORMAT_VERSION.to_string();

    let format_ok = format_field.as_deref() == Some("basemyai-memory");
    let version_ok = version.as_deref() == Some(expected_version.as_str());
    let engine_ok = engine.is_some();
    let ok = format_ok && version_ok && engine_ok;

    format.print(
        || {
            if format_ok {
                println!("✓ format: basemyai-memory");
            } else {
                println!("✗ format: expected 'basemyai-memory', got {:?}", format_field.as_deref());
            }
            if version_ok {
                println!("✓ format_version: {}", version.as_deref().unwrap_or_default());
            } else {
                println!(
                    "✗ format_version: expected '{expected_version}', got {:?}",
                    version.as_deref()
                );
            }
            match engine.as_deref() {
                Some(e) => println!("✓ storage_engine: {e}"),
                None => println!("✗ storage_engine: missing"),
            }
            if ok {
                println!("{} is a valid .bmai container", path.display());
            }
        },
        || {
            serde_json::json!({
                "path": path.display().to_string(),
                "checks": {
                    "format": format_field,
                    "format_version": version,
                    "storage_engine": engine,
                },
                "valid": ok,
            })
        },
    );

    if ok {
        Ok(())
    } else {
        Err("verification failed".into())
    }
}

fn hardware_json(hw: &basemyai::HardwareProfile) -> serde_json::Value {
    serde_json::json!({
        "ram_mb": hw.total_ram_mb,
        "cpu_cores": hw.cpu_cores,
        "gpu_vram_mb": hw.gpu_vram_mb,
        "device": format!("{:?}", hw.device),
    })
}

async fn cmd_setup(fetch: bool, format: Format) -> Result<(), Box<dyn std::error::Error>> {
    let hw = basemyai::detect_hardware();
    if format == Format::Text {
        print_hardware(&hw);
    }

    if fetch {
        if format == Format::Text {
            println!("\nProvisioning baseline model (fetching if absent)...");
        }
        let mp = basemyai::provision_with_progress(true, |recv, total| match total {
            Some(t) => eprint!("\r  {recv}/{t} bytes"),
            None => eprint!("\r  {recv} bytes"),
        })
        .await?;
        eprintln!();
        format.print(
            || {
                println!(
                    "model ready: {} (dim {}) at {}",
                    mp.model_id,
                    mp.dim,
                    mp.model_path.display()
                );
            },
            || {
                serde_json::json!({
                    "hardware": hardware_json(&hw),
                    "model_id": mp.model_id,
                    "dim": mp.dim,
                    "path": mp.model_path.display().to_string(),
                    "provisioned": true,
                })
            },
        );
    } else {
        match basemyai::provision(false).await {
            Ok(mp) => format.print(
                || {
                    println!(
                        "\nmodel already provisioned: {} at {}",
                        mp.model_id,
                        mp.model_path.display()
                    );
                },
                || {
                    serde_json::json!({
                        "hardware": hardware_json(&hw),
                        "model_id": mp.model_id,
                        "dim": mp.dim,
                        "path": mp.model_path.display().to_string(),
                        "provisioned": true,
                    })
                },
            ),
            Err(_) => format.print(
                || {
                    println!(
                        "\nbaseline model not provisioned. Re-run `basemyai setup --fetch` to download it (explicit consent)."
                    );
                },
                || {
                    serde_json::json!({
                        "hardware": hardware_json(&hw),
                        "model_id": null,
                        "dim": null,
                        "path": null,
                        "provisioned": false,
                    })
                },
            ),
        }
    }
    Ok(())
}

async fn cmd_status(format: Format) -> Result<(), Box<dyn std::error::Error>> {
    let hw = basemyai::detect_hardware();
    if format == Format::Text {
        print_hardware(&hw);
    }
    match basemyai::provision(false).await {
        Ok(mp) => {
            let present = mp.model_path.exists();
            format.print(
                || {
                    println!("\nprovisioned model: {} (dim {})", mp.model_id, mp.dim);
                    println!("  path: {}", mp.model_path.display());
                    println!("  files present: {present}");
                },
                || {
                    serde_json::json!({
                        "hardware": hardware_json(&hw),
                        "model_id": mp.model_id,
                        "dim": mp.dim,
                        "path": mp.model_path.display().to_string(),
                        "provisioned": present,
                    })
                },
            );
        }
        Err(e) => format.print(
            || println!("\nmodel not provisioned: {e}"),
            || {
                serde_json::json!({
                    "hardware": hardware_json(&hw),
                    "model_id": null,
                    "dim": null,
                    "path": null,
                    "provisioned": false,
                })
            },
        ),
    }
    Ok(())
}

fn print_hardware(hw: &basemyai::HardwareProfile) {
    println!("Detected hardware:");
    println!("  RAM: {} MB", hw.total_ram_mb);
    println!("  CPU cores: {}", hw.cpu_cores);
    match hw.gpu_vram_mb {
        Some(v) => println!("  GPU VRAM: {v} MB"),
        None => println!("  GPU VRAM: (none detected)"),
    }
    println!("  Device: {:?}", hw.device);
}

async fn cmd_llm_detect(format: Format) -> Result<(), Box<dyn std::error::Error>> {
    let opts = basemyai::detect_llm_options().await;
    let best = basemyai::best_llm_option(&opts);
    format.print(
        || {
            if opts.is_empty() {
                println!("no local LLM servers detected (Ollama / llama.cpp / OpenAI-compatible).");
                return;
            }
            println!("detected {} local LLM option(s):", opts.len());
            for o in &opts {
                let ram = o
                    .ram_mb
                    .map(|r| format!("{r} MB"))
                    .unwrap_or_else(|| "unknown".to_string());
                println!("  - {} via {:?} @ {} (RAM ~{ram})", o.model_id, o.backend, o.server_url);
            }
            if let Some(best) = best {
                println!("best for this machine: {}", best.model_id);
            }
        },
        || {
            serde_json::json!({
                "options": opts.iter().map(|o| serde_json::json!({
                    "model_id": o.model_id,
                    "backend": format!("{:?}", o.backend),
                    "server_url": o.server_url,
                    "ram_mb": o.ram_mb,
                })).collect::<Vec<_>>(),
                "best": best.map(|b| b.model_id.clone()),
            })
        },
    );
    Ok(())
}

async fn cmd_llm_suggest(format: Format) -> Result<(), Box<dyn std::error::Error>> {
    let installed = basemyai::detect_llm_options().await;
    let suggestions = basemyai::propose_models_to_install(&installed);
    format.print(
        || {
            if suggestions.is_empty() {
                println!("no additional models to suggest for this hardware.");
                return;
            }
            println!("suggested models (e.g. `ollama pull <tag>`):");
            for m in &suggestions {
                println!("  - {} (~{} MB) — {}", m.ollama_tag, m.ram_mb, m.description);
            }
        },
        || {
            serde_json::json!({
                "suggestions": suggestions.iter().map(|m| serde_json::json!({
                    "ollama_tag": m.ollama_tag,
                    "ram_mb": m.ram_mb,
                    "description": m.description,
                })).collect::<Vec<_>>(),
            })
        },
    );
    Ok(())
}

#[cfg(not(all(feature = "crypto", feature = "embed")))]
async fn execute(_cli: Cli, _format: Format) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "basemyai CLI must be built with the `crypto` and `embed` features (they are in the default feature set)"
            .into(),
    )
}
