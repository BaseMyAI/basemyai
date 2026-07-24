// SPDX-License-Identifier: BUSL-1.1
//! Mesure reproductible du profil Argon2id retenu par ADR-042.
//!
//! ```text
//! cargo run --release -p basemyai-engine --features test-util --bin argon2_bench -- [iterations]
//! ```
//!
//! La sortie est volontairement compacte et machine-readable. Ce binaire ne
//! touche aucun format ni chemin d'ouverture : c'est l'instrumentation PR2
//! qui valide le coût du profil avant son branchement dans `crypto.meta:2`.

use std::time::{Duration, Instant};

use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::Zeroizing;

const MEMORY_KIB: u32 = 65_536;
const TIME_COST: u32 = 3;
const PARALLELISM: u32 = 4;
const OUTPUT_LEN: usize = 32;
const PASSPHRASE: &[u8] = b"basemyai-argon2-benchmark-passphrase";
const SALT: &[u8; 16] = b"bmai-kdf-benchv1";

fn main() {
    let iterations = std::env::args()
        .nth(1)
        .map_or(Ok(5), |value| value.parse::<u32>())
        .unwrap_or_else(|error| fatal(&format!("iterations must be a positive integer: {error}")));
    if iterations == 0 {
        fatal("iterations must be greater than zero");
    }

    let params = Params::new(MEMORY_KIB, TIME_COST, PARALLELISM, Some(OUTPUT_LEN))
        .unwrap_or_else(|error| fatal(&format!("invalid ADR-042 Argon2id parameters: {error}")));
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut samples = Vec::with_capacity(iterations as usize);

    for _ in 0..iterations {
        let mut output = Zeroizing::new([0_u8; OUTPUT_LEN]);
        let started = Instant::now();
        argon2
            .hash_password_into(PASSPHRASE, SALT, output.as_mut())
            .unwrap_or_else(|error| fatal(&format!("Argon2id derivation failed: {error}")));
        samples.push(started.elapsed());
    }

    samples.sort_unstable();
    let total: Duration = samples.iter().sum();
    let mean = total / iterations;
    let median = samples[(iterations as usize - 1) / 2];
    let p95 = samples[((iterations as usize * 95).div_ceil(100)).saturating_sub(1)];
    println!(
        "{{\"algorithm\":\"argon2id\",\"m_kib\":{MEMORY_KIB},\"t_cost\":{TIME_COST},\"p\":{PARALLELISM},\"iterations\":{iterations},\"mean_ms\":{},\"median_ms\":{},\"p95_ms\":{}}}",
        mean.as_secs_f64() * 1_000.0,
        median.as_secs_f64() * 1_000.0,
        p95.as_secs_f64() * 1_000.0,
    );
}

fn fatal(message: &str) -> ! {
    eprintln!("argon2_bench: {message}");
    std::process::exit(2);
}
