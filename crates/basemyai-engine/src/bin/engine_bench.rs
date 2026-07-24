// SPDX-License-Identifier: BUSL-1.1
//! `engine_bench` — canonical, reproducible engine workloads (N7.2,
//! `docs/PLAN-NATIVE-ENGINE.md` §4.2), emitting a machine-archivable JSON
//! report. Manual tool + `cargo xtask engine-bench`; never part of the CI
//! gate (numbers, not pass/fail).
//!
//! ```text
//! cargo run --release -p basemyai-engine --features test-util --bin engine_bench -- \
//!     [all|kv|memory] [--n N] [--memory-n N] [--encrypted] [--out report.json]
//! ```
//!
//! Workload groups:
//! - `kv` — kv-fill, kv-point-read, kv-prefix-scan, mixed-read-write,
//!   delete-churn, flush-compaction, open-large-store, on one store;
//! - `memory` — memory-remember, memory-recall (384d latent vectors + FTS),
//!   on a second store;
//! - `all` — both groups (default).
//!
//! The plan's `encrypted-vs-clear` workload is realized by running the same
//! invocation twice (with and without `--encrypted`) and diffing the two
//! reports — `cargo xtask engine-bench` does exactly that.
//!
//! Determinism: every key, value, vector and access pattern derives from
//! seeded xorshift64* streams — two runs at the same `n` exercise the exact
//! same operations. Latency numbers obviously still vary with the machine;
//! the JSON meta block records enough context to compare runs honestly.
//!
//! The RNG/dataset generators and the RSS sampler are duplicated from
//! `vector_bench` (itself duplicated from `tests/common`) — a `src/bin`
//! target can't import either, and the three copies are each documented as
//! such.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use basemyai_engine::{
    Engine, EngineOptions, EngineStats, NewMemoryRecord, PersistentFts, PersistentMemoryIndex, PersistentVectorIndex,
    VectorIndexParams, VerifyMode, verify_store,
};

const VECTOR_DIM: usize = 384;
const LATENT_DIM: usize = 16;
const BENCH_KEY: &[u8] = b"engine-bench encryption key (not a secret)";
const AGENT: &str = "bench-agent";
/// Keys per prefix bucket — makes `kv-prefix-scan` a bounded range, not a
/// full-store scan.
const BUCKET: u64 = 1_000;

fn main() {
    let args = Args::parse();
    let mut report = Report::new(&args);

    if args.group != Group::Memory {
        run_kv_group(&args, &mut report);
    }
    if args.group != Group::Kv {
        run_memory_group(&args, &mut report);
    }

    let json = report.finish();
    match &args.out {
        Some(path) => {
            std::fs::write(path, &json).unwrap_or_else(|e| fatal(&format!("write {}: {e}", path.display())));
            eprintln!("[engine_bench] report written to {}", path.display());
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(json.as_bytes())
                .unwrap_or_else(|e| fatal(&format!("stdout: {e}")));
        }
    }
}

// ── workloads ────────────────────────────────────────────────────────────────

fn run_kv_group(args: &Args, report: &mut Report) {
    let dir = tempdir(args, "kv");
    let mut engine = open_engine(&dir, args);
    let n = args.n;
    let mut value_rng = XorShift64::new(0xBEEF);
    let value = |rng: &mut XorShift64| -> Vec<u8> { (0..100).map(|_| (rng.next_u64() & 0xFF) as u8).collect() };

    // kv-fill — sequential inserts, the raw single-record write path (one
    // WAL fsync per put — group commit is N13's business, measured here as
    // the honest current cost).
    let mut lat = Latencies::with_capacity(n as usize);
    for i in 0..n {
        let v = value(&mut value_rng);
        lat.time(|| engine.put(&kv_key(i), &v).unwrap_or_else(die));
    }
    report.push(Workload::from_run("kv-fill", lat, &engine, &[]));

    // kv-point-read — uniform random point lookups over the filled store.
    let reads = n.min(10_000);
    let mut rng = XorShift64::new(0xF00D);
    let mut lat = Latencies::with_capacity(reads as usize);
    let mut hits = 0u64;
    for _ in 0..reads {
        let key = kv_key(rng.next_u64() % n);
        lat.time(|| {
            if engine.get(&key).unwrap_or_else(die).is_some() {
                hits += 1;
            }
        });
    }
    assert_eq!(hits, reads, "every sampled key was inserted by kv-fill");
    report.push(Workload::from_run("kv-point-read", lat, &engine, &[]));

    // kv-prefix-scan — bounded bucket scans (BUCKET keys each).
    let scans = 50.min(n / BUCKET).max(1);
    let mut rng = XorShift64::new(0xCAFE);
    let mut lat = Latencies::with_capacity(scans as usize);
    let mut returned = 0u64;
    for _ in 0..scans {
        let bucket = rng.next_u64() % n.div_ceil(BUCKET);
        let prefix = format!("kv/{bucket:06}/");
        lat.time(|| returned += engine.scan_prefix(prefix.as_bytes()).unwrap_or_else(die).len() as u64);
    }
    report.push(Workload::from_run(
        "kv-prefix-scan",
        lat,
        &engine,
        &[("rows_returned", Json::U64(returned))],
    ));

    // mixed-read-write — 4 reads per write, interleaved.
    let ops = n.min(20_000);
    let mut rng = XorShift64::new(0xD1CE);
    let mut lat = Latencies::with_capacity(ops as usize);
    for i in 0..ops {
        if i % 5 == 4 {
            let key = kv_key(rng.next_u64() % n);
            let v = value(&mut value_rng);
            lat.time(|| engine.put(&key, &v).unwrap_or_else(die));
        } else {
            let key = kv_key(rng.next_u64() % n);
            lat.time(|| {
                let _ = engine.get(&key).unwrap_or_else(die);
            });
        }
    }
    report.push(Workload::from_run("mixed-read-write", lat, &engine, &[]));

    // delete-churn — delete 20% (bounded), re-insert, three cycles.
    let churn = (n / 5).clamp(1, 20_000);
    let mut rng = XorShift64::new(0xDEAD);
    let mut lat = Latencies::with_capacity((churn * 2 * 3) as usize);
    for _ in 0..3 {
        let base = rng.next_u64() % n;
        for i in 0..churn {
            let key = kv_key((base + i) % n);
            lat.time(|| engine.delete(&key).unwrap_or_else(die));
        }
        for i in 0..churn {
            let key = kv_key((base + i) % n);
            let v = value(&mut value_rng);
            lat.time(|| engine.put(&key, &v).unwrap_or_else(die));
        }
    }
    report.push(Workload::from_run(
        "delete-churn",
        lat,
        &engine,
        &[("churned_keys_per_cycle", Json::U64(churn))],
    ));

    // flush-compaction — explicit flush latency under small refills; the
    // engine's own counters say how many compactions the cycle triggered.
    let stats_before = engine.stats().unwrap_or_else(die);
    let mut lat = Latencies::with_capacity(32);
    let mut rng = XorShift64::new(0xF1A5);
    for cycle in 0..32u64 {
        for i in 0..64u64 {
            let key = format!("fc/{cycle:04}/{i:04}");
            let v = value(&mut value_rng);
            engine.put(key.as_bytes(), &v).unwrap_or_else(die);
        }
        let _ = rng.next_u64();
        lat.time(|| engine.flush().unwrap_or_else(die));
    }
    let stats_after = engine.stats().unwrap_or_else(die);
    report.push(Workload::from_run(
        "flush-compaction",
        lat,
        &engine,
        &[(
            "compactions_triggered",
            Json::U64(stats_after.compaction_count - stats_before.compaction_count),
        )],
    ));

    // open-large-store — close, then time a cold reopen of everything the
    // run produced (whole-SST loads: THE number N8 must shrink).
    engine.close().unwrap_or_else(die);
    let rss_before = ram::current_rss_bytes().unwrap_or(0);
    let started = Instant::now();
    let engine = open_engine(&dir, args);
    let open_secs = started.elapsed().as_secs_f64();
    let rss_after = ram::current_rss_bytes().unwrap_or(0);
    let stats = engine.stats().unwrap_or_else(die);
    let mut lat = Latencies::with_capacity(1);
    lat.push(Duration::from_secs_f64(open_secs));
    report.push(Workload::from_run(
        "open-large-store",
        lat,
        &engine,
        &[
            ("open_bytes_read", Json::U64(stats.bytes_read)),
            ("rss_delta_bytes", Json::U64(rss_after.saturating_sub(rss_before))),
        ],
    ));
    drop(engine);
    verify_dir_if_requested(args, &dir, "kv");
    let _ = std::fs::remove_dir_all(&dir);
}

fn run_memory_group(args: &Args, report: &mut Report) {
    let dir = tempdir(args, "memory");
    let mut engine = open_engine(&dir, args);
    let mut vectors =
        PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(VECTOR_DIM)).unwrap_or_else(die);
    let fts = PersistentFts::new();
    let mut memory = PersistentMemoryIndex::open(&engine).unwrap_or_else(die);
    let n = args.memory_n;
    let mut data = LatentData::new(42, VECTOR_DIM);

    // memory-remember — the full composed write: record + vecmap + DiskANN
    // node/neighbors + FTS postings, one WAL record per put (ADR-027 §3).
    let mut lat = Latencies::with_capacity(n as usize);
    for i in 0..n {
        let id = format!("mem-{i:08}");
        let content = format!("memory record {i} about topic-{} with token{}", i % 97, i % 1013);
        let new = NewMemoryRecord {
            layer: "semantic",
            content: &content,
            source: "bench",
            valid_from: 1_700_000_000 + i as i64,
            valid_until: None,
            importance: 1.0,
            last_access: 1_700_000_000 + i as i64,
        };
        let vector = data.point();
        lat.time(|| {
            memory
                .put(&mut engine, &mut vectors, &fts, AGENT, &id, &new, vector.clone())
                .unwrap_or_else(die);
        });
    }
    report.push(Workload::from_run("memory-remember", lat, &engine, &[]));

    // memory-recall — ANN top-10 over the same distribution. Vector search
    // only (hydration/temporal filtering live in `basemyai`, benched there
    // by `native_memory_store_bench`).
    let queries = 200.min(n);
    let mut lat = Latencies::with_capacity(queries as usize);
    let mut found = 0u64;
    for _ in 0..queries {
        let q = data.point();
        lat.time(|| found += vectors.search(&engine, &q, 10).unwrap_or_else(die).len() as u64);
    }
    report.push(Workload::from_run(
        "memory-recall",
        lat,
        &engine,
        &[
            ("k", Json::U64(10)),
            ("hits_returned", Json::U64(found)),
            ("vector_search_only", Json::Bool(true)),
        ],
    ));
    drop(engine);
    verify_dir_if_requested(args, &dir, "memory");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--verify` (N11): re-open the just-closed store read-only and run the
/// engine's own `verify_store` in `FullLogical` mode (the deepest audit,
/// ADR-040 §2) before the temp dir is wiped — the exit-gate criterion of
/// `docs/PLAN-NATIVE-ENGINE.md` §8 ("`verify --deep` vert après chaque
/// scénario non destructif") exercised against the actual bench-produced
/// store, not a synthetic stand-in. Fatal on an unhealthy report: a soak
/// run that silently produced a corrupt store must not report success.
fn verify_dir_if_requested(args: &Args, dir: &Path, label: &str) {
    if !args.verify {
        return;
    }
    let key = args.encrypted.then_some(BENCH_KEY);
    let report = verify_store(dir, key, VerifyMode::FullLogical)
        .unwrap_or_else(|e| fatal(&format!("verify_store({label}) failed: {e}")));
    eprintln!(
        "[engine_bench] verify({label}, FullLogical): healthy={} files={} blocks={} records={} errors={} warnings={}",
        report.healthy,
        report.files_checked,
        report.blocks_checked,
        report.records_checked,
        report.errors.len(),
        report.warnings.len(),
    );
    for w in &report.warnings {
        eprintln!("[engine_bench]   warning: {w}");
    }
    if !report.healthy {
        for e in &report.errors {
            eprintln!("[engine_bench]   ERROR: {e}");
        }
        fatal(&format!("verify_store({label}) reported unhealthy store"));
    }
}

// ── plumbing ─────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Group {
    All,
    Kv,
    Memory,
}

struct Args {
    group: Group,
    n: u64,
    memory_n: u64,
    encrypted: bool,
    out: Option<PathBuf>,
    keep_dir: Option<PathBuf>,
    verify: bool,
}

impl Args {
    fn parse() -> Self {
        let mut group = Group::All;
        let mut n = 10_000u64;
        let mut memory_n = 0u64;
        let mut encrypted = false;
        let mut out = None;
        let mut keep_dir = None;
        let mut verify = false;
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "all" => group = Group::All,
                "kv" => group = Group::Kv,
                "memory" => group = Group::Memory,
                "--n" => n = next_u64(&mut it, "--n"),
                "--memory-n" => memory_n = next_u64(&mut it, "--memory-n"),
                "--encrypted" => encrypted = true,
                "--out" => out = Some(PathBuf::from(it.next().unwrap_or_else(|| fatal("--out needs a path")))),
                "--dir" => keep_dir = Some(PathBuf::from(it.next().unwrap_or_else(|| fatal("--dir needs a path")))),
                "--verify" => verify = true,
                other => fatal(&format!(
                    "unknown arg {other:?}\nusage: engine_bench [all|kv|memory] [--n N] [--memory-n N] [--encrypted] [--out report.json] [--dir workdir] [--verify]"
                )),
            }
        }
        if memory_n == 0 {
            // The composed memory write path costs ~ms/op (DiskANN insert);
            // capping it keeps `all --n 100000` tractable while the KV
            // workloads still run at full n. Override with --memory-n.
            memory_n = n.min(10_000);
        }
        Self {
            group,
            n,
            memory_n,
            encrypted,
            out,
            keep_dir,
            verify,
        }
    }
}

fn next_u64(it: &mut impl Iterator<Item = String>, flag: &str) -> u64 {
    it.next()
        .and_then(|v| v.replace('_', "").parse().ok())
        .unwrap_or_else(|| fatal(&format!("{flag} needs an integer")))
}

fn tempdir(args: &Args, group: &str) -> PathBuf {
    let base = args
        .keep_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("engine-bench-{group}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    base
}

fn open_engine(dir: &Path, args: &Args) -> Engine {
    // Defaults on purpose: the baseline measures the engine as shipped.
    let options = EngineOptions::default();
    let result = if args.encrypted {
        Engine::open_encrypted_with_options(dir, BENCH_KEY, options)
    } else {
        Engine::open_with_options(dir, options)
    };
    result.unwrap_or_else(die)
}

fn kv_key(i: u64) -> Vec<u8> {
    format!("kv/{:06}/{:06}", i / BUCKET, i % BUCKET).into_bytes()
}

fn die<T>(err: basemyai_engine::EngineError) -> T {
    fatal(&format!("engine error: {err}"))
}

fn fatal(msg: &str) -> ! {
    eprintln!("[engine_bench] {msg}");
    std::process::exit(1);
}

struct Latencies {
    nanos: Vec<u64>,
}

impl Latencies {
    fn with_capacity(cap: usize) -> Self {
        Self {
            nanos: Vec::with_capacity(cap),
        }
    }

    fn time(&mut self, mut op: impl FnMut()) {
        let started = Instant::now();
        op();
        self.push(started.elapsed());
    }

    fn push(&mut self, d: Duration) {
        self.nanos.push(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    }

    fn percentile_ms(sorted: &[u64], q: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
        sorted[idx] as f64 / 1e6
    }
}

struct Workload {
    name: &'static str,
    ops: u64,
    total_secs: f64,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    rss_after_bytes: u64,
    stats: EngineStats,
    extra: Vec<(&'static str, Json)>,
}

impl Workload {
    fn from_run(name: &'static str, lat: Latencies, engine: &Engine, extra: &[(&'static str, Json)]) -> Self {
        let mut sorted = lat.nanos;
        sorted.sort_unstable();
        let total_nanos: u64 = sorted.iter().sum();
        let ops = sorted.len() as u64;
        eprintln!("[engine_bench] {name}: {ops} ops in {:.2}s", total_nanos as f64 / 1e9);
        Self {
            name,
            ops,
            total_secs: total_nanos as f64 / 1e9,
            mean_ms: if ops == 0 {
                0.0
            } else {
                total_nanos as f64 / ops as f64 / 1e6
            },
            p50_ms: Latencies::percentile_ms(&sorted, 0.50),
            p95_ms: Latencies::percentile_ms(&sorted, 0.95),
            p99_ms: Latencies::percentile_ms(&sorted, 0.99),
            rss_after_bytes: ram::current_rss_bytes().unwrap_or(0),
            stats: engine.stats().unwrap_or_else(die),
            extra: extra.to_vec(),
        }
    }
}

struct Report {
    meta: Vec<(&'static str, Json)>,
    workloads: Vec<Workload>,
    sampler: ram::Sampler,
}

impl Report {
    fn new(args: &Args) -> Self {
        let meta = vec![
            ("schema", Json::Str("basemyai-engine-bench/1".into())),
            ("commit", Json::Str(git_commit())),
            ("unix_time", Json::U64(unix_time())),
            ("os", Json::Str(std::env::consts::OS.into())),
            ("arch", Json::Str(std::env::consts::ARCH.into())),
            ("cpu", Json::Str(cpu_model())),
            ("encrypted", Json::Bool(args.encrypted)),
            ("n", Json::U64(args.n)),
            ("memory_n", Json::U64(args.memory_n)),
            (
                "memtable_flush_threshold",
                Json::U64(EngineOptions::default().memtable_flush_threshold as u64),
            ),
            (
                "compaction_sst_threshold",
                Json::U64(EngineOptions::default().compaction_sst_threshold as u64),
            ),
        ];
        Self {
            meta,
            workloads: Vec::new(),
            sampler: ram::Sampler::start(Duration::from_millis(50)),
        }
    }

    fn push(&mut self, w: Workload) {
        self.workloads.push(w);
    }

    fn finish(mut self) -> String {
        let samples = self.sampler.stop_and_collect();
        let peak = samples.iter().map(|&(_, rss)| rss).max().unwrap_or(0);
        let mean = if samples.is_empty() {
            0
        } else {
            samples.iter().map(|&(_, rss)| rss).sum::<u64>() / samples.len() as u64
        };
        self.meta.push(("rss_peak_bytes", Json::U64(peak)));
        self.meta.push(("rss_mean_bytes", Json::U64(mean)));

        let mut out = String::new();
        out.push_str("{\n  \"meta\": {");
        for (i, (k, v)) in self.meta.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("\n    \"{k}\": {}", v.render()));
        }
        out.push_str("\n  },\n  \"workloads\": [");
        for (i, w) in self.workloads.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "\n    {{\n      \"name\": \"{}\", \"ops\": {}, \"total_secs\": {:.4},\n      \"mean_ms\": {:.4}, \"p50_ms\": {:.4}, \"p95_ms\": {:.4}, \"p99_ms\": {:.4},\n      \"throughput_ops_per_sec\": {:.1}, \"rss_after_bytes\": {},",
                w.name,
                w.ops,
                w.total_secs,
                w.mean_ms,
                w.p50_ms,
                w.p95_ms,
                w.p99_ms,
                if w.total_secs > 0.0 { w.ops as f64 / w.total_secs } else { 0.0 },
                w.rss_after_bytes,
            ));
            for (k, v) in &w.extra {
                out.push_str(&format!("\n      \"{k}\": {},", v.render()));
            }
            let s = &w.stats;
            out.push_str(&format!(
                "\n      \"engine_stats\": {{ \"wal_bytes\": {}, \"wal_records\": {}, \"memtable_bytes\": {}, \"sst_count\": {}, \"sst_bytes\": {}, \"tombstone_count\": {}, \"flush_count\": {}, \"compaction_count\": {}, \"compaction_input_bytes\": {}, \"compaction_output_bytes\": {}, \"bytes_read\": {}, \"bytes_written\": {}, \"block_cache_hits\": {}, \"block_cache_misses\": {}, \"point_lookup_full_sst_read\": {} }}\n    }}",
                s.wal_bytes,
                s.wal_records,
                s.memtable_bytes,
                s.sst_count,
                s.sst_bytes,
                s.tombstone_count,
                s.flush_count,
                s.compaction_count,
                s.compaction_input_bytes,
                s.compaction_output_bytes,
                s.bytes_read,
                s.bytes_written,
                s.block_cache_hits,
                s.block_cache_misses,
                s.point_lookup_full_sst_read,
            ));
        }
        out.push_str("\n  ]\n}\n");
        out
    }
}

#[derive(Clone)]
enum Json {
    Str(String),
    U64(u64),
    Bool(bool),
}

impl Json {
    fn render(&self) -> String {
        match self {
            // Values here are all internally produced (no user input);
            // escape the two characters that could still break framing.
            Json::Str(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            Json::U64(v) => v.to_string(),
            Json::Bool(b) => b.to_string(),
        }
    }
}

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Best-effort `git rev-parse --short HEAD`; "unknown" offline (this is a
/// subprocess of a local dev command, not engine code — the engine itself
/// stays zero-network/zero-subprocess).
fn git_commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn cpu_model() -> String {
    #[cfg(target_os = "windows")]
    {
        if let Ok(id) = std::env::var("PROCESSOR_IDENTIFIER") {
            return id;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
            for line in cpuinfo.lines() {
                if let Some(rest) = line.strip_prefix("model name") {
                    return rest.trim_start_matches([' ', '\t', ':']).to_string();
                }
            }
        }
    }
    "unknown".into()
}

/// Deterministic xorshift64* PRNG (documented duplicate of
/// `tests/common/mod.rs`, not importable from a `src/bin` target).
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

/// Seeded low-intrinsic-dimension dataset generator (documented duplicate of
/// `tests/common::LatentData` — models MiniLM-style embeddings; see
/// `tests/vector_recall.rs` for why iid-uniform 384d would be an ANN
/// pathology, not a benchmark).
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

/// In-process RSS sampling — documented duplicate of `vector_bench`'s `ram`
/// module (same `src/bin` import constraint), same whole-process-RSS
/// trade-off stated there.
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
    pub(crate) fn current_rss_bytes() -> Option<u64> {
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
    pub(crate) fn current_rss_bytes() -> Option<u64> {
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
    pub(crate) fn current_rss_bytes() -> Option<u64> {
        None
    }
}
