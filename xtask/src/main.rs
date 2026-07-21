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
    // + N7 : `engine_stats` (compteurs/jauges EngineStats), `failpoints`
    // (injection d'erreurs aux frontières de durabilité) et
    // `corruption_smoke` (bit-flip SST/WAL/crypto.meta → erreurs typées,
    // gap manifest N9 pinné) — smoke tests du gate PR (PLAN §8.3).
    // + N12/ADR-042 : `adr042_contract` (verrou advisory en écriture,
    // zeroization de `EncryptionKey` au point de génération/persistance,
    // non-substitution silencieuse RawKey/Argon2id) — rapide, pas un
    // kill-loop, donc dans le gate léger contrairement à `crash_consistency`.
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
        "--test",
        "engine_stats",
        "--test",
        "failpoints",
        "--test",
        "corruption_smoke",
        "--test",
        "adr042_contract",
        // J0 preflight (ENG-DUR-002/004,
        // docs/audits/2026-07-engine-architecture-safety-audit.md).
        "--test",
        "generation_pointer_loss_is_rejected_and_gen1_survives",
        "--test",
        "compaction_remove_retry",
        // N11.2/N11.3 — présents dans ci.yml depuis leur introduction mais
        // absents d'ici ; drift xtask/ci.yml pré-existant (même défaut que
        // celui documenté §8.3/status.md pour adr042_contract), corrigé au
        // passage en ajoutant ce plan J0 juste au-dessus.
        "--test",
        "model_based",
        "--test",
        "io_faults",
        // N13/J3 (ADR-043 §2 amendé) : snapshots S1 + suppression différée
        // des SST remplacées par compaction.
        "--test",
        "snapshot_compaction",
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

/// `engine-check` (N7.3) : validation moteur complète en une commande —
/// clippy + TOUS les tests de `basemyai-engine` (y compris le kill-loop
/// `crash_consistency`, ~10-15 s) + `format.lock`. Plus large que l'entrée
/// moteur du gate (qui exclut le kill-loop, job CI dédié) : c'est le harnais
/// unifié du plan, pas le gate rapide.
const ENGINE_CHECK: &[&[&str]] = &[
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
    &["test", "-p", "basemyai-engine", "--features", "test-util"],
    &["test", "-p", "basemyai-engine", "--test", "format_lock"],
];

/// `engine-corrupt` (N7.3) : les tests adversariaux de corruption seuls
/// (déjà inclus dans le gate ; entrée dédiée pour itérer dessus).
const ENGINE_CORRUPT: &[&[&str]] = &[&[
    "test",
    "-p",
    "basemyai-engine",
    "--features",
    "test-util",
    "--test",
    "corruption_smoke",
    "--test",
    "malformed_open",
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
        // ── Commandes moteur N7.3 (PLAN-NATIVE-ENGINE §4.3) ──────────────
        // Lourdes ou à sortie chiffrée : jamais dans `ci`, mêmes invocations
        // en local qu'en CI nightly quand elle existera.
        "engine-check" => run_all(ENGINE_CHECK),
        "engine-crash" => run_all(TEST_CRASH_CONSISTENCY),
        "engine-corrupt" => run_all(ENGINE_CORRUPT),
        "engine-bench" => engine_bench(&args.collect::<Vec<_>>()),
        "engine-soak" => engine_soak(&args.collect::<Vec<_>>()),
        "engine-fuzz" => engine_fuzz(),
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
         \x20 engine-check  clippy + TOUS les tests basemyai-engine (kill-loop inclus) + format.lock\n\
         \x20 engine-bench  banc canonique (release), clair PUIS chiffré — args passés au binaire\n\
         \x20 engine-crash  alias de test-crash-consistency (nommage PLAN §4.3)\n\
         \x20 engine-corrupt  tests adversariaux de corruption (corruption_smoke + malformed_open)\n\
         \x20 engine-soak   boucle du banc (défaut 10 cycles à n=100000) — manuel/nightly\n\
         \x20 engine-fuzz   cibles cargo-fuzz (WSL/Linux seulement — libFuzzer ≠ Windows natif)\n\
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

/// `engine-bench` (N7.3) : le banc canonique `engine_bench` en release,
/// **deux fois** — clair puis chiffré (le workload `encrypted-vs-clear` du
/// plan est la paire de rapports). Arguments passés tels quels au binaire
/// (`--n`, `--memory-n`, `--out` …) ; `--out chemin.json` produit
/// `chemin.json` (clair) et `chemin.encrypted.json` (chiffré).
fn engine_bench(extra: &[String]) {
    let base: Vec<&str> = vec![
        "run",
        "--release",
        "-p",
        "basemyai-engine",
        "--features",
        "test-util",
        "--bin",
        "engine_bench",
        "--",
    ];
    // Sépare un éventuel `--out` pour suffixer la variante chiffrée.
    let mut clear_args: Vec<String> = Vec::new();
    let mut out: Option<String> = None;
    let mut it = extra.iter();
    while let Some(arg) = it.next() {
        if arg == "--out" {
            out = it.next().cloned();
        } else {
            clear_args.push(arg.clone());
        }
    }
    for encrypted in [false, true] {
        let mut args: Vec<String> = base.iter().map(ToString::to_string).collect();
        args.extend(clear_args.iter().cloned());
        if encrypted {
            args.push("--encrypted".into());
        }
        if let Some(out) = &out {
            args.push("--out".into());
            args.push(if encrypted {
                out.replace(".json", ".encrypted.json")
            } else {
                out.clone()
            });
        }
        run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    }
}

/// `engine-soak` (N7.3) : boucle du banc en continu (défaut : 10 cycles à
/// n=100 000, clair+chiffré alternés). Usage manuel/nightly — jamais le gate.
/// `cargo xtask engine-soak [cycles] [n]`.
fn engine_soak(extra: &[String]) {
    let cycles: u64 = extra.first().and_then(|v| v.parse().ok()).unwrap_or(10);
    let n = extra.get(1).cloned().unwrap_or_else(|| "100000".to_string());
    for cycle in 1..=cycles {
        println!("── engine-soak cycle {cycle}/{cycles} (n={n}) ──");
        engine_bench(&["--n".to_string(), n.clone()]);
    }
}

/// `engine-fuzz` (N7.3) : pointeur d'exécution des cibles cargo-fuzz.
/// libFuzzer ne linke pas sous Windows natif (contrainte documentée depuis
/// N2, `crates/basemyai-engine/fuzz/README.md`) — sous Windows cette
/// commande imprime la marche à suivre WSL et échoue explicitement plutôt
/// que de prétendre avoir fuzzé.
fn engine_fuzz() {
    if cfg!(windows) {
        eprintln!(
            "engine-fuzz : libFuzzer ne linke pas sous Windows natif.\n\
             Lancer sous WSL/Linux :\n\
             \x20 cd crates/basemyai-engine/fuzz\n\
             \x20 cargo +nightly fuzz run <cible> -- -max_total_time=300\n\
             Cibles : voir crates/basemyai-engine/fuzz/fuzz_targets/ et fuzz/README.md."
        );
        exit(1);
    }
    let root = workspace_root().join("crates/basemyai-engine/fuzz");
    println!("→ cargo +nightly fuzz list (dans {})", root.display());
    let status = Command::new("cargo")
        .args(["+nightly", "fuzz", "list"])
        .current_dir(&root)
        .status();
    match status {
        Ok(s) if s.success() => {
            println!(
                "Lancer chaque cible : cargo +nightly fuzz run <cible> -- -max_total_time=300\n\
                 (depuis {})",
                root.display()
            );
        }
        _ => {
            eprintln!("cargo-fuzz indisponible — installer : cargo install cargo-fuzz (toolchain nightly requise)");
            exit(1);
        }
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
