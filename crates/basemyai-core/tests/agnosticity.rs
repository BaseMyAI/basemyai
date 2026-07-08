//! Test d'agnosticité (ADR-001) : `basemyai-core` ne doit contenir **aucun**
//! concept métier. Échoue si un terme interdit apparaît dans le *code* (les
//! commentaires `//` sont retirés avant le scan).

use std::fs;
use std::path::Path;

/// Concepts métier interdits dans le socle (mémoire d'agent + sémantique code).
const FORBIDDEN: &[&str] = &["agent_id", "valid_from", "valid_until", "episodic", "Symbol", "Edge"];

fn scan(dir: &Path, hits: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            scan(&path, hits);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let src = fs::read_to_string(&path).expect("read source");
            for (i, line) in src.lines().enumerate() {
                // Retire tout commentaire de ligne (`//`, `//!`, trailing) avant le scan.
                let code = line.split("//").next().unwrap_or("");
                for tok in FORBIDDEN {
                    if code.contains(tok) {
                        hits.push(format!("{}:{} -> {tok}", path.display(), i + 1));
                    }
                }
            }
        }
    }
}

#[test]
fn core_contains_no_business_concept() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    scan(&src, &mut hits);
    assert!(
        hits.is_empty(),
        "concept(s) métier interdit(s) dans basemyai-core (le sens va dans le consommateur) :\n{}",
        hits.join("\n")
    );
}
