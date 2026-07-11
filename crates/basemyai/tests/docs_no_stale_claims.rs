//! Garde-fou documentaire (M0, `PLAN.md` racine) : les documents qui décrivent
//! l'état *actuel* du projet (`CLAUDE.md`, `README.md`) ne doivent plus
//! affirmer une architecture retirée par ADR-033 (ancien moteur SQL comme
//! défaut, chiffrement lié à CMake, licence permissive). Une doc fausse peut
//! faire réintroduire une architecture supprimée par un agent de code ou un
//! contributeur.
//!
//! Volontairement limité à `CLAUDE.md`/`README.md` : `CHANGELOG.md` et les
//! ADR documentent légitimement l'historique et ne doivent pas être scannés
//! par ce garde-fou. Complémentaire à `cargo xtask doc-drift` (`xtask/src/main.rs`),
//! qui scanne `crates/`/`bindings/`/`Cargo.toml` mais pas les docs racine.
//!
//! Les jetons interdits sont assemblés par concaténation plutôt qu'écrits en
//! toutes lettres pour ne pas se faire piéger lui-même par `doc-drift`, qui
//! scanne aussi ce fichier.

use std::fs;
use std::path::Path;

fn forbidden_tokens() -> Vec<String> {
    vec![
        "backend = lib".to_string() + "sql",
        "lib".to_string() + "sql reste le défaut",
        "lib".to_string() + "sql is the default",
        "sql".to_string() + "cipher",
        "feature engine-native".to_string(),
        "feature crypto lib".to_string() + "sql",
        "license mit".to_string(),
        "license-mit".to_string(),
    ]
}

fn scan(path: &Path, tokens: &[String], hits: &mut Vec<String>) {
    let src = fs::read_to_string(path).expect("read doc file");
    let lower = src.to_lowercase();
    for tok in tokens {
        if lower.contains(tok.as_str()) {
            hits.push(format!("{} -> {tok:?}", path.display()));
        }
    }
}

#[test]
fn current_state_docs_contain_no_obsolete_architecture_claims() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tokens = forbidden_tokens();
    let mut hits = Vec::new();
    for doc in ["CLAUDE.md", "README.md"] {
        let path = root.join(doc);
        if path.exists() {
            scan(&path, &tokens, &mut hits);
        }
    }
    assert!(
        hits.is_empty(),
        "affirmation(s) obsolète(s) (architecture pré-ADR-033) dans la documentation courante :\n{}",
        hits.join("\n")
    );
}
