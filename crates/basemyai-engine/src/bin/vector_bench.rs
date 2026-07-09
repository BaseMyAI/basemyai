// SPDX-License-Identifier: BUSL-1.1
//! Manual N3 parity-bench harness (ADR-026 §6,
//! `docs/TODO-NATIVE-ENGINE.md` N3 "Parité bench M6"): reproduces the same
//! scenario shape as `docs/benchmarks/m6-knn-results-2026-07-01.md`
//! (10k/100k rows, k=10, cosine, 384d) against
//! [`basemyai_engine::PersistentVectorIndex`] — the real `remember` insert
//! path (one vector at a time, not a bulk-load-then-index shortcut like
//! M6's harness had to resort to) — instead of libSQL's native
//! `vector_top_k`. Not a Criterion microbench: a realistic, timed,
//! print-as-you-go scenario, the same shape as the M6 script, so the two
//! numbers are comparable at face value without a statistics layer getting
//! in the way of a multi-hour run.
//!
//! Not wired into `cargo xtask` (a manual tool, like `crash_writer` has its
//! own dedicated CI job instead of living in the default `check`/`test`
//! gate) — run it explicitly, in release (the M6 numbers were release too):
//!
//! ```text
//! cargo run --release -p basemyai-engine --bin vector_bench -- <n> [engine_dir]
//! ```
//!
//! N3.1 follow-up (`docs/benchmarks/n3-vector-scale-followup-2026-07-05.md`):
//! scale-up runs beyond the N3 10k/100k checkpoints. Env vars:
//!
//! - `VECTOR_BENCH_KEEP=1` — leave the engine directory on disk afterwards
//!   instead of deleting it (inspect the measured disk footprint by hand).
//! - `VECTOR_BENCH_SKIP_ORACLE=1` — do not retain the full `n`-vector
//!   dataset in memory and do not compute recall. At `n` in the hundreds of
//!   thousands the retained `Vec<Vec<f32>>` oracle copy (384×4 bytes/row —
//!   ≈1.43 GiB at 1M) becomes the dominant, and misleading, share of
//!   process RAM; this flag drops it so the RAM sampler reports the index's
//!   own footprint instead of the oracle's. Query latency is still
//!   measured (against freshly generated query vectors); recall prints as
//!   `skipped`. Recommended for any `n` above ~100 000 — see the
//!   scale-up doc for why exact recall at that size needs a *separate*,
//!   smaller, oracle-enabled run instead of one giant one.
//! - `VECTOR_BENCH_RAM_INTERVAL_MS=<ms>` — RAM sampler interval, default
//!   1000. Lower values cost a bit of sampler-thread overhead; higher
//!   values coarsen the peak estimate.
//! - `VECTOR_BENCH_RAM_LOG=<path>` — also write every `(elapsed_s,
//!   rss_bytes)` sample as CSV to this path, for later plotting.
//!
//! ## In-process RAM sampling, not an external poller
//!
//! `docs/benchmarks/n3-vector-parity-2026-07-05.md`'s RAM section documents
//! a real failure: an external `Start-Job`-backgrounded PowerShell
//! `Get-Process` poller died silently when its launching shell exited,
//! leaving only a handful of ad hoc manual snapshots (a floor, not a peak).
//! This harness now samples its **own** process RSS from a background
//! thread inside the same process (see the `ram` module below) — it lives
//! and dies with the benchmark itself, so it cannot be silently torn down
//! by an unrelated parent shell. It still only reports **whole-process**
//! RSS (dataset generator + oracle copy, if enabled + the index's own
//! caches/memtable + the harness itself) — there is no allocator-level way
//! to isolate "the index's bytes" from that total without instrumenting
//! the allocator, which this manual tool does not do. Say so plainly in any
//! report generated from these numbers rather than implying an isolated
//! figure.
//!
//! ## Deliberate protocol difference from M6: the vector generator
//!
//! This harness does **not** reuse libSQL M6's `synthetic_vector` (iid
//! uniform, then L2-normalized — see
//! `crates/basemyai-core/benches/knn_scalability.rs`). iid-uniform 384d is a
//! documented pathology for graph-ANN indexes of *any* family, not specific
//! to this implementation: pairwise cosine distances concentrate around 0
//! (concentration of measure in high dimension), so there is no neighborhood
//! structure for a proximity graph to navigate and recall collapses as N
//! grows regardless of tuning (`tests/common/mod.rs`'s module doc, measured
//! there at 0.664 recall@10 at N=10 000). Real embeddings (MiniLM) have low
//! intrinsic dimensionality, so this harness reuses the same generator as
//! the ADR-026 §6 recall gate (`tests/vector_recall.rs`,
//! `tests/common::LatentData`): a small latent space pushed through a fixed
//! seeded random linear map into the 384d ambient space. Duplicated here
//! (not imported) because `tests/common` lives under `tests/`, which is not
//! visible to a `src/bin` target. This is a real, called-out difference from
//! the libSQL M6 protocol — see the archived report's "limites" section for
//! what it does and doesn't affect (query latency and disk/build cost are
//! shape-driven, not data-distribution-driven; recall numbers are only
//! comparable to the other ADR-026-gated numbers, not to a hypothetical
//! "libSQL on the same data" run that was never measured).
//!
//! ## What is and isn't measured
//!
//! - **Build**: total wall time and ms/row for `n` sequential
//!   `PersistentVectorIndex::insert` calls (the real incremental path any
//!   consumer's `remember` takes — not the bulk-load-then-index shortcut M6
//!   had to add for libSQL), plus one final `Engine::flush()`.
//! - **Query**: `NUM_QUERIES` × k=`K` searches, mean/p50/p95 latency.
//! - **Recall@`K`**: against an exact brute-force oracle over the same `n`
//!   vectors, on the same `NUM_QUERIES` query vectors — unless
//!   `VECTOR_BENCH_SKIP_ORACLE` is set, in which case this is skipped (see
//!   above).
//! - **Disk**: total byte size of the engine directory after build + flush.
//! - **RAM**: whole-process RSS, sampled continuously by a background
//!   thread in this same process (see the `ram` module). Not an isolated
//!   allocator-attributed figure for the index alone — see above.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use basemyai_engine::idx::vector::distance::cosine_distance;
use basemyai_engine::{Engine, EngineOptions, PersistentVectorIndex, VectorIndexParams};

/// Embedding dimension (`all-MiniLM-L6-v2`, matches the ADR-026 default and
/// the M6 protocol).
const DIM: usize = 384;
/// Intrinsic (latent) dimensionality of the generated dataset — see the
/// module doc and `tests/common/mod.rs`.
const LATENT_DIM: usize = 16;
/// Number of query vectors, matching M6's per-size query sampling shape
/// closely enough for a realistic scenario (M6 measured steady-state query
/// latency via Criterion sampling; this measures `NUM_QUERIES` individual
/// calls directly since there is no Criterion harness here).
const NUM_QUERIES: usize = 100;
const K: usize = 10;
const BENCH_CRYPTO_KEY: &[u8] = b"vector-bench-dev-key";

fn main() {
    let mut args = env::args().skip(1);
    let Some(n) = args.next().and_then(|s| s.parse::<usize>().ok()) else {
        eprintln!("usage: vector_bench <n> [engine_dir]");
        eprintln!(
            "  env: VECTOR_BENCH_SKIP_ORACLE=1 (recommended above ~100000 rows), \
             VECTOR_BENCH_KEEP=1, VECTOR_BENCH_RAM_INTERVAL_MS=<ms>, \
             VECTOR_BENCH_RAM_LOG=<path>"
        );
        std::process::exit(1);
    };
    let engine_dir = args.next().map(PathBuf::from).unwrap_or_else(|| default_dir(n));
    let keep = env::var("VECTOR_BENCH_KEEP").is_ok();
    let skip_oracle = env::var("VECTOR_BENCH_SKIP_ORACLE").is_ok();
    let ram_interval = Duration::from_millis(
        env::var("VECTOR_BENCH_RAM_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000),
    );
    let ram_log_path = env::var("VECTOR_BENCH_RAM_LOG").ok().map(PathBuf::from);

    if engine_dir.exists() {
        fs::remove_dir_all(&engine_dir).unwrap_or_else(|e| {
            eprintln!("vector_bench: failed to clear stale dir {}: {e}", engine_dir.display());
            std::process::exit(1);
        });
    }

    let params = VectorIndexParams::with_dim(DIM);
    println!(
        "[vector_bench] n={n} dim={DIM} k={K} queries={NUM_QUERIES} dir={} skip_oracle={skip_oracle}",
        engine_dir.display()
    );
    println!(
        "[vector_bench] params: max_degree(R)={} beam_width(L)={} alpha={}",
        params.max_degree, params.beam_width, params.alpha
    );

    let mut generator = LatentData::new(0xBA5E_A126_2026_0705, DIM);

    let mut engine = Engine::open_encrypted_with_options(&engine_dir, BENCH_CRYPTO_KEY, EngineOptions::default())
        .unwrap_or_else(|e| {
            eprintln!("vector_bench: failed to open engine: {e}");
            std::process::exit(1);
        });
    let mut index = PersistentVectorIndex::open(&mut engine, params).unwrap_or_else(|e| {
        eprintln!("vector_bench: failed to open index: {e}");
        std::process::exit(1);
    });

    let sampler = ram::Sampler::start(ram_interval);

    let (build_elapsed, ms_per_row, vectors) = if skip_oracle {
        println!(
            "[vector_bench] build: generating+inserting {n} vectors one at a time \
             (oracle disabled, dataset not retained)..."
        );
        let build_start = Instant::now();
        let mut last_report = Instant::now();
        for id in 0..n {
            let vector = generator.point();
            if let Err(e) = index.insert(&mut engine, id as u64, vector) {
                eprintln!("vector_bench: insert({id}) failed: {e}");
                std::process::exit(1);
            }
            report_progress(id, n, build_start, &mut last_report);
        }
        finish_build(&mut engine);
        let build_elapsed = build_start.elapsed();
        let ms_per_row = build_elapsed.as_secs_f64() * 1000.0 / n as f64;
        (build_elapsed, ms_per_row, None)
    } else {
        println!("[vector_bench] generating {n} seeded low-intrinsic-dim vectors (latent_dim={LATENT_DIM})...");
        let vectors: Vec<Vec<f32>> = (0..n).map(|_| generator.point()).collect();

        println!("[vector_bench] build: inserting {n} vectors one at a time (real incremental remember path)...");
        let build_start = Instant::now();
        let mut last_report = Instant::now();
        for (id, vector) in vectors.iter().enumerate() {
            if let Err(e) = index.insert(&mut engine, id as u64, vector.clone()) {
                eprintln!("vector_bench: insert({id}) failed: {e}");
                std::process::exit(1);
            }
            report_progress(id, n, build_start, &mut last_report);
        }
        finish_build(&mut engine);
        let build_elapsed = build_start.elapsed();
        let ms_per_row = build_elapsed.as_secs_f64() * 1000.0 / n as f64;
        (build_elapsed, ms_per_row, Some(vectors))
    };

    let disk_bytes = dir_size(&engine_dir).unwrap_or_else(|e| {
        eprintln!("vector_bench: failed to measure disk size: {e}");
        std::process::exit(1);
    });

    println!("[vector_bench] querying: {NUM_QUERIES} x k={K}...");
    let mut latencies_ms: Vec<f64> = Vec::with_capacity(NUM_QUERIES);
    let mut hits = 0usize;
    let mut recall_measured = false;
    for _ in 0..NUM_QUERIES {
        let query = generator.point();
        let expected = vectors.as_ref().map(|v| brute_force_top_k(v, &query, K));
        let query_start = Instant::now();
        let got = index.search(&engine, &query, K).unwrap_or_else(|e| {
            eprintln!("vector_bench: search failed: {e}");
            std::process::exit(1);
        });
        latencies_ms.push(query_start.elapsed().as_secs_f64() * 1000.0);
        if let Some(expected) = expected {
            recall_measured = true;
            hits += got.iter().filter(|id| expected.contains(id)).count();
        }
    }

    let ram_samples = sampler.stop_and_collect();

    let mut sorted = latencies_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
    let p50 = percentile(&sorted, 0.50);
    let p95 = percentile(&sorted, 0.95);

    println!("=== vector_bench results: n={n} ===");
    println!("build_total={build_elapsed:?} build_ms_per_row={ms_per_row:.4}");
    println!("query_mean_ms={mean:.4} query_p50_ms={p50:.4} query_p95_ms={p95:.4} (n_queries={NUM_QUERIES}, k={K})");
    if recall_measured {
        let recall = hits as f64 / (NUM_QUERIES * K) as f64;
        println!("recall_at_{K}={recall:.4}");
    } else {
        println!("recall_at_{K}=skipped (VECTOR_BENCH_SKIP_ORACLE set — see module doc)");
    }
    println!(
        "disk_bytes={disk_bytes} disk_mib={:.2}",
        disk_bytes as f64 / (1024.0 * 1024.0)
    );
    print_ram_summary(&ram_samples, ram_log_path.as_deref());

    if keep {
        println!(
            "[vector_bench] VECTOR_BENCH_KEEP set: leaving {} on disk",
            engine_dir.display()
        );
    } else if let Err(e) = fs::remove_dir_all(&engine_dir) {
        eprintln!(
            "vector_bench: warning: failed to clean up {}: {e}",
            engine_dir.display()
        );
    }
}

fn report_progress(id: usize, n: usize, build_start: Instant, last_report: &mut Instant) {
    if last_report.elapsed().as_secs() >= 30 || id + 1 == n {
        println!(
            "[vector_bench]   ...{}/{n} inserted ({:.1?} elapsed, {:.3} ms/row so far)",
            id + 1,
            build_start.elapsed(),
            build_start.elapsed().as_secs_f64() * 1000.0 / (id + 1) as f64
        );
        *last_report = Instant::now();
    }
}

fn finish_build(engine: &mut Engine) {
    if let Err(e) = engine.flush() {
        eprintln!("vector_bench: final flush failed: {e}");
        std::process::exit(1);
    }
}

fn print_ram_summary(samples: &[(f64, u64)], log_path: Option<&std::path::Path>) {
    if samples.is_empty() {
        println!(
            "peak_rss=not_measured (unsupported platform, or run shorter than one sampler \
             interval — see `ram` module doc)"
        );
        return;
    }
    let peak = samples.iter().map(|(_, rss)| *rss).max().unwrap_or(0);
    let mean = samples.iter().map(|(_, rss)| *rss as f64).sum::<f64>() / samples.len() as f64;
    println!(
        "peak_rss_bytes={peak} peak_rss_mib={:.2} mean_rss_mib={:.2} ram_samples={} \
         (whole-process RSS, continuous in-process sampler — see module doc for what's included)",
        peak as f64 / (1024.0 * 1024.0),
        mean / (1024.0 * 1024.0),
        samples.len()
    );
    if let Some(path) = log_path {
        match write_ram_csv(path, samples) {
            Ok(()) => println!("[vector_bench] RAM samples written to {}", path.display()),
            Err(e) => eprintln!("vector_bench: warning: failed to write RAM log {}: {e}", path.display()),
        }
    }
}

fn write_ram_csv(path: &std::path::Path, samples: &[(f64, u64)]) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    writeln!(file, "elapsed_s,rss_bytes")?;
    for (elapsed_s, rss) in samples {
        writeln!(file, "{elapsed_s:.3},{rss}")?;
    }
    Ok(())
}

fn default_dir(n: usize) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    env::temp_dir().join(format!("basemyai-vector-bench-{}-{n}-{now}", std::process::id()))
}

/// Total byte size of every regular file under `dir`, recursively.
fn dir_size(dir: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total += dir_size(&path)?;
        } else {
            total += metadata.len();
        }
    }
    Ok(total)
}

/// Linear-interpolation-free percentile over an already-ascending-sorted
/// slice (nearest-rank method) — sufficient precision for a `NUM_QUERIES`
/// sample this small; no need for interpolation machinery.
fn percentile(sorted_ascending: &[f64], p: f64) -> f64 {
    if sorted_ascending.is_empty() {
        return 0.0;
    }
    let rank = ((p * sorted_ascending.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(sorted_ascending.len() - 1);
    sorted_ascending[rank]
}

/// Exact top-k by cosine distance — the recall oracle (same shape as
/// `tests/common::brute_force_top_k`).
fn brute_force_top_k(vectors: &[Vec<f32>], query: &[f32], k: usize) -> HashSet<u64> {
    let mut scored: Vec<(u64, f32)> = vectors
        .iter()
        .enumerate()
        .map(|(id, v)| (id as u64, cosine_distance(query, v)))
        .collect();
    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
    scored.into_iter().take(k).map(|(id, _)| id).collect()
}

/// Deterministic xorshift64* PRNG (duplicated from `tests/common/mod.rs` —
/// not visible from a `src/bin` target; see the module doc).
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [-1, 1).
    fn next_f32(&mut self) -> f32 {
        let bits = (self.next_u64() >> 40) as f32; // 24 random bits
        bits / (1u64 << 23) as f32 - 1.0
    }

    fn vector(&mut self, dim: usize) -> Vec<f32> {
        (0..dim).map(|_| self.next_f32()).collect()
    }
}

/// Seeded low-intrinsic-dimension dataset generator — duplicated from
/// `tests/common::LatentData` (see the module doc for why it isn't a shared
/// import).
struct LatentData {
    rng: XorShift64,
    basis: Vec<Vec<f32>>,
    dim: usize,
}

impl LatentData {
    fn new(seed: u64, dim: usize) -> Self {
        let mut rng = XorShift64::new(seed);
        let basis = (0..LATENT_DIM).map(|_| rng.vector(dim)).collect();
        Self { rng, basis, dim }
    }

    fn point(&mut self) -> Vec<f32> {
        let latent = self.rng.vector(LATENT_DIM);
        let mut ambient = vec![0.0f32; self.dim];
        for (z, axis) in latent.iter().zip(&self.basis) {
            for (out, &component) in ambient.iter_mut().zip(axis) {
                *out += z * component;
            }
        }
        ambient
    }
}

/// In-process RAM sampling (N3.1 follow-up): a background thread inside
/// this same process, sampling whole-process RSS at a fixed interval into
/// an in-memory buffer, started/stopped by the `main` flow above. Unlike
/// an external `Start-Job`-style poller (the approach that failed silently
/// for the N3 parity bench — see the top-level module doc), this thread
/// shares the process's lifetime: it cannot be torn down by some unrelated
/// launching shell exiting, because there is no such shell in the loop.
///
/// Trade-off, stated plainly: this still only reports **whole-process**
/// RSS. There is no portable, dependency-free `std` API for "just this
/// allocation" or even "just this thread's contribution" — getting that
/// would mean instrumenting the global allocator (`GlobalAlloc`), which
/// this manual one-off tool does not do. What it fixes is *reliability and
/// continuity* of the whole-process number, not its granularity.
mod ram {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    pub(crate) struct Sampler {
        stop: Arc<AtomicBool>,
        samples: Arc<Mutex<Vec<(f64, u64)>>>,
        handle: Option<JoinHandle<()>>,
    }

    impl Sampler {
        /// Spawns the sampling thread. If RSS reading isn't supported on
        /// this platform, the thread still runs but every sample is
        /// skipped, so `stop_and_collect` returns an empty vec and the
        /// caller reports "not measured" rather than fabricating zeros.
        pub(crate) fn start(interval: Duration) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let samples = Arc::new(Mutex::new(Vec::new()));
            let stop_thread = Arc::clone(&stop);
            let samples_thread = Arc::clone(&samples);
            let handle = std::thread::spawn(move || {
                let start = Instant::now();
                while !stop_thread.load(Ordering::Relaxed) {
                    if let Some(rss) = current_rss_bytes()
                        && let Ok(mut guard) = samples_thread.lock()
                    {
                        guard.push((start.elapsed().as_secs_f64(), rss));
                    }
                    std::thread::sleep(interval);
                }
            });
            Self {
                stop,
                samples,
                handle: Some(handle),
            }
        }

        pub(crate) fn stop_and_collect(mut self) -> Vec<(f64, u64)> {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
            self.samples.lock().map(|guard| guard.clone()).unwrap_or_default()
        }
    }

    #[cfg(target_os = "windows")]
    fn current_rss_bytes() -> Option<u64> {
        use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        // SAFETY: `GetCurrentProcess` returns a pseudo-handle valid for the
        // process's lifetime (no ownership to release); `counters` is a
        // correctly-sized, zeroed struct passed by pointer with its `cb`
        // field set to its own size, exactly as `K32GetProcessMemoryInfo`
        // requires.
        unsafe {
            let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            let process = GetCurrentProcess();
            if K32GetProcessMemoryInfo(process, &mut counters, counters.cb) != 0 {
                Some(counters.WorkingSetSize as u64)
            } else {
                None
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn current_rss_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    fn current_rss_bytes() -> Option<u64> {
        None
    }
}
