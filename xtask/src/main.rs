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
    &["clippy", "-p", "basemyai-core", "--all-targets", "--", "-D", "warnings"],
    &[
        "clippy",
        "-p",
        "basemyai-engine",
        "--all-targets",
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
    &["test", "-p", "basemyai-core"],
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
    &["test", "-p", "basemyai-core", "--features", "embed"],
    &["test", "-p", "basemyai", "--features", "embed"],
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
            run_all(CLIPPY);
            run_all(FORMAT_LOCK);
        }
        "test" => run_all(TEST),
        "test-embed" => run_all(TEST_EMBED),
        "format-lock" => run_all(FORMAT_LOCK),
        "test-crash-consistency" => run_all(TEST_CRASH_CONSISTENCY),
        "ci" => {
            fmt_check();
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
         \x20 check        fmt --check + clippy par crate, features CI, -D warnings + format.lock\n\
         \x20 test         tests par crate, config légère (sans embed)\n\
         \x20 test-embed   tests du job CI `embed` (Candle — compilation lourde)\n\
         \x20 format-lock  vérifie basemyai-engine/format.lock contre les specs de format actuelles\n\
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
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask vit à la racine du workspace : un parent existe toujours");
    if root.join(".rustfmt.toml").exists() || root.join("rustfmt.toml").exists() {
        run(&["fmt", "--all", "--check"]);
    } else {
        println!("(pas de .rustfmt.toml — fmt --check sauté)");
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
