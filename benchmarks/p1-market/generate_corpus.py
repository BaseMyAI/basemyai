"""Generates a larger, reproducible corpus.jsonl for the P1 benchmark.

Replaces the 10-item smoke corpus with ~500 items so p95/p99 latency figures
are statistically meaningful (see docs/benchmarks/local-memory-vs-mem0-qdrant.md).
Deterministic (fixed seed): re-running this script produces the same file.
"""

from __future__ import annotations

import json
import random
from pathlib import Path

SEED = 42

NAMES = [
    "Alice", "Bob", "Chen", "Diego", "Elena", "Farah", "Gus", "Hana", "Ivan", "Jules",
    "Kira", "Liam", "Mara", "Noor", "Omar", "Priya", "Quinn", "Rosa", "Sami", "Tao",
]

PROJECTS = [
    "a Rust memory engine", "a local-first code assistant", "a vector search prototype",
    "a billing dashboard", "a CLI for agent memory", "a benchmark harness",
    "a graph traversal tool", "a consolidation pipeline", "an encrypted storage layer",
    "a Python SDK", "a Node binding", "a REST sidecar", "an MCP server",
]

CITIES = [
    "Lisbon", "Berlin", "Austin", "Tokyo", "Nairobi", "Montreal", "Singapore",
    "Buenos Aires", "Helsinki", "Seoul",
]

TOOLS = [
    "libSQL", "Candle", "Qdrant", "Mem0", "Ollama", "Tokio", "PyO3", "Docker",
    "SQLite", "Turso",
]

CATEGORIES = [
    # (text_template, query_template, fill_kind)
    ("{name} prefers short, direct answers with no filler.", "how should I answer {name}", "name"),
    ("{name} is working on {project}.", "what project is {name} building", "name_project"),
    ("{name}'s billing plan is {plan}.", "what is {name}'s billing plan", "name_plan"),
    ("{name} prefers local-first tools with no cloud dependency.", "what is {name}'s privacy preference", "name"),
    ("{name} wants benchmarks comparing BaseMyAI against Mem0 and Qdrant.", "what benchmark does {name} want", "name"),
    ("Agents must never read another agent's memory.", "what is the cross-agent isolation invariant", "none"),
    ("BaseMyAI stores vectors inside libSQL, not an external vector database.", "where does BaseMyAI store vectors", "none"),
    ("{name} wants a temporal memory demo with invalidated facts.", "what demo does {name} want", "name"),
    ("The release for {name}'s team requires Python wheels and Node prebuilds.", "what does the release require for {name}'s team", "name"),
    ("The CLI must support init, inspect, stats, recall, verify, and migrate.", "what subcommands must the cli support", "none"),
    ("{name} is based in {city} and works remotely.", "where is {name} based", "name_city"),
    ("{name} uses {tool} as part of their stack.", "what tool does {name} use", "name_tool"),
    ("{name}'s meeting with the {project} team is scheduled for next week.", "when is {name}'s next meeting", "name_project"),
    ("{name} reported a bug in the {tool} integration.", "who reported the {tool} bug", "name_tool"),
    ("{name} asked for the encryption key rotation policy.", "what did {name} ask about", "name"),
    ("{name} wants the benchmark corpus to be at least 500 items.", "how large should the benchmark corpus be", "name"),
    ("{name} reviewed the ADR about the storage engine trait.", "who reviewed the storage engine ADR", "name"),
    ("{name}'s favorite city to work from is {city}.", "where does {name} like to work from", "name_city"),
    ("{name} flagged that {tool} needs a version bump before release.", "what needs a version bump before release", "name_tool"),
    ("{name} is the point of contact for questions about {project}.", "who is the contact for {project}", "name_project"),
]

PLANS = ["Free", "Pro", "Team", "Enterprise"]


def fill(template: str, kind: str, rng: random.Random) -> tuple[str, dict[str, str]]:
    values: dict[str, str] = {}
    if "name" in kind:
        values["name"] = rng.choice(NAMES)
    if "project" in kind:
        values["project"] = rng.choice(PROJECTS)
    if "plan" in kind:
        values["plan"] = rng.choice(PLANS)
    if "city" in kind:
        values["city"] = rng.choice(CITIES)
    if "tool" in kind:
        values["tool"] = rng.choice(TOOLS)
    return template.format(**values), values


def main() -> None:
    rng = random.Random(SEED)
    target_count = 500
    out_path = Path(__file__).parent / "corpus.jsonl"

    rows: list[dict[str, str]] = []
    i = 0
    while len(rows) < target_count:
        text_tpl, query_tpl, kind = CATEGORIES[i % len(CATEGORIES)]
        text, values = fill(text_tpl, kind, rng)
        query = query_tpl.format(**values) if values else query_tpl
        rows.append({"id": f"m{i + 1:04d}", "text": text, "query": query})
        i += 1

    with out_path.open("w", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

    print(f"wrote {len(rows)} items to {out_path}")


if __name__ == "__main__":
    main()
