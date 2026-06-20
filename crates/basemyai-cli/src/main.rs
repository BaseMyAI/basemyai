//! Binaire `basemyai` : CLI développeur de la base mémoire BaseMyAI.
//!
//! Donne accès en ligne de commande au cœur memory database : provisionnement
//! du modèle d'embedding (hardware-aware, sans download silencieux — ADR-010),
//! création/inspection/vérification d'un conteneur `.bmai` chiffré (ADR-019),
//! et les opérations mémoire de base (`remember`, `recall`, `stats`).
//!
//! ## Chiffrement obligatoire
//!
//! Toute commande qui ouvre un fichier `.bmai` exige la clé via la variable
//! d'environnement `BASEMYAI_DB_KEY` (chiffrement au repos, ADR-007). Aucune
//! commande n'ouvre un fichier en clair.
//!
//! ## Features
//!
//! Le chemin réel exige `crypto` (chiffrement libSQL) et `embed` (embedder
//! Candle) — tous deux dans le set par défaut. Sans eux, le binaire se contente
//! d'afficher l'aide et une erreur explicite.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// CLI développeur de BaseMyAI — la base mémoire privée pour agents IA.
#[derive(Parser)]
#[command(name = "basemyai", version, about, long_about = None)]
struct Cli {
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
    /// Crée un nouveau conteneur `.bmai` chiffré (migrations + métadonnées).
    Init {
        /// Chemin du fichier `.bmai` à créer.
        path: PathBuf,
    },
    /// Inspecte un `.bmai` : métadonnées du conteneur + nombre de souvenirs.
    Inspect {
        /// Chemin du fichier `.bmai`.
        path: PathBuf,
    },
    /// Statistiques mémoire par agent (souvenirs valides).
    Stats {
        /// Chemin du fichier `.bmai`.
        path: PathBuf,
        /// Identifiant de l'agent.
        #[arg(long)]
        agent: String,
    },
    /// Mémorise un texte pour un agent.
    Remember {
        /// Chemin du fichier `.bmai`.
        path: PathBuf,
        /// Identifiant de l'agent.
        #[arg(long)]
        agent: String,
        /// Couche mémoire cible.
        #[arg(long, value_enum, default_value_t = Layer::Semantic)]
        layer: Layer,
        /// Texte à mémoriser.
        text: String,
    },
    /// Rappelle des souvenirs d'un agent par requête sémantique.
    Recall {
        /// Chemin du fichier `.bmai`.
        path: PathBuf,
        /// Identifiant de l'agent.
        #[arg(long)]
        agent: String,
        /// Texte de la requête.
        query: String,
        /// Nombre de résultats.
        #[arg(short, long, default_value_t = 5)]
        k: usize,
        /// Rappel hybride (vecteur + BM25 fusionnés par RRF).
        #[arg(long)]
        hybrid: bool,
    },
    /// Vérifie un `.bmai` : conteneur valide, version de format attendue.
    Verify {
        /// Chemin du fichier `.bmai`.
        path: PathBuf,
    },
    /// Applique les migrations de schéma en attente (idempotent).
    Migrate {
        /// Chemin du fichier `.bmai`.
        path: PathBuf,
    },
    /// Helpers de provisionnement LLM local (consolidation).
    Llm {
        #[command(subcommand)]
        action: LlmAction,
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
    match runtime.block_on(execute(cli)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn execute(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    use basemyai::Record;

    match cli.command {
        Command::Setup { fetch } => cmd_setup(fetch).await,
        Command::Status => cmd_status().await,
        Command::Init { path } => cmd_init(&path).await,
        Command::Inspect { path } => cmd_inspect(&path).await,
        Command::Verify { path } => cmd_verify(&path).await,
        Command::Migrate { path } => cmd_migrate(&path).await,
        Command::Stats { path, agent } => {
            let mem = open_memory(&path, &agent).await?;
            let s = mem.stats().await?;
            println!("Agent '{agent}' — valid memories:");
            println!("  short_term: {}", s.short_term);
            println!("  episodic:   {}", s.episodic);
            println!("  procedural: {}", s.procedural);
            println!("  semantic:   {}", s.semantic);
            println!("  total:      {}", s.total());
            Ok(())
        }
        Command::Remember {
            path,
            agent,
            layer,
            text,
        } => {
            let mem = open_memory(&path, &agent).await?;
            let id = mem.remember(&text, memory_layer(layer)).await?;
            println!("remembered {id} in layer {}", layer_name(layer));
            Ok(())
        }
        Command::Recall {
            path,
            agent,
            query,
            k,
            hybrid,
        } => {
            let mem = open_memory(&path, &agent).await?;
            let records: Vec<Record> = if hybrid {
                mem.recall_hybrid(&query, k).await?
            } else {
                mem.recall(&query, k).await?
            };
            if records.is_empty() {
                println!("(no memories matched)");
            } else {
                println!("{} result(s) for \"{query}\":", records.len());
                for (i, r) in records.iter().enumerate() {
                    println!(
                        "  {}. [{:.3}] ({}) {}",
                        i + 1,
                        r.similarity(),
                        layer_table(r.layer),
                        r.text
                    );
                }
            }
            Ok(())
        }
        Command::Llm { action } => match action {
            LlmAction::Detect => cmd_llm_detect().await,
            LlmAction::Suggest => cmd_llm_suggest().await,
        },
    }
}

#[cfg(all(feature = "crypto", feature = "embed"))]
fn memory_layer(layer: Layer) -> basemyai::MemoryLayer {
    use basemyai::MemoryLayer;
    match layer {
        Layer::ShortTerm => MemoryLayer::ShortTerm,
        Layer::Episodic => MemoryLayer::Episodic,
        Layer::Procedural => MemoryLayer::Procedural,
        Layer::Semantic => MemoryLayer::Semantic,
    }
}

#[cfg(all(feature = "crypto", feature = "embed"))]
fn layer_name(layer: Layer) -> &'static str {
    memory_layer(layer).table()
}

#[cfg(all(feature = "crypto", feature = "embed"))]
fn layer_table(layer: basemyai::MemoryLayer) -> &'static str {
    layer.table()
}

/// Clé de chiffrement depuis `BASEMYAI_DB_KEY` (obligatoire, ADR-007).
#[cfg(all(feature = "crypto", feature = "embed"))]
fn require_key() -> Result<basemyai_core::EncryptionKey, Box<dyn std::error::Error>> {
    let raw = std::env::var("BASEMYAI_DB_KEY")
        .map_err(|_| "BASEMYAI_DB_KEY is required (encryption at rest is mandatory)")?;
    Ok(basemyai_core::EncryptionKey::new(raw))
}

/// Charge l'embedder baseline depuis le cache (sans téléchargement). Guide vers
/// `basemyai setup --fetch` si le modèle est absent.
#[cfg(all(feature = "crypto", feature = "embed"))]
async fn load_embedder() -> Result<Box<dyn basemyai_core::Embedder>, Box<dyn std::error::Error>> {
    let mp = basemyai::provision(false)
        .await
        .map_err(|e| format!("{e}\nhint: run `basemyai setup --fetch` to provision the baseline model"))?;
    let embedder = basemyai_core::CandleEmbedder::load(&mp.model_path, mp.device)?;
    Ok(Box::new(embedder))
}

/// Ouvre un store chiffré sans embedder (commandes purement structurelles).
#[cfg(all(feature = "crypto", feature = "embed"))]
async fn open_store(path: &std::path::Path) -> Result<basemyai_core::Store, Box<dyn std::error::Error>> {
    if path.extension().and_then(|e| e.to_str()) != Some("bmai") {
        eprintln!("warning: '{}' does not use the .bmai extension", path.display());
    }
    let key = require_key()?;
    Ok(basemyai_core::Store::open(path, Some(key)).await?)
}

/// Ouvre une mémoire complète (store chiffré + embedder + isolation agent).
#[cfg(all(feature = "crypto", feature = "embed"))]
async fn open_memory(path: &std::path::Path, agent: &str) -> Result<basemyai::Memory, Box<dyn std::error::Error>> {
    let agent_id = basemyai::AgentId::new(agent).ok_or("agent id must not be empty")?;
    let store = open_store(path).await?;
    let embedder = load_embedder().await?;
    Ok(basemyai::Memory::open(store, embedder, agent_id).await?)
}

/// Lit la table de métadonnées `bmai_meta`.
#[cfg(all(feature = "crypto", feature = "embed"))]
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

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn count_memories(store: &basemyai_core::Store) -> Result<i64, Box<dyn std::error::Error>> {
    let conn = store.connect();
    let mut rows = conn.query("SELECT COUNT(*) FROM memory", ()).await?;
    let total = match rows.next().await? {
        Some(row) => row.get::<i64>(0)?,
        None => 0,
    };
    Ok(total)
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn cmd_init(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Err(format!("'{}' already exists", path.display()).into());
    }
    let store = open_store(path).await?;
    store.migrate(&basemyai::schema()).await?;
    println!("created encrypted .bmai container at {}", path.display());
    println!(
        "format_version={}, embedding_dim={}",
        basemyai::BMAI_FORMAT_VERSION,
        basemyai::EMBEDDING_DIM
    );
    Ok(())
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn cmd_migrate(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_store(path).await?;
    store.migrate(&basemyai::schema()).await?;
    println!("migrations applied (idempotent) on {}", path.display());
    Ok(())
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn cmd_inspect(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_store(path).await?;
    let meta = read_meta(&store).await?;
    println!("Container metadata ({}):", path.display());
    for (k, v) in &meta {
        println!("  {k} = {v}");
    }
    let total = count_memories(&store).await?;
    println!("Total memory rows (all agents): {total}");
    Ok(())
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn cmd_verify(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_store(path).await?;
    let meta = read_meta(&store).await?;
    let get = |key: &str| meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    let format = get("format");
    let version = get("format_version");
    let engine = get("storage_engine");

    let expected_version = basemyai::BMAI_FORMAT_VERSION.to_string();
    let mut ok = true;

    match format.as_deref() {
        Some("basemyai-memory") => println!("✓ format: basemyai-memory"),
        other => {
            ok = false;
            println!("✗ format: expected 'basemyai-memory', got {other:?}");
        }
    }
    match version.as_deref() {
        Some(v) if v == expected_version => println!("✓ format_version: {v}"),
        other => {
            ok = false;
            println!("✗ format_version: expected '{expected_version}', got {other:?}");
        }
    }
    match engine.as_deref() {
        Some(e) => println!("✓ storage_engine: {e}"),
        None => {
            ok = false;
            println!("✗ storage_engine: missing");
        }
    }

    if ok {
        println!("{} is a valid .bmai container", path.display());
        Ok(())
    } else {
        Err("verification failed".into())
    }
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn cmd_setup(fetch: bool) -> Result<(), Box<dyn std::error::Error>> {
    let hw = basemyai::detect_hardware();
    print_hardware(&hw);

    if fetch {
        println!("\nProvisioning baseline model (fetching if absent)...");
        let mp = basemyai::provision_with_progress(true, |recv, total| match total {
            Some(t) => eprint!("\r  {recv}/{t} bytes"),
            None => eprint!("\r  {recv} bytes"),
        })
        .await?;
        eprintln!();
        println!(
            "model ready: {} (dim {}) at {}",
            mp.model_id,
            mp.dim,
            mp.model_path.display()
        );
    } else {
        match basemyai::provision(false).await {
            Ok(mp) => println!(
                "\nmodel already provisioned: {} at {}",
                mp.model_id,
                mp.model_path.display()
            ),
            Err(_) => println!(
                "\nbaseline model not provisioned. Re-run `basemyai setup --fetch` to download it (explicit consent)."
            ),
        }
    }
    Ok(())
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn cmd_status() -> Result<(), Box<dyn std::error::Error>> {
    let hw = basemyai::detect_hardware();
    print_hardware(&hw);
    match basemyai::provision(false).await {
        Ok(mp) => {
            println!("\nprovisioned model: {} (dim {})", mp.model_id, mp.dim);
            println!("  path: {}", mp.model_path.display());
            println!("  files present: {}", mp.model_path.exists());
        }
        Err(e) => println!("\nmodel not provisioned: {e}"),
    }
    Ok(())
}

#[cfg(all(feature = "crypto", feature = "embed"))]
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

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn cmd_llm_detect() -> Result<(), Box<dyn std::error::Error>> {
    let opts = basemyai::detect_llm_options().await;
    if opts.is_empty() {
        println!("no local LLM servers detected (Ollama / llama.cpp / OpenAI-compatible).");
        return Ok(());
    }
    println!("detected {} local LLM option(s):", opts.len());
    for o in &opts {
        let ram = o
            .ram_mb
            .map(|r| format!("{r} MB"))
            .unwrap_or_else(|| "unknown".to_string());
        println!("  - {} via {:?} @ {} (RAM ~{ram})", o.model_id, o.backend, o.server_url);
    }
    if let Some(best) = basemyai::best_llm_option(&opts) {
        println!("best for this machine: {}", best.model_id);
    }
    Ok(())
}

#[cfg(all(feature = "crypto", feature = "embed"))]
async fn cmd_llm_suggest() -> Result<(), Box<dyn std::error::Error>> {
    let installed = basemyai::detect_llm_options().await;
    let suggestions = basemyai::propose_models_to_install(&installed);
    if suggestions.is_empty() {
        println!("no additional models to suggest for this hardware.");
        return Ok(());
    }
    println!("suggested models (e.g. `ollama pull <tag>`):");
    for m in suggestions {
        println!("  - {} (~{} MB) — {}", m.ollama_tag, m.ram_mb, m.description);
    }
    Ok(())
}

#[cfg(not(all(feature = "crypto", feature = "embed")))]
async fn execute(_cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "basemyai CLI must be built with the `crypto` and `embed` features (they are in the default feature set)"
            .into(),
    )
}
