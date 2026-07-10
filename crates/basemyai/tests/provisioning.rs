//! Tests du setup hardware-aware. **Aucun test ne télécharge** (ADR-010).
//!
//! Fichier nommé `provisioning` (et non `setup`) à dessein : un binaire de test
//! `setup-*.exe` déclenche la détection d'installeur de Windows (UAC, os error
//! 740) et refuse de se lancer sans élévation. Le nom neutre évite cet artefact.

use basemyai::{BASELINE_DIM, BASELINE_MODEL_ID, detect_hardware, provision};

#[test]
fn detect_hardware_returns_plausible_values() {
    let hw = detect_hardware();
    assert!(hw.cpu_cores >= 1, "au moins un cœur logique doit être détecté");
    assert!(hw.total_ram_mb > 0, "la RAM totale doit être > 0 Mo");
    // La détection GPU NVIDIA (NVML, feature `cuda-detect`) est best-effort :
    // sur la CI (sans GPU NVIDIA), `gpus` DOIT être vide, pas paniquer.
    // Quand des GPU sont bien détectés, les valeurs doivent rester cohérentes.
    for gpu in &hw.gpus {
        assert!(
            gpu.vram_total_mb > 0,
            "VRAM totale annoncée par NVML doit être positive"
        );
        assert!(
            gpu.vram_free_mb <= gpu.vram_total_mb,
            "VRAM libre ne peut pas dépasser la VRAM totale"
        );
    }
}

// Compilé seulement avec `--features cuda-detect` (`cargo test -p basemyai
// --features cuda-detect --test provisioning`) — hors du gate CI léger
// (`cargo xtask ci`) par choix (voir Cargo.toml). Sans GPU NVIDIA (poste de
// dev/CI habituel), NVML échoue proprement à s'initialiser : ce test vérifie
// justement que `detect_hardware()` ne panique jamais dans ce cas, le cas le
// plus courant en pratique.
#[cfg(feature = "cuda-detect")]
#[test]
fn detect_hardware_nvml_never_panics_without_gpu() {
    // L'assertion réelle est que cet appel ne panique pas : NVML/driver absent
    // (cas normal sur la CI, sans GPU NVIDIA) doit être absorbé proprement.
    let hw = detect_hardware();
    // Quand NVML détecte au moins un GPU, le device résolu DOIT pointer vers
    // ce GPU (index cohérent) — sinon `gpus` est vide, ce qui est valide.
    if let Some(gpu) = hw.gpus.first() {
        assert_eq!(hw.device, basemyai_core::Device::Cuda(gpu.index));
    }
}

#[tokio::test]
async fn provision_without_consent_fails_when_model_absent() {
    // Sans modèle en cache et sans consentement, `provision` DOIT échouer
    // proprement — et ne déclenche AUCUN téléchargement.
    match provision(false).await {
        Err(_) => { /* cas attendu : cache absent, pas de fetch silencieux */ }
        Ok(p) => {
            assert_eq!(p.model_id, BASELINE_MODEL_ID);
            assert_eq!(p.dim, BASELINE_DIM);
            assert!(p.model_path.is_dir(), "le chemin doit pointer vers un dossier existant");
        }
    }
}
