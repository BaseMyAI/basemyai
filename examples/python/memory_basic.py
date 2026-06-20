from __future__ import annotations

import argparse
import asyncio
import os

import basemyai


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Basic BaseMyAI memory example.")
    parser.add_argument("--db", default="basemyai-example.bmai", help="Path to the local libSQL database.")
    parser.add_argument("--agent", default="agent-1", help="Agent id stored in this database.")
    parser.add_argument("--model-dir", required=True, help="Path to a local embedding model directory.")
    parser.add_argument("--device", default="auto", help="Device: auto, cpu, metal, cuda, or cuda:<index>.")
    parser.add_argument(
        "--encryption-key",
        default=os.environ.get("BASEMYAI_ENCRYPTION_KEY"),
        help="Encryption key, or set BASEMYAI_ENCRYPTION_KEY.",
    )
    return parser.parse_args()


async def main() -> None:
    args = parse_args()
    if not args.encryption_key:
        raise SystemExit("Provide --encryption-key or BASEMYAI_ENCRYPTION_KEY.")

    mem = await basemyai.Memory.open(
        args.db,
        args.agent,
        args.encryption_key,
        model_dir=args.model_dir,
        device=args.device,
        consent_to_fetch=False,
    )

    memory_id = await mem.remember("Alice works with the platform team.", layer="semantic")
    hits = await mem.recall("Who works with the platform team?", k=3)

    print(f"stored: {memory_id}")
    for hit in hits:
        print(f"{hit.score:.3f} [{hit.layer}] {hit.text}")


if __name__ == "__main__":
    asyncio.run(main())
