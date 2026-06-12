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
