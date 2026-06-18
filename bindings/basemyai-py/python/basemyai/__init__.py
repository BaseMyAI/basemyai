"""basemyai — local memory engine for AI agents (Python bindings).

Re-exports the native module ``basemyai._internal`` (built by maturin/PyO3).
All memory operations are asynchronous (asyncio coroutines).

Example
-------
>>> import asyncio, basemyai
>>> async def main():
...     mem = await basemyai.Memory.open_in_memory("agent-1")
...     mid = await mem.remember("the sky is blue", layer="semantic")
...     hits = await mem.recall("the sky is blue", k=5)
...     return [h.text for h in hits]
>>> asyncio.run(main())
['the sky is blue']
"""

from ._internal import (  # type: ignore[attr-defined]
    AgentStats,
    BasemyaiError,
    EncryptionError,
    Entity,
    InferenceError,
    Memory,
    Record,
    StorageError,
    ValidationError,
    __version__,
)

__all__ = [
    "Memory",
    "Record",
    "AgentStats",
    "Entity",
    "BasemyaiError",
    "ValidationError",
    "StorageError",
    "EncryptionError",
    "InferenceError",
    "__version__",
]
