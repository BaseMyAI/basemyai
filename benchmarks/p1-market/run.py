from __future__ import annotations

import argparse
import asyncio
import json
import os
import statistics
import time
from pathlib import Path
from typing import Any, Callable


def load_corpus(path: Path) -> list[dict[str, str]]:
    with path.open("r", encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


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


async def run_basemyai(corpus: list[dict[str, str]], k: int) -> dict[str, Any]:
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

    remember_ms: list[float] = []
    recall_ms: list[float] = []
    hybrid_ms: list[float] = []

    for item in corpus:
        remember_ms.append(await time_async(lambda item=item: mem.remember(item["text"], layer="semantic")))

    for item in corpus:
        recall_ms.append(await time_async(lambda item=item: mem.recall(item["query"], k=k)))
        hybrid_ms.append(await time_async(lambda item=item: mem.recall_hybrid(item["query"], k=k)))

    size = Path(db_path).stat().st_size if Path(db_path).exists() else None
    return {
        "target": "basemyai",
        "database_bytes": size,
        "metrics": [
            summarize("remember", remember_ms),
            summarize("recall", recall_ms),
            summarize("recall_hybrid", hybrid_ms),
        ],
    }


async def run_mem0_qdrant(corpus: list[dict[str, str]], k: int, partial_out: Path | None = None) -> dict[str, Any]:
    from qdrant_client import QdrantClient

    from mem0 import Memory

    agent = os.environ.get("MEM0_BENCH_USER", "bench-agent")
    collection = os.environ.get("MEM0_QDRANT_COLLECTION", "basemyai_p1_bench")
    host = os.environ.get("MEM0_QDRANT_HOST", "localhost")
    port = int(os.environ.get("MEM0_QDRANT_PORT", "6333"))
    # Default qdrant-client REST timeout is 5s. mem0's per-add comparison step
    # (embed + vector search against the growing collection) gets slower as the
    # collection grows, so a long 500-item run needs real headroom here.
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

    def save_partial(stage: str) -> None:
        if partial_out is None:
            return
        partial = {
            "target": "mem0_qdrant",
            "database_bytes": None,
            "partial_stage": stage,
            "metrics": [
                summarize("remember", remember_ms),
                summarize("recall", recall_ms),
            ],
        }
        partial_out.parent.mkdir(parents=True, exist_ok=True)
        partial_out.write_text(json.dumps(partial, indent=2), encoding="utf-8")

    remember_ms: list[float] = []
    recall_ms: list[float] = []

    for i, item in enumerate(corpus):
        remember_ms.append(
            await time_async(lambda item=item: memory.add(item["text"], user_id=agent, metadata={"id": item["id"]}))
        )
        if (i + 1) % 25 == 0 or i + 1 == len(corpus):
            print(f"[mem0] remember {i + 1}/{len(corpus)}", flush=True)
            save_partial("remember")

    for i, item in enumerate(corpus):
        recall_ms.append(
            await time_async(lambda item=item: memory.search(item["query"], filters={"user_id": agent}, top_k=k))
        )
        if (i + 1) % 25 == 0 or i + 1 == len(corpus):
            print(f"[mem0] recall {i + 1}/{len(corpus)}", flush=True)
            save_partial("recall")

    return {
        "target": "mem0_qdrant",
        "database_bytes": None,
        "metrics": [
            summarize("remember", remember_ms),
            summarize("recall", recall_ms),
        ],
    }


async def main() -> None:
    parser = argparse.ArgumentParser(description="P1 market benchmark harness.")
    parser.add_argument("--target", choices=["basemyai", "mem0_qdrant"], required=True)
    parser.add_argument("--corpus", type=Path, default=Path("corpus.jsonl"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("-k", type=int, default=5)
    args = parser.parse_args()

    corpus = load_corpus(args.corpus)
    if args.target == "basemyai":
        result = await run_basemyai(corpus, args.k)
    else:
        result = await run_mem0_qdrant(corpus, args.k, partial_out=args.out)

    result["corpus_count"] = len(corpus)
    result["k"] = args.k
    result["created_at_unix"] = int(time.time())
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    asyncio.run(main())
