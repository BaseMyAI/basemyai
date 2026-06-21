#!/usr/bin/env bash
# Crash-proof P1 benchmark runner.
#
# Every mem0 run can die mid-flight (segfault in a native dep, Docker/Qdrant
# drop, OOM). run.py checkpoints every item; this supervisor re-invokes it with
# --resume until it exits 0, so a crash costs one item, never the whole run.
#
# Three measurements:
#   1. basemyai            full 500 (it is fast: ~0.3s/op)
#   2. mem0 infer=True     N=100, sequential — real Mem0 latency + growth curve
#   3. mem0 infer=False    N=500 — storage-only, isolates LLM-orchestration cost
set -uo pipefail
cd "$(dirname "$0")"

PY=.venv/Scripts/python.exe
QDRANT=http://127.0.0.1:6333  # NOT localhost — IPv6 fallback penalty on Windows

export BASEMYAI_BENCH_DB="./basemyai-bench.bmai"
export BASEMYAI_BENCH_AGENT="bench-agent"
export BASEMYAI_BENCH_MODEL_DIR="${BASEMYAI_BENCH_MODEL_DIR:-C:/models/all-MiniLM-L6-v2}"
export BASEMYAI_BENCH_KEY="dev-benchmark-key"

wait_qdrant() {
  echo "waiting for Qdrant at $QDRANT ..."
  for _ in $(seq 1 60); do
    curl -sf "$QDRANT/healthz" >/dev/null 2>&1 && { echo "Qdrant up."; return 0; }
    sleep 2
  done
  echo "Qdrant did not come up — is Docker Desktop running?" >&2
  return 1
}

# supervise <out> -- <run.py args...>   : retry with --resume until exit 0
supervise() {
  local out="$1"; shift; shift  # drop "--"
  local attempt=1
  while true; do
    local flag=""
    [ -f "${out}.ckpt" ] && flag="--resume"
    echo ">>> attempt $attempt: $PY run.py $* --out $out $flag"
    PYTHONFAULTHANDLER=1 "$PY" run.py "$@" --out "$out" $flag && return 0
    echo "!!! crashed (exit $?). resuming in 3s (attempt $((++attempt)))..."
    sleep 3
    wait_qdrant || return 1
  done
}

reset_collection() { curl -s -X DELETE "$QDRANT/collections/$1" >/dev/null 2>&1; }

wait_qdrant || exit 1

# 1. BaseMyAI — fast, full corpus (fresh DB unless resuming a partial)
[ -f out/basemyai.json.ckpt ] || rm -f "$BASEMYAI_BENCH_DB"
supervise out/basemyai.json -- --target basemyai --corpus corpus.jsonl

# 2. Mem0 infer=True — latency + growth, N=100 (fresh collection unless resuming)
export MEM0_QDRANT_COLLECTION="basemyai_p1_bench"
[ -f out/mem0-qdrant.json.ckpt ] || reset_collection "$MEM0_QDRANT_COLLECTION"
supervise out/mem0-qdrant.json -- --target mem0_qdrant --corpus corpus.jsonl --limit 100 --infer

# 3. Mem0 infer=False — storage-only, N=500 (separate collection)
export MEM0_QDRANT_COLLECTION="basemyai_p1_bench_noinfer"
[ -f out/mem0-noinfer.json.ckpt ] || reset_collection "$MEM0_QDRANT_COLLECTION"
supervise out/mem0-noinfer.json -- --target mem0_qdrant --corpus corpus.jsonl --limit 500 --no-infer

echo "=== summary ==="
"$PY" summarize.py out/basemyai.json out/mem0-qdrant.json out/mem0-noinfer.json | tee out/summary.md
