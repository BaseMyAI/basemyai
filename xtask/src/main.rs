//! xtask — reproduit localement la matrice CI (`.github/workflows/ci.yml`).
//!
//! La CI ne lance PAS `cargo clippy --workspace` : elle cible chaque crate
//! avec `-p` et des combinaisons de features précises (un workspace virtuel
//! interdit `--features` à la racine). Ce binaire encode cette matrice pour
//! qu'un run local vert implique une CI verte.
//!
//! Usage : `cargo xtask <check|test|test-embed|ci>`.
//!
//! Toute évolution de `ci.yml` doit être répercutée ici (et inversement).

use std::process::{Command, exit};

/// Matrice clippy du job `gate` (ci.yml, étape « Clippy (-D warnings) »).
const CLIPPY: &[&[&str]] = &[
    &[
        "clippy",
        "-p",
        "basemyai-core",
        "--all-targets",
        "--features",
        "test-util",
        "--",
        "-D",
        "warnings",
    ],
    &[
        "clippy",
        "-p",
        "basemyai-engine",
        "--all-targets",
        "--features",
        "test-util",
        "--",
        "-D",
        "warnings",
    ],
    &[
        "clippy",
        "-p",
        "basemyai",
        "--features",
        "test-util",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ],
    &[
        "clippy",
        "-p",
        "basemyai-mcp",
        "--no-default-features",
        "--features",
        "stdio,http,test-util",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ],
    &[
        "clippy",
        "-p",
        "basemyai-rest",
        "--no-default-features",
        "--features",
        "test-util",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ],
    &["clippy", "-p", "basemyai-cli", "--all-targets", "--", "-D", "warnings"],
    &[
        "clippy",
        "-p",
        "basemyai-py",
        "--no-default-features",
        "--features",
        "test-util",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ],
    &[
        "clippy",
        "-p",
        "basemyai-node",
        "--no-default-features",
        "--features",
        "test-util",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ],
];

/// Matrice de tests du job `gate` (config légère : ni Candle ni tests `#[ignore]`).
const TEST: &[&[&str]] = &[
    &["test", "-p", "basemyai-core", "--features", "test-util"],
    // `--lib --bins --test basic --test vector_recall --test vector_persistence
    // --test vector_churn --test graph_parity` : les tests unitaires du
    // moteur natif + le harnais recall de l'index vectoriel (N3, oracle
    // brute-force, N=2000 — le N=10000 reste `#[ignore]`, run manuel) + le
    // harnais persistance KV de l'index (N3 étape 3 : round-trip reopen,
    // rebuild depuis les vecteurs) + le harnais churn insert/delete (N3
    // étape 4 : tombstones, consolidation FreshDiskANN, recall@10 ≥ 0.9
    // APRÈS churn — critère ADR-026 §6) + la parité du graphe natif (N4 :
    // scénarios de `crates/basemyai/tests/graph.rs` portés fidèlement contre
    // les deux flavors RAM/persistant), SANS `crash_consistency` (kill-loop
    // lent, job CI dédié / `test-crash-consistency`) ni `format_lock` (gate
    // dédié `FORMAT_LOCK`, inclus dans `check`/`ci`).
    &[
        "test",
        "-p",
        "basemyai-engine",
        "--features",
        "test-util",
        "--lib",
        "--bins",
        "--test",
        "basic",
        "--test",
        "vector_recall",
        "--test",
        "vector_persistence",
        "--test",
        "vector_churn",
        "--test",
        "graph_parity",
        "--test",
        "malformed_open",
    ],
    &["test", "-p", "basemyai", "--features", "test-util"],
    // `--test memory_tests` : runner déclaratif du contrat MemoryStore sur le
    // backend natif (clair + chiffré), zéro divergence tolérée.
    &[
        "test",
        "-p",
        "basemyai",
        "--features",
        "test-util",
        "--test",
        "memory_tests",
    ],
    // Isolation adversariale P1 (ADR-006) — agent_id hostile, FTS hostile, etc.
    &[
        "test",
        "-p",
        "basemyai",
        "--features",
        "test-util",
        "--test",
        "p1_isolation_adversarial",
    ],
    // Audit sécurité 2026-07-08 — poisoning, isolation export/graphe, temporel.
    &[
        "test",
        "-p",
        "basemyai",
        "--features",
        "test-util",
        "--test",
        "poisoning_procedural_recall",
        "--test",
        "provenance_trust",
        "--test",
        "plaintext_open_forbidden",
        "--test",
        "temporal_replacement_ci",
        "--test",
        "temporal_dedup_consolidation",
        "--test",
        "export_isolation_adversarial",
        "--test",
        "isolation_recall_graph_adversarial",
    ],
    &[
        "test",
        "-p",
        "basemyai",
        "--features",
        "test-util",
        "--test",
        "zero_network_recall",
    ],
    // `--test native_memory_store_bench` : KNN via le chemin `MemoryStore`
    // complet (N5.5) — la variante N=2000 seule tourne ici (rapide) ; la
    // variante N=10000 reste `#[ignore]`, run manuel (même convention que
    // `vector_recall`).
    &[
        "test",
        "-p",
        "basemyai",
        "--features",
        "test-util",
        "--test",
        "native_memory_store_bench",
    ],
    &[
        "test",
        "-p",
        "basemyai-mcp",
        "--no-default-features",
        "--features",
        "stdio,http,test-util",
    ],
    &[
        "test",
        "-p",
        "basemyai-rest",
        "--no-default-features",
        "--features",
        "test-util",
    ],
    &["test", "-p", "basemyai-cli"],
];

/// Job `embed` (compile Candle — lourd ; les tests réels sont `#[ignore]`).
const TEST_EMBED: &[&[&str]] = &[
    &["test", "-p", "basemyai-core", "--features", "embed,test-util"],
    &["test", "-p", "basemyai", "--features", "embed,test-util"],
];

/// `format.lock` anti-drift check (ADR-025, `docs/PLAN-NATIVE-ENGINE.md`
/// §3.1/§4) : chaque type persisté de `basemyai-engine` (WAL, SST) est
/// versionné et son hash de format doit matcher `format.lock`, sinon échec.
/// Pas de features spéciales : ce crate ne compile qu'en config par défaut.
const FORMAT_LOCK: &[&[&str]] = &[&["test", "-p", "basemyai-engine", "--test", "format_lock"]];

/// Job `crash-consistency` (N2, `docs/TODO-NATIVE-ENGINE.md` : « le harnais
/// d'abord, le moteur ensuite ») : spawn du binaire `crash_writer`, kill
/// forcé (`taskkill /F`/`kill -9`) en boucle (~20 cycles), réouverture +
/// vérification d'intégrité. Séparé du gate léger : plus lent (~10-15 s),
/// spawn/kill de process réels.
const TEST_CRASH_CONSISTENCY: &[&[&str]] = &[&[
    "test",
    "-p",
    "basemyai-engine",
    "--features",
    "test-util",
    "--test",
    "crash_consistency",
    "--",
    "--nocapture",
]];

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();

    match cmd.as_str() {
        "check" => {
            fmt_check();
            doc_drift_check();
            run_all(CLIPPY);
            run_all(FORMAT_LOCK);
        }
        "test" => run_all(TEST),
        "test-embed" => run_all(TEST_EMBED),
        "format-lock" => run_all(FORMAT_LOCK),
        "doc-drift" => doc_drift_check(),
        "test-crash-consistency" => run_all(TEST_CRASH_CONSISTENCY),
        "ci" => {
            fmt_check();
            doc_drift_check();
            run_all(CLIPPY);
            run_all(TEST);
            run_all(FORMAT_LOCK);
            println!(
                "\nGate CI léger vert. Le job `embed` reste séparé en CI :\n\
                 lance `cargo xtask test-embed` (Candle, compilation lourde) \
                 pour couvrir la matrice complète."
            );
        }
        "" | "help" | "-h" | "--help" => usage(0),
        other => {
            eprintln!("sous-commande inconnue : `{other}`\n");
            usage(2);
        }
    }
}

fn usage(code: i32) -> ! {
    println!(
        "cargo xtask — reproduit la matrice CI (.github/workflows/ci.yml) en local\n\n\
         USAGE : cargo xtask <SOUS-COMMANDE>\n\n\
         SOUS-COMMANDES :\n\
         \x20 check        fmt --check + doc-drift + clippy par crate, features CI, -D warnings + format.lock\n\
         \x20 test         tests par crate, config légère (sans embed)\n\
         \x20 test-embed   tests du job CI `embed` (Candle — compilation lourde)\n\
         \x20 format-lock  vérifie basemyai-engine/format.lock contre les specs de format actuelles\n\
         \x20 doc-drift     refuse les mentions libSQL/SQLCipher obsolètes (ADR-033)\n\
         \x20 test-crash-consistency  kill/reopen/verify en boucle sur basemyai-engine (~20 cycles)\n\
         \x20 ci           check + test (embed/crash-consistency restent des jobs séparés)\n\
         \x20 help         affiche cette aide\n\n\
         NB : `cargo clippy --workspace` ne reproduit PAS la CI (features par crate)."
    );
    exit(code);
}

/// `cargo fmt --all --check` comme le job `format` de la CI — seulement si le
/// repo a une config rustfmt (`.rustfmt.toml` à la racine du workspace).
fn fmt_check() {
    let root = workspace_root();
    if root.join(".rustfmt.toml").exists() || root.join("rustfmt.toml").exists() {
        run(&["fmt", "--all", "--check"]);
    } else {
        println!("(pas de .rustfmt.toml — fmt --check sauté)");
    }
}

/// Refuse les mentions actives de libSQL/SQLCipher dans le code produit
/// (ADR-033). `"adaptive forgetting"` faisait partie de cette liste comme
/// garde-fou anti-réintroduction silencieuse (le mécanisme avait été retiré
/// sans portage, ADR-033) ; retiré depuis que ADR-037 documente son portage
/// natif en bonne et due forme — le garde-fou a fait son travail.
fn doc_drift_check() {
    let root = workspace_root();
    let patterns = [
        "libsqlmemorystore",
        "sqlcipher",
        "libsql's built-in",
        "feature `crypto`",
    ];
    let scan_roots = ["crates", "bindings"];
    let mut violations = Vec::new();

    for scan_root in scan_roots {
        let base = root.join(scan_root);
        if !base.is_dir() {
            continue;
        }
        walk_for_doc_drift(&base, &patterns, &mut violations);
    }

    let cargo_toml = root.join("Cargo.toml");
    if cargo_toml.is_file() {
        check_file_doc_drift(&cargo_toml, &patterns, &mut violations);
    }

    if violations.is_empty() {
        println!("doc-drift : OK");
        return;
    }

    eprintln!("doc-drift : mentions interdites trouvées :");
    for (path, line_no, line) in violations {
        eprintln!("  {}:{}: {}", path.display(), line_no, line.trim());
    }
    exit(1);
}

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask vit à la racine du workspace : un parent existe toujours")
        .to_path_buf()
}

fn walk_for_doc_drift(
    dir: &std::path::Path,
    patterns: &[&str],
    violations: &mut Vec<(std::path::PathBuf, usize, String)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target" || n == "fuzz") {
                continue;
            }
            walk_for_doc_drift(&path, patterns, violations);
        } else if path
            .extension()
            .is_some_and(|e| e == "rs" || e == "toml" || e == "yaml" || e == "yml" || e == "md")
        {
            check_file_doc_drift(&path, patterns, violations);
        }
    }
}

fn check_file_doc_drift(
    path: &std::path::Path,
    patterns: &[&str],
    violations: &mut Vec<(std::path::PathBuf, usize, String)>,
) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    for (i, line) in content.lines().enumerate() {
        let lower = line.to_lowercase();
        if patterns.iter().any(|p| lower.contains(p)) {
            violations.push((path.to_path_buf(), i + 1, line.to_string()));
        }
    }
}

fn run_all(cmds: &[&[&str]]) {
    for cmd in cmds {
        run(cmd);
    }
}

/// Lance `cargo <args>`, affiche la commande, s'arrête au premier échec en
/// propageant le code de sortie.
fn run(args: &[&str]) {
    println!("→ cargo {}", args.join(" "));
    let status = Command::new("cargo").args(args).status().unwrap_or_else(|e| {
        eprintln!("impossible de lancer cargo : {e}");
        exit(1);
    });
    if !status.success() {
        eprintln!("✗ échec : cargo {}", args.join(" "));
        exit(status.code().unwrap_or(1));
    }
}
