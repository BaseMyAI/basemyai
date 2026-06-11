//! Setup **hardware-aware** (ADR-010), façon AnythingLLM. Détecte le matériel,
//! choisit modèle + device, fait un fetch **explicite** (jamais silencieux),
//! et persiste le choix. C'est ce module — côté produit — qui résout ce que le
//! core reçoit ensuite tout cuit (chemin + device).

use std::path::PathBuf;

use basemyai_core::{CoreError, Device};
use sysinfo::System;

/// Modèle baseline garanti partout en V1 (compat `.idx` ForgeMyAI).
pub const BASELINE_MODEL_ID: &str = "all-MiniLM-L6-v2";
/// Dimension du baseline.
pub const BASELINE_DIM: usize = 384;

/// Fichiers attendus dans le dossier d'un modèle BERT provisionné.
const REQUIRED_MODEL_FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];

/// Specs machine détectées.
#[derive(Debug, Clone)]
pub struct HardwareProfile {
    pub total_ram_mb: u64,
    pub gpu_vram_mb: Option<u64>,
    pub cpu_cores: usize,
    pub device: Device,
}

/// Résultat du provisioning : ce que l'`Embedder` du core recevra, déjà résolu.
#[derive(Debug, Clone)]
pub struct ModelProvision {
    pub model_id: String,
    pub dim: usize,
    pub model_path: PathBuf,
    pub device: Device,
}

/// Détecte le matériel (RAM, GPU/VRAM, cœurs) et résout le device.
///
/// Plateforme-spécifique (NVML/Metal/sysinfo) — câblé à l'implémentation.
#[must_use]
pub fn detect_hardware() -> HardwareProfile {
    let mut sys = System::new();
    sys.refresh_memory();

    // `total_memory()` est en octets → Mo.
    let total_ram_mb = sys.total_memory() / (1024 * 1024);

    // Nombre de cœurs logiques. Repli sur 1 si l'OS ne renseigne rien.
    let cpu_cores = std::thread::available_parallelism().map(usize::from).unwrap_or(1);

    // VRAM/GPU : best-effort. `sysinfo` n'expose pas la VRAM de façon portable ;
    // on laisse `None` (acceptable) tant qu'une détection GPU dédiée n'est pas
    // câblée. Le device est résolu CUDA > Metal > CPU selon la disponibilité.
    let gpu_vram_mb = None;
    let device = resolve_device();

    HardwareProfile { total_ram_mb, gpu_vram_mb, cpu_cores, device }
}

/// Choisit le device : **CUDA > Metal > CPU**. Détection best-effort ; le CPU est
/// le repli universel garanti. La disponibilité réelle d'un backend GPU dépend
/// des features Candle compilées côté `basemyai-core` (l'`Embedder` repliera sur
/// CPU si l'init échoue malgré tout).
#[must_use]
fn resolve_device() -> Device {
    if cuda_available() {
        Device::Cuda(0)
    } else if metal_available() {
        Device::Metal
    } else {
        Device::Cpu
    }
}

/// Détection CUDA best-effort : présence d'un runtime/driver NVIDIA via env.
fn cuda_available() -> bool {
    std::env::var_os("CUDA_PATH").is_some() || std::env::var_os("CUDA_HOME").is_some()
}

/// Détection Metal best-effort : disponible sur macOS (Apple Silicon/Intel récents).
fn metal_available() -> bool {
    cfg!(target_os = "macos")
}

/// Provisionne le modèle de façon hardware-aware.
///
/// **Pas de download silencieux** : si le modèle n'est pas en cache, le fetch
/// est explicite (consentement + checksum) — déclenché ici, jamais par l'`Embedder`.
///
/// # Errors
/// Échoue proprement si le setup n'a pas été fait et que l'utilisateur n'a pas
/// consenti au fetch (le 1ᵉʳ usage invite à lancer `basemyai setup`).
pub fn provision(consent_to_fetch: bool) -> crate::Result<ModelProvision> {
    let hw = detect_hardware();
    // V1 : on reste sur le baseline quel que soit le matériel (compat .idx).
    // V2 : un modèle plus fort pourra être proposé si `hw` le permet.
    let model_path = baseline_cache_dir();

    if model_present(&model_path) {
        return Ok(ModelProvision {
            model_id: BASELINE_MODEL_ID.to_string(),
            dim: BASELINE_DIM,
            model_path,
            device: hw.device,
        });
    }

    // Modèle absent du cache.
    if !consent_to_fetch {
        // **Pas de download silencieux** (ADR-010) : on échoue proprement et on
        // invite l'utilisateur à lancer le setup avec consentement explicite.
        return Err(CoreError::ModelNotProvisioned(format!(
            "modèle '{BASELINE_MODEL_ID}' absent du cache ({}). Lancez le setup \
             hardware-aware avec consentement explicite pour le récupérer.",
            model_path.display()
        ))
        .into());
    }

    // Consentement donné : un fetch explicite (avec checksum) viendrait ICI.
    // Volontairement NON implémenté pour la V1 scaffoldée — aucun téléchargement
    // n'est déclenché (les tests ne doivent jamais accéder au réseau). On signale
    // donc que le provisioning consenti reste à câbler plutôt que de simuler un
    // download.
    Err(CoreError::ModelNotProvisioned(format!(
        "fetch explicite du modèle '{BASELINE_MODEL_ID}' non encore implémenté ; \
         placez manuellement les fichiers dans {}.",
        model_path.display()
    ))
    .into())
}

/// Dossier de cache du modèle baseline : `<cache>/basemyai/models/<model_id>`.
///
/// Utilise le dossier cache utilisateur (`dirs`) ; repli sur `~/.basemyai` via
/// `HOME`/`USERPROFILE`, puis sur le dossier courant en dernier recours.
#[must_use]
fn baseline_cache_dir() -> PathBuf {
    let base = dirs::cache_dir()
        .or_else(dirs::home_dir)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("basemyai").join("models").join(BASELINE_MODEL_ID)
}

/// Le modèle est « présent » si tous ses fichiers requis existent dans le dossier.
#[must_use]
fn model_present(dir: &std::path::Path) -> bool {
    REQUIRED_MODEL_FILES.iter().all(|f| dir.join(f).is_file())
}
