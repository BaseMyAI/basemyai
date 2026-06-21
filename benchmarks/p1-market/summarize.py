from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any


def main() -> None:
    paths = [Path(p) for p in sys.argv[1:]]
    if not paths:
        raise SystemExit("usage: python summarize.py out/*.json")

    rows: list[dict[str, Any]] = []
    for path in paths:
        data = json.loads(path.read_text(encoding="utf-8"))
        for metric in data["metrics"]:
            rows.append({"target": data["target"], **metric})

    print("| Target | Operation | Count | Mean ms | p50 ms | p95 ms | p99 ms |")
    print("|---|---:|---:|---:|---:|---:|---:|")
    for row in rows:
        print(
            f"| {row['target']} | {row['operation']} | {row['count']} | "
            f"{row['mean_ms']:.2f} | {row['p50_ms']:.2f} | "
            f"{row['p95_ms']:.2f} | {row['p99_ms']:.2f} |"
        )


if __name__ == "__main__":
    main()
