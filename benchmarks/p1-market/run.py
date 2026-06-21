from __future__ import annotations

import argparse
import asyncio
import json
import os
import statistics
import time
from pathlib import Path
from typing import Any, Callable


def load_corpus(path: Path, limit: int | None = None) -> list[dict[str, str]]:
    with path.open("r", encoding="utf-8") as f:
        rows = [json.loads(line) for line in f if line.strip()]
    return rows[:limit] if limit else rows


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, int(round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[index]


async def time_async(fn: Callable[[], Any]) -> float:
    start = time.perf_counter()
    result = fn()
    if hasattr(result, "__await__"):
        await result
    return (time.perf_counter() - start) * 1000.0


def summarize(name: str, values: list[float]) -> dict[str, float | str | int]:
    return {
        "operation": name,
        "count": len(values),
        "mean_ms": statistics.mean(values) if values else 0.0,
        "p50_ms": percentile(values, 50),
        "p95_ms": percentile(values, 95),
        "p99_ms": percentile(values, 99),
    }


def growth(values: list[float]) -> dict[str, float]:
    """First-quarter vs last-quarter mean — surfaces latency that grows with the
    collection size (Mem0's reconciliation prompt grows as memories accumulate)."""
    if len(values) < 8:
        return {"first_quartile_mean_ms": 0.0, "last_quartile_mean_ms": 0.0, "growth_ratio": 0.0}
    q = len(values) // 4
    first = statistics.mean(values[:q])
    last = statistics.mean(values[-q:])
    return {
        "first_quartile_mean_ms": first,
        "last_quartile_mean_ms": last,
        "growth_ratio": (last / first) if first else 0.0,
    }


# ── Checkpointing ──────────────────────────────────────────────────────────────
# A crash (segfault, Docker/Qdrant drop, OOM) must cost at most one item, not the
# whole run. We persist raw per-item latencies after every item; a --resume start
# reloads them and skips the work already done. For Mem0 + Qdrant this is correct
# because the Qdrant collection lives in a named volume and survives restarts.


def checkpoint_path(out: Path) -> Path:
    return out.with_suffix(out.suffix + ".ckpt")


def load_checkpoint(out: Path, resume: bool) -> dict[str, Any]:
    if not resume:
        return {}
    path = checkpoint_path(out)
    if path.exists():
        return json.loads(path.read_text(encoding="utf-8"))
    return {}


def save_checkpoint(out: Path, data: dict[str, Any]) -> None:
    path = checkpoint_path(out)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data), encoding="utf-8")
    # os.replace is atomic, but on Windows it intermittently raises
    # PermissionError (WinError 5) when an antivirus/indexer briefly holds a
    # handle on the target. Retry with backoff so a transient lock does not
    # crash a multi-minute run.
    for attempt in range(10):
        try:
            tmp.replace(path)
            return
        except PermissionError:
            if attempt == 9:
                raise
            time.sleep(0.1 * (attempt + 1))


async def run_basemyai(corpus: list[dict[str, str]], k: int, out: Path, resume: bool) -> dict[str, Any]:
    import basemyai

    db_path = os.environ.get("BASEMYAI_BENCH_DB", "basemyai-bench.bmai")
    agent = os.environ.get("BASEMYAI_BENCH_AGENT", "bench-agent")
    key = os.environ["BASEMYAI_BENCH_KEY"]
    model_dir = os.environ["BASEMYAI_BENCH_MODEL_DIR"]

    mem = await basemyai.Memory.open(
        db_path,
        agent,
        key,
        model_dir=model_dir,
        consent_to_fetch=False,
    )

    ckpt = load_checkpoint(out, resume)
    remember_ms: list[float] = ckpt.get("remember_raw_ms", [])
    recall_ms: list[float] = ckpt.get("recall_raw_ms", [])
    hybrid_ms: list[float] = ckpt.get("hybrid_raw_ms", [])

    def persist() -> None:
        save_checkpoint(out, {
            "remember_raw_ms": remember_ms,
            "recall_raw_ms": recall_ms,
            "hybrid_raw_ms": hybrid_ms,
        })

    for i in range(len(remember_ms), len(corpus)):
        item = corpus[i]
        remember_ms.append(await time_async(lambda item=item: mem.remember(item["text"], layer="semantic")))
        if (i + 1) % 25 == 0 or i + 1 == len(corpus):
            print(f"[basemyai] remember {i + 1}/{len(corpus)}", flush=True)
            persist()

    for i in range(len(recall_ms), len(corpus)):
        item = corpus[i]
        recall_ms.append(await time_async(lambda item=item: mem.recall(item["query"], k=k)))
        hybrid_ms.append(await time_async(lambda item=item: mem.recall_hybrid(item["query"], k=k)))
        if (i + 1) % 25 == 0 or i + 1 == len(corpus):
            print(f"[basemyai] recall {i + 1}/{len(corpus)}", flush=True)
            persist()

    size = Path(db_path).stat().st_size if Path(db_path).exists() else None
    return {
        "target": "basemyai",
        "database_bytes": size,
        "metrics": [
            summarize("remember", remember_ms),
            summarize("recall", recall_ms),
            summarize("recall_hybrid", hybrid_ms),
        ],
        "growth": {"remember": growth(remember_ms), "recall": growth(recall_ms)},
        "remember_raw_ms": remember_ms,
        "recall_raw_ms": recall_ms,
    }


async def run_mem0_qdrant(corpus: list[dict[str, str]], k: int, infer: bool, out: Path, resume: bool) -> dict[str, Any]:
    from qdrant_client import QdrantClient

    from mem0 import Memory

    agent = os.environ.get("MEM0_BENCH_USER", "bench-agent")
    collection = os.environ.get("MEM0_QDRANT_COLLECTION", "basemyai_p1_bench")
    # 127.0.0.1, not "localhost": on Windows + Docker Desktop, "localhost"
    # resolves to IPv6 ::1 first, fails, then falls back to IPv4 — a fixed
    # per-connection penalty (hundreds of ms to seconds) paid on every one of
    # mem0's several Qdrant round-trips per add. This single change takes an
    # add from ~25-67s down to ~4s. See README "Gotchas".
    host = os.environ.get("MEM0_QDRANT_HOST", "127.0.0.1")
    port = int(os.environ.get("MEM0_QDRANT_PORT", "6333"))
    # Default qdrant-client REST timeout is 5s. mem0's per-add comparison step
    # (embed + vector search against the growing collection) gets slower as the
    # collection grows, so a long run needs real headroom here.
    qdrant_timeout = float(os.environ.get("MEM0_QDRANT_TIMEOUT", "120"))

    llm_provider = os.environ.get("MEM0_LLM_PROVIDER", "ollama")
    llm_model = os.environ.get("MEM0_LLM_MODEL", "llama3.2:1b")
    embedder_provider = os.environ.get("MEM0_EMBEDDER_PROVIDER", "ollama")
    embedder_model = os.environ.get("MEM0_EMBEDDER_MODEL", "all-minilm")
    embedder_dims = int(os.environ.get("MEM0_EMBEDDER_DIMS", "384"))

    client = QdrantClient(host=host, port=port, timeout=qdrant_timeout)
    config = {
        "vector_store": {
            "provider": "qdrant",
            "config": {
                "collection_name": collection,
                "host": host,
                "port": port,
                "embedding_model_dims": embedder_dims,
                "client": client,
            },
        },
        "llm": {"provider": llm_provider, "config": {"model": llm_model}},
        "embedder": {
            "provider": embedder_provider,
            "config": {"model": embedder_model, "embedding_dims": embedder_dims},
        },
    }
    memory = Memory.from_config(config)
    target = "mem0_qdrant" if infer else "mem0_qdrant_noinfer"

    ckpt = load_checkpoint(out, resume)
    remember_ms: list[float] = ckpt.get("remember_raw_ms", [])
    recall_ms: list[float] = ckpt.get("recall_raw_ms", [])

    def persist() -> None:
        save_checkpoint(out, {"remember_raw_ms": remember_ms, "recall_raw_ms": recall_ms})

    if remember_ms:
        print(f"[mem0] resuming: {len(remember_ms)} remember, {len(recall_ms)} recall already done", flush=True)

    # remember (infer=True → 2 LLM calls/add; infer=False → embed+store only)
    for i in range(len(remember_ms), len(corpus)):
        item = corpus[i]
        remember_ms.append(
            await time_async(
                lambda item=item: memory.add(
                    item["text"], user_id=agent, metadata={"id": item["id"]}, infer=infer
                )
            )
        )
        persist()  # every item: a 25s add must never be redone
        if (i + 1) % 5 == 0 or i + 1 == len(corpus):
            print(f"[mem0] remember {i + 1}/{len(corpus)}  last={remember_ms[-1]:.0f}ms", flush=True)

    # recall
    for i in range(len(recall_ms), len(corpus)):
        item = corpus[i]
        recall_ms.append(
            await time_async(lambda item=item: memory.search(item["query"], filters={"user_id": agent}, top_k=k))
        )
        persist()
        if (i + 1) % 25 == 0 or i + 1 == len(corpus):
            print(f"[mem0] recall {i + 1}/{len(corpus)}", flush=True)

    return {
        "target": target,
        "database_bytes": None,
        "infer": infer,
        "metrics": [
            summarize("remember", remember_ms),
            summarize("recall", recall_ms),
        ],
        "growth": {"remember": growth(remember_ms), "recall": growth(recall_ms)},
        "remember_raw_ms": remember_ms,
        "recall_raw_ms": recall_ms,
    }


async def main() -> None:
    parser = argparse.ArgumentParser(description="P1 market benchmark harness.")
    parser.add_argument("--target", choices=["basemyai", "mem0_qdrant"], required=True)
    parser.add_argument("--corpus", type=Path, default=Path("corpus.jsonl"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("-k", type=int, default=5)
    parser.add_argument("--limit", type=int, default=None, help="cap corpus to first N items")
    parser.add_argument("--resume", action="store_true", help="resume from checkpoint, skip done items")
    infer_group = parser.add_mutually_exclusive_group()
    infer_group.add_argument("--infer", dest="infer", action="store_true", help="Mem0: run LLM fact extraction (default)")
    infer_group.add_argument("--no-infer", dest="infer", action="store_false", help="Mem0: store raw, skip all LLM calls")
    parser.set_defaults(infer=True)
    args = parser.parse_args()

    corpus = load_corpus(args.corpus, args.limit)
    if args.target == "basemyai":
        result = await run_basemyai(corpus, args.k, args.out, args.resume)
    else:
        result = await run_mem0_qdrant(corpus, args.k, args.infer, args.out, args.resume)

    result["corpus_count"] = len(corpus)
    result["k"] = args.k
    result["created_at_unix"] = int(time.time())
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result, indent=2), encoding="utf-8")
    # success → drop the checkpoint so a later run starts clean unless --resume
    ckpt = checkpoint_path(args.out)
    if ckpt.exists():
        ckpt.unlink()
    print(json.dumps(result["metrics"], indent=2))


if __name__ == "__main__":
    asyncio.run(main())
