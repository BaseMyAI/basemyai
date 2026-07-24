from __future__ import annotations

import asyncio

import basemyai

# No setup required. `path` defaults to "./basemyai.bmai", `agent_id` to
# "default", and the encryption key is generated at ~/.basemyai/key on first
# use if none exists (a one-line notice goes to stderr — back that file up,
# it's the only copy). Override any of these — or run
# `basemyai config set db-path|agent` once — for a multi-agent or scripted
# setup. `consent_to_fetch` is the one real network op (fetching the local
# embedding model, ~90MB): pass True once to consent, then it's cached.


async def main() -> None:
    mem = await basemyai.Memory.open(consent_to_fetch=True)

    # Layer defaults to "semantic" — pass one explicitly only when it matters.
    memory_id = await mem.remember("Alice works with the platform team.")

    # Hand over raw conversation turns; they land in the "episodic" layer
    # as-is. Background consolidation later promotes durable facts to
    # "semantic".
    await mem.observe(
        [
            ("user", "Who works with the platform team?"),
            ("assistant", "Alice works with the platform team."),
        ]
    )

    hits = await mem.recall("Who works with the platform team?", k=3)

    print(f"stored: {memory_id}")
    for hit in hits:
        print(f"{hit.score:.3f} [{hit.layer}] {hit.text}")

    context = await mem.compile_context(
        "Who works with the platform team?",
        token_budget=256,
        explain=True,
    )
    print(context.rendered)


if __name__ == "__main__":
    asyncio.run(main())
