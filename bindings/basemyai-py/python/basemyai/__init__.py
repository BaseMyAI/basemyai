"""basemyai — local memory engine for AI agents (Python bindings).

Re-exports the native module ``basemyai._internal`` (built by maturin/PyO3).
All memory operations are asynchronous (asyncio coroutines).

Example
-------
>>> import asyncio, basemyai
>>> async def main():
...     mem = await basemyai.Memory.open(
...         path="./agent.bmai",
...         agent_id="agent-1",
...         encryption_key="change-me",
...         model_dir="~/.basemyai/models/all-MiniLM-L6-v2",
...     )
...     return mem.agent()
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
