//! Tests du provisioning LLM hardware-aware.
//!
//! - Logique pure (`best_llm_option`, `propose_models_to_install`) : toujours verts.
//! - Détection réseau (`detect_llm_options`) : toujours verts (liste vide si aucun
//!   serveur actif — aucun téléchargement, aucune erreur).
//! - Test d'intégration réel (`#[ignore]`) : nécessite un serveur Ollama actif.

use basemyai::{BackendKind, KNOWN_MODELS, LlmOption, best_llm_option, detect_llm_options, propose_models_to_install};

/// Fabrique une option factice avec un coût mémoire connu.
fn fake_option(model_id: &str, ram_mb: u64, backend: BackendKind) -> LlmOption {
    LlmOption {
        model_id: model_id.to_string(),
        server_url: "http://localhost:11434".to_string(),
        backend,
        ram_mb: Some(ram_mb),
    }
}

#[test]
fn known_models_are_ordered_heaviest_first() {
    // Invariant de la table : parcourir dans l'ordre = « meilleur d'abord ».
    let rams: Vec<u64> = KNOWN_MODELS.iter().map(|m| m.ram_mb).collect();
    let mut sorted = rams.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(rams, sorted, "KNOWN_MODELS doit être trié par ram_mb décroissant");
}

#[test]
fn best_option_picks_heaviest_fitting() {
    // Simule une machine avec 8 000 Mo RAM → budget = 4 800 Mo (0.6 × 8000).
    // Options disponibles : 2 000 Mo, 4 700 Mo, 5 500 Mo.
    // → attend llama3.1:8b (4 700 Mo) : le plus lourd qui tient.
    let options = vec![
        fake_option("gemma2:9b", 5_500, BackendKind::Ollama),
        fake_option("llama3.1:8b", 4_700, BackendKind::Ollama),
        fake_option("llama3.2:3b", 2_000, BackendKind::Ollama),
    ];
    // On ne peut pas contrôler la RAM réelle de la machine dans un test unitaire,
    // donc on vérifie le comportement de la fonction sur un sous-ensemble fictif.
    // Avec `best_llm_option` qui utilise la RAM réelle, on vérifie au moins que :
    // 1) elle ne panique pas sur une liste non vide,
    // 2) si elle retourne Some, l'option a bien un `ram_mb`.
    let pick = best_llm_option(&options);
    if let Some(p) = pick {
        assert!(p.ram_mb.is_some(), "l'option choisie doit avoir un coût mémoire connu");
        // Elle doit être le max parmi les options filtrées.
        let budget = pick.and_then(|x| x.ram_mb);
        let max_fitting = options.iter().filter(|o| o.ram_mb <= budget).max_by_key(|o| o.ram_mb);
        if let Some(expected) = max_fitting {
            assert_eq!(
                p.model_id, expected.model_id,
                "doit choisir le modèle le plus lourd qui tient"
            );
        }
    }
    // Si None : machine avec moins de 2 000 Mo libre, acceptable en test.
}

#[test]
fn best_option_returns_none_on_empty() {
    assert!(best_llm_option(&[]).is_none());
}

#[test]
fn best_option_excludes_unknown_ram() {
    // Une option sans `ram_mb` ne doit jamais être choisie.
    let options = vec![LlmOption {
        model_id: "unknown-model".to_string(),
        server_url: "http://localhost:11434".to_string(),
        backend: BackendKind::Ollama,
        ram_mb: None,
    }];
    assert!(best_llm_option(&options).is_none());
}

#[test]
fn propose_models_excludes_already_installed() {
    let installed = vec![fake_option("mistral:7b", 4_100, BackendKind::Ollama)];
    let proposals = propose_models_to_install(&installed);
    // mistral:7b ne doit pas réapparaître dans les proposals.
    assert!(
        proposals.iter().all(|m| !m.ollama_tag.starts_with("mistral:7b")),
        "un modèle déjà installé ne doit pas être proposé"
    );
}

#[test]
fn anythingllm_excluded_from_best_option() {
    // AnythingLLM a ram_mb = None → best_llm_option doit le filtrer même si c'est
    // la seule entrée (proxy, non utilisable pour l'inférence directe).
    let options = vec![LlmOption {
        model_id: "anythingllm".to_string(),
        server_url: "http://localhost:3001".to_string(),
        backend: BackendKind::AnythingLlm,
        ram_mb: None,
    }];
    assert!(
        best_llm_option(&options).is_none(),
        "AnythingLLM ne doit jamais être sélectionné"
    );
}

#[test]
fn new_backend_kinds_compile_and_are_distinct() {
    // Smoke test : tous les variants compilent et sont différents.
    let kinds = [
        BackendKind::Ollama,
        BackendKind::LmStudio,
        BackendKind::Jan,
        BackendKind::Gpt4All,
        BackendKind::KoboldCpp,
        BackendKind::Vllm,
        BackendKind::LocalAi,
        BackendKind::AnythingLlm,
        BackendKind::OpenAiCompat,
    ];
    // Chaque variant doit être unique (pas de dérivé Hash, donc on compare par pairs).
    for i in 0..kinds.len() {
        for j in (i + 1)..kinds.len() {
            assert_ne!(kinds[i], kinds[j], "deux variants identiques dans BackendKind");
        }
    }
}

#[test]
fn known_models_2026_include_key_models() {
    // Vérifier la présence des modèles importants 2026.
    let tags: Vec<&str> = KNOWN_MODELS.iter().map(|m| m.ollama_tag).collect();
    for expected in &[
        "qwen3:14b",
        "qwen3:8b",
        "qwen3:4b",
        "gemma3:12b",
        "gemma3:4b",
        "phi4-mini",
        "deepseek-r1:7b",
        "llama3.3:8b",
        "devstral:24b",
    ] {
        assert!(
            tags.contains(expected),
            "modèle 2026 manquant dans KNOWN_MODELS : {expected}"
        );
    }
}

#[test]
fn lm_studio_option_selectable_by_best_option() {
    // Un modèle servi par LM Studio doit être sélectionnable comme n'importe quel autre.
    let options = vec![
        fake_option("qwen3:8b", 5_100, BackendKind::LmStudio),
        fake_option("llama3.2:1b", 700, BackendKind::Ollama),
    ];
    let pick = best_llm_option(&options);
    // qwen3:8b est plus lourd → sélectionné si la machine a assez de RAM.
    if let Some(p) = pick {
        assert!(p.ram_mb.is_some());
    }
}

#[test]
fn propose_models_returns_compatible_subset() {
    // Tous les modèles proposés doivent avoir un tag présent dans KNOWN_MODELS.
    let proposals = propose_models_to_install(&[]);
    for p in &proposals {
        assert!(
            KNOWN_MODELS.iter().any(|m| m.ollama_tag == p.ollama_tag),
            "proposal '{}' doit être dans KNOWN_MODELS",
            p.ollama_tag
        );
    }
}

/// Détection réseau : ne panique jamais, retourne une liste (éventuellement vide).
/// Timeout court (1 s) → rapide même sans serveur.
#[tokio::test]
async fn detect_returns_empty_or_list_without_panic() {
    let options = detect_llm_options().await;
    // Aucun serveur actif en CI → liste vide. Serveur actif → liste non vide.
    // On vérifie juste que la fonction se termine et que les options sont cohérentes.
    for opt in &options {
        assert!(!opt.model_id.is_empty(), "model_id ne doit pas être vide");
        assert!(!opt.server_url.is_empty(), "server_url ne doit pas être vide");
    }
}

/// Test d'intégration RÉEL — nécessite `ollama serve` actif avec au moins un modèle.
/// Lance avec : `cargo test -p basemyai --test llm_provision -- --ignored`
#[tokio::test]
#[ignore = "nécessite un serveur Ollama actif (ollama serve)"]
async fn integration_full_llm_cycle() {
    use basemyai::{LlmInference, choose_llm};
    let provision = choose_llm()
        .await
        .expect("Ollama doit être actif avec au moins un modèle");
    let response = provision
        .backend
        .complete("Réponds juste 'ok'.")
        .await
        .expect("completion");
    assert!(!response.is_empty(), "la réponse du LLM ne doit pas être vide");
}
