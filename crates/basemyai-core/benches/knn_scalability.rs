use std::fmt::Write as _;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use basemyai_core::{Store, libsql};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tokio::runtime::Runtime;

const DIM: usize = 384;
const TABLE: &str = "bench_emb";
const DEFAULT_SIZES: &str = "10000";
const INSERT_CHUNK: usize = 1_000;

fn knn_scalability(c: &mut Criterion) {
    let runtime = Runtime::new().expect("tokio runtime for async libSQL bench");
    let mut group = c.benchmark_group("knn_scalability");
    group.sample_size(10);

    for rows in bench_sizes() {
        group.throughput(Throughput::Elements(u64::try_from(rows).expect("rows fit in u64")));
        let (store, path) = runtime.block_on(seed_store(rows)).expect("seed synthetic vector store");
        let query = synthetic_vector(rows / 2);

        group.bench_with_input(BenchmarkId::new("vector_knn_cosine_k10", rows), &rows, |b, _| {
            b.iter(|| {
                let neighbors = runtime
                    .block_on(store.vector_knn(TABLE, black_box(&query), black_box(10), None))
                    .expect("KNN query succeeds");
                black_box(neighbors);
            });
        });

        drop(store);
        cleanup(&path);
    }

    group.finish();
}

fn bench_sizes() -> Vec<usize> {
    let raw = std::env::var("BASEMYAI_KNN_BENCH_SIZES").unwrap_or_else(|_| DEFAULT_SIZES.to_string());
    let sizes: Vec<usize> = raw
        .split(',')
        .filter_map(|part| part.trim().parse::<usize>().ok())
        .filter(|size| *size > 0)
        .collect();
    if sizes.is_empty() { vec![10_000] } else { sizes }
}

async fn seed_store(rows: usize) -> basemyai_core::Result<(Store, PathBuf)> {
    let path = temp_db_path(rows);
    cleanup(&path);

    let store = Store::open_with(&path, None, 4).await?;
    store.ensure_vector_table(TABLE, DIM).await?;

    let mut inserted = 0;
    while inserted < rows {
        let upper = rows.min(inserted + INSERT_CHUNK);
        let txn = store.begin_write().await?;
        for i in inserted..upper {
            txn.execute(
                &format!(
                    "INSERT INTO {TABLE} (id, emb) VALUES (?1, vector(?2)) \
                     ON CONFLICT(id) DO UPDATE SET emb = vector(?2)"
                ),
                libsql::params![format!("v-{i:08}"), vector_literal(&synthetic_vector(i))],
            )
            .await
            .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        }
        txn.commit().await?;
        inserted = upper;
    }

    Ok((store, path))
}

fn temp_db_path(rows: usize) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("basemyai-knn-bench-{}-{rows}-{now}.db", std::process::id()))
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

#[allow(clippy::cast_precision_loss)]
fn synthetic_vector(seed: usize) -> Vec<f32> {
    let mut state = (seed as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut vector = Vec::with_capacity(DIM);
    for _ in 0..DIM {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let unit = ((state >> 40) as u32) as f32 / ((1_u32 << 24) as f32);
        vector.push((unit * 2.0) - 1.0);
    }
    normalize(&mut vector);
    vector
}

fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in vector {
            *x /= norm;
        }
    }
}

fn vector_literal(vector: &[f32]) -> String {
    let mut out = String::with_capacity((vector.len() * 10) + 2);
    out.push('[');
    for (i, value) in vector.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write!(&mut out, "{value:.8}").expect("writing to String cannot fail");
    }
    out.push(']');
    out
}

criterion_group!(benches, knn_scalability);
criterion_main!(benches);
