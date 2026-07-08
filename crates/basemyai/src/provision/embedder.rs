// SPDX-License-Identifier: BUSL-1.1
//! Setup **hardware-aware** (ADR-010), façon AnythingLLM. Détecte le matériel,
//! choisit modèle + device, fait un fetch **explicite** (jamais silencieux),
//! et persiste le choix dans `~/.local/share/basemyai/provision.json` (Linux /
//! `%APPDATA%` Windows / `~/Library/Application Support` macOS).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use basemyai_core::{CoreError, Device};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sysinfo::System;

/// Modèle baseline garanti partout en V1 (ADR-003).
pub const BASELINE_MODEL_ID: &str = "all-MiniLM-L6-v2";
/// Dimension du baseline.
pub const BASELINE_DIM: usize = 384;

/// URL de base HuggingFace pour le baseline.
const HF_BASE_URL: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/";

/// Fichiers attendus dans le dossier d'un modèle BERT provisionné.
const REQUIRED_MODEL_FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];

/// SHA-256 officiels, révision `main` du 12 juin 2026 (ADR-010).
const EXPECTED_SHA256: &[(&str, &str)] = &[
    (
        "config.json",
        "953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41",
    ),
    (
        "tokenizer.json",
        "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037",
    ),
    (
        "model.safetensors",
        "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db",
    ),
];

// ── Types publics ─────────────────────────────────────────────────────────────

/// Specs machine détectées.
#[derive(Debug, Clone)]
pub struct HardwareProfile {
    pub total_ram_mb: u64,
    /// `None` si aucune détection GPU n'a abouti.
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

// ── Config persistée ──────────────────────────────────────────────────────────

/// Version sérialisable de [`Device`] (le core ne dérive pas Serialize).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedDevice {
    Cpu,
    Cuda(usize),
    Metal,
}

impl From<Device> for PersistedDevice {
    fn from(d: Device) -> Self {
        match d {
            Device::Cpu => Self::Cpu,
            Device::Cuda(i) => Self::Cuda(i),
            Device::Metal => Self::Metal,
            _ => Self::Cpu,
        }
    }
}

impl From<PersistedDevice> for Device {
    fn from(d: PersistedDevice) -> Self {
        match d {
            PersistedDevice::Cpu => Self::Cpu,
            PersistedDevice::Cuda(i) => Self::Cuda(i),
            PersistedDevice::Metal => Self::Metal,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedProvision {
    model_id: String,
    dim: usize,
    model_path: PathBuf,
    device: PersistedDevice,
}

impl From<&ModelProvision> for PersistedProvision {
    fn from(p: &ModelProvision) -> Self {
        Self {
            model_id: p.model_id.clone(),
            dim: p.dim,
            model_path: p.model_path.clone(),
            device: p.device.into(),
        }
    }
}

impl From<PersistedProvision> for ModelProvision {
    fn from(p: PersistedProvision) -> Self {
        Self {
            model_id: p.model_id,
            dim: p.dim,
            model_path: p.model_path,
            device: p.device.into(),
        }
    }
}

// ── API publique ──────────────────────────────────────────────────────────────

/// Détecte le matériel (RAM, VRAM GPU best-effort, cœurs) et résout le device.
#[must_use]
pub fn detect_hardware() -> HardwareProfile {
    let mut sys = System::new();
    sys.refresh_memory();

    let total_ram_mb = sys.total_memory() / (1024 * 1024);
    let cpu_cores = std::thread::available_parallelism().map(usize::from).unwrap_or(1);
    let gpu_vram_mb = detect_vram_mb();
    let device = resolve_device();

    HardwareProfile {
        total_ram_mb,
        gpu_vram_mb,
        cpu_cores,
        device,
    }
}

/// Provisionne le modèle de façon hardware-aware.
///
/// - Lit d'abord la config persistée ; si le modèle est encore présent, retourne
///   immédiatement sans re-détecter le matériel.
/// - Si le modèle est en cache mais sans config : re-détecte et persiste.
/// - Si absent et `consent_to_fetch = false` : erreur propre, aucun download.
/// - Si absent et `consent_to_fetch = true` : télécharge depuis HuggingFace avec
///   vérification SHA-256 des fichiers officiels HuggingFace, puis persiste.
///
/// # Errors
/// [`CoreError::ModelNotProvisioned`] si le modèle est absent, le consentement
/// refusé, ou si le download/vérification échoue.
pub async fn provision(consent_to_fetch: bool) -> crate::Result<ModelProvision> {
    provision_inner(consent_to_fetch, |_, _| {}).await
}

/// Comme [`provision`] mais signale la progression du téléchargement via
/// `on_progress(bytes_reçus, total_octets_optionnel)`.
pub async fn provision_with_progress(
    consent_to_fetch: bool,
    on_progress: impl Fn(u64, Option<u64>),
) -> crate::Result<ModelProvision> {
    provision_inner(consent_to_fetch, on_progress).await
}

// ── Implémentation interne ────────────────────────────────────────────────────

async fn provision_inner(
    consent_to_fetch: bool,
    on_progress: impl Fn(u64, Option<u64>),
) -> crate::Result<ModelProvision> {
    // Fast path : config persistée + modèle toujours présent.
    if let Some(cached) = load_persisted_provision() {
        return Ok(cached);
    }

    let hw = detect_hardware();
    let model_path = baseline_cache_dir();

    if model_present(&model_path) {
        let result = ModelProvision {
            model_id: BASELINE_MODEL_ID.to_string(),
            dim: BASELINE_DIM,
            model_path,
            device: hw.device,
        };
        save_provision(&result);
        return Ok(result);
    }

    if !consent_to_fetch {
        return Err(CoreError::ModelNotProvisioned(format!(
            "modèle '{BASELINE_MODEL_ID}' absent du cache ({}). Lancez le setup \
             hardware-aware avec consentement explicite pour le récupérer.",
            model_path.display()
        ))
        .into());
    }

    fetch_model_files(&model_path, on_progress).await?;

    let result = ModelProvision {
        model_id: BASELINE_MODEL_ID.to_string(),
        dim: BASELINE_DIM,
        model_path,
        device: hw.device,
    };
    save_provision(&result);
    Ok(result)
}

/// Télécharge les fichiers du modèle baseline et appelle `on_progress` à chaque
/// chunk reçu (par fichier — pas de compteur global).
async fn fetch_model_files(target_dir: &Path, on_progress: impl Fn(u64, Option<u64>)) -> crate::Result<()> {
    std::fs::create_dir_all(target_dir).map_err(|e| {
        CoreError::ModelNotProvisioned(format!(
            "impossible de créer le dossier modèle {} : {e}",
            target_dir.display()
        ))
    })?;

    let client = reqwest::Client::new();

    for filename in REQUIRED_MODEL_FILES {
        let url = format!("{HF_BASE_URL}{filename}");
        let dest = target_dir.join(filename);
        let expected = expected_sha256_for(filename);
        download_and_verify(&client, &url, &dest, expected, &on_progress).await?;
    }

    Ok(())
}

/// Télécharge `url` vers `dest` avec vérification SHA-256 et suivi de progression.
///
/// Protocole atomique :
/// 1. Télécharge en streaming par chunks vers `dest.with_extension("tmp")`.
/// 2. Calcule le SHA-256 à la volée.
/// 3. Vérifie contre `expected_sha256` si fourni (erreur dure) ; sinon, vérifie/
///    crée un fichier companion `dest.with_extension("sha256")`.
/// 4. Renomme `.tmp` → `dest` (write atomique).
async fn download_and_verify(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    on_progress: &impl Fn(u64, Option<u64>),
) -> crate::Result<()> {
    let tmp = dest.with_extension("tmp");

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| CoreError::ModelNotProvisioned(format!("téléchargement échoué ({url}) : {e}")))?;

    if !response.status().is_success() {
        return Err(CoreError::ModelNotProvisioned(format!("HTTP {} pour {url}", response.status())).into());
    }

    let total = response.content_length();
    let mut received = 0u64;
    let mut hasher = Sha256::new();
    let mut data: Vec<u8> = Vec::with_capacity(total.unwrap_or(0) as usize);

    // Chunk-by-chunk : calcule SHA-256 + signale progression.
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| CoreError::ModelNotProvisioned(format!("erreur stream ({url}) : {e}")))?
    {
        hasher.update(&chunk);
        received += chunk.len() as u64;
        data.extend_from_slice(&chunk);
        on_progress(received, total);
    }

    // sha2 0.11 : `finalize()` renvoie un `Array` qui n'implémente plus `LowerHex`.
    // On encode l'empreinte en hexadécimal nous-mêmes (sans dépendance supplémentaire).
    let mut computed = String::with_capacity(Sha256::output_size() * 2);
    for byte in hasher.finalize() {
        let _ = write!(computed, "{byte:02x}");
    }

    // ── Vérification SHA-256 ────────────────────────────────────────────────
    let sha_path = dest.with_extension("sha256");
    match expected_sha256 {
        Some(expected) => {
            if computed != expected {
                return Err(CoreError::ModelNotProvisioned(format!(
                    "SHA-256 mismatch pour {} : attendu {expected}, calculé {computed}",
                    dest.display()
                ))
                .into());
            }
        }
        None => {
            if sha_path.exists() {
                let stored = std::fs::read_to_string(&sha_path)
                    .map_err(|e| CoreError::ModelNotProvisioned(format!("lecture sha256 companion : {e}")))?;
                if stored.trim() != computed {
                    return Err(CoreError::ModelNotProvisioned(format!(
                        "SHA-256 mismatch (companion) pour {} : stocké {}, calculé {computed}",
                        dest.display(),
                        stored.trim()
                    ))
                    .into());
                }
            } else {
                std::fs::write(&sha_path, &computed)
                    .map_err(|e| CoreError::ModelNotProvisioned(format!("écriture sha256 companion : {e}")))?;
            }
        }
    }

    // Écriture atomique : .tmp → dest.
    std::fs::write(&tmp, &data)
        .map_err(|e| CoreError::ModelNotProvisioned(format!("écriture tmp {} : {e}", tmp.display())))?;
    std::fs::rename(&tmp, dest).map_err(|e| {
        CoreError::ModelNotProvisioned(format!("renommage {} → {} : {e}", tmp.display(), dest.display()))
    })?;

    Ok(())
}

// ── Détection VRAM ────────────────────────────────────────────────────────────

/// Détection best-effort de la VRAM GPU : NVIDIA via `nvidia-smi`, puis
/// plateforme-spécifique (macOS via `system_profiler`).
fn detect_vram_mb() -> Option<u64> {
    detect_vram_nvidia_smi().or_else(detect_vram_platform)
}

/// Interroge `nvidia-smi` pour obtenir la VRAM totale du GPU 0, en Mo.
fn detect_vram_nvidia_smi() -> Option<u64> {
    let output = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // Sortie : une ligne par GPU, valeur en MiB. On prend le premier.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .parse::<u64>()
        .ok()
}

#[cfg(target_os = "macos")]
fn detect_vram_platform() -> Option<u64> {
    detect_vram_macos()
}

#[cfg(not(target_os = "macos"))]
fn detect_vram_platform() -> Option<u64> {
    None
}

/// Interroge `system_profiler SPDisplaysDataType -json` pour la VRAM (macOS).
/// Fonctionne pour Apple Silicon (mémoire unifiée) et Intel/AMD discrets.
#[cfg(target_os = "macos")]
fn detect_vram_macos() -> Option<u64> {
    let output = std::process::Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output()
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let vram_str = json["SPDisplaysDataType"][0]["spdisplays_vram"].as_str()?;
    parse_vram_mb(vram_str)
}

/// Parse une chaîne VRAM style `"8 GB"` ou `"512 MB"` en mégaoctets.
#[cfg(target_os = "macos")]
fn parse_vram_mb(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(n) = s.strip_suffix(" gb") {
        n.trim().parse::<u64>().ok().map(|gb| gb * 1024)
    } else if let Some(n) = s.strip_suffix(" mb") {
        n.trim().parse::<u64>().ok()
    } else {
        None
    }
}

// ── Device & cache ────────────────────────────────────────────────────────────

/// Choisit le device : **CUDA > Metal > CPU**.
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

fn cuda_available() -> bool {
    std::env::var_os("CUDA_PATH").is_some() || std::env::var_os("CUDA_HOME").is_some()
}

fn metal_available() -> bool {
    cfg!(target_os = "macos")
}

/// Dossier de cache du modèle baseline : `<cache>/basemyai/models/<model_id>`.
#[must_use]
fn baseline_cache_dir() -> PathBuf {
    let base = dirs::cache_dir()
        .or_else(dirs::home_dir)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("basemyai").join("models").join(BASELINE_MODEL_ID)
}

/// `true` si tous les fichiers requis existent dans `dir`.
#[must_use]
fn model_present(dir: &Path) -> bool {
    REQUIRED_MODEL_FILES.iter().all(|f| dir.join(f).is_file())
}

/// Retourne le SHA-256 attendu pour `filename`, ou `None` si non ancré.
fn expected_sha256_for(filename: &str) -> Option<&'static str> {
    EXPECTED_SHA256.iter().find(|(f, _)| *f == filename).map(|(_, h)| *h)
}

// ── Persistance config ────────────────────────────────────────────────────────

/// Chemin vers la config persistée : `<data_dir>/basemyai/provision.json`.
fn provision_config_path() -> PathBuf {
    dirs::data_dir()
        .or_else(dirs::home_dir)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("basemyai")
        .join("provision.json")
}

/// Charge la config persistée si elle existe et si le modèle est encore présent.
fn load_persisted_provision() -> Option<ModelProvision> {
    let text = std::fs::read_to_string(provision_config_path()).ok()?;
    let p: PersistedProvision = serde_json::from_str(&text).ok()?;
    let result: ModelProvision = p.into();
    if model_present(&result.model_path) {
        Some(result)
    } else {
        None
    }
}

/// Persiste la config (best-effort — échec silencieux pour ne pas bloquer l'usage).
fn save_provision(provision: &ModelProvision) {
    let path = provision_config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&PersistedProvision::from(provision)) {
        let _ = std::fs::write(path, json);
    }
}
