//! xtask — reproduit localement la matrice CI (`.github/workflows/ci.yml`).
//!
//! La CI ne lance PAS `cargo clippy --workspace` : elle cible chaque crate
//! avec `-p` et des combinaisons de features précises (un workspace virtuel
//! interdit `--features` à la racine). Ce binaire encode cette matrice pour
//! qu'un run local vert implique une CI verte.
//!
//! Usage : `cargo xtask <check|test|test-embed|test-crypto|ci>`.
//!
//! Toute évolution de `ci.yml` doit être répercutée ici (et inversement).

use std::process::{Command, exit};

/// Matrice clippy du job `gate` (ci.yml, étape « Clippy (-D warnings) »).
/// NB : `basemyai-cli` n'est pas dans le gate CI — fidélité stricte à ci.yml.
const CLIPPY: &[&[&str]] = &[
    &["clippy", "-p", "basemyai-core", "--all-targets", "--", "-D", "warnings"],
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

/// Matrice de tests du job `gate` (config légère : ni Candle ni CMake).
const TEST: &[&[&str]] = &[
    &["test", "-p", "basemyai-core"],
    &["test", "-p", "basemyai", "--features", "test-util"],
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
];

/// Job `embed` (compile Candle — lourd ; les tests réels sont `#[ignore]`).
const TEST_EMBED: &[&[&str]] = &[
    &["test", "-p", "basemyai-core", "--features", "embed"],
    &["test", "-p", "basemyai", "--features", "embed"],
];

/// Job `crypto` (chiffrement libSQL — EXIGE CMake installé).
const TEST_CRYPTO: &[&[&str]] = &[
    &["test", "-p", "basemyai-core", "--features", "crypto"],
    &["test", "-p", "basemyai", "--features", "crypto"],
];

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();

    match cmd.as_str() {
        "check" => {
            fmt_check();
            run_all(CLIPPY);
        }
        "test" => run_all(TEST),
        "test-embed" => run_all(TEST_EMBED),
        "test-crypto" => {
            eprintln!(
                "⚠ la feature `crypto` compile libsql avec chiffrement : CMake doit être \
                 installé (et `cp` dans le PATH sous Windows — Git usr/bin le fournit)."
            );
            run_all(TEST_CRYPTO);
        }
        "ci" => {
            fmt_check();
            run_all(CLIPPY);
            run_all(TEST);
            println!(
                "\nGate CI léger vert. Les jobs `embed` et `crypto` sont séparés en CI :\n\
                 lance `cargo xtask test-embed` (Candle, lourd) et `cargo xtask test-crypto` \
                 (CMake requis) pour la matrice complète."
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
         \x20 check        fmt --check + clippy par crate, features CI, -D warnings\n\
         \x20 test         tests par crate, config légère (sans embed/crypto)\n\
         \x20 test-embed   tests du job CI `embed` (Candle — compilation lourde)\n\
         \x20 test-crypto  tests du job CI `crypto` (chiffrement libSQL — CMake requis)\n\
         \x20 ci           check + test (embed/crypto restent des jobs séparés)\n\
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
