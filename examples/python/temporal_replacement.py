from __future__ import annotations

import argparse
import asyncio
import os

import basemyai


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Temporal replacement BaseMyAI example.")
    parser.add_argument("--db", default="basemyai-temporal-demo.bmai")
    parser.add_argument("--agent", default="temporal-demo")
    parser.add_argument("--model-dir", required=True, help="Path to a local embedding model directory.")
    parser.add_argument("--device", default="auto")
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

    old_id = await mem.remember("The user is on the Free billing plan.", layer="semantic")
    await mem.invalidate(old_id)
    await mem.remember("The user is on the Pro billing plan.", layer="semantic")

    hits = await mem.recall_hybrid("current billing plan", k=5)
    print("Recall for `current billing plan`:")
    for hit in hits:
        print(f"{hit.score:.4f} [{hit.layer}] {hit.text}")

    assert any("Pro billing plan" in hit.text for hit in hits)
    assert all("Free billing plan" not in hit.text for hit in hits)


if __name__ == "__main__":
    asyncio.run(main())
