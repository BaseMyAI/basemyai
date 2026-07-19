"""basemyai — local memory engine for AI agents (Python bindings).

Re-exports the native module ``basemyai._internal`` (built by maturin/PyO3).
All memory operations are asynchronous (asyncio coroutines).

No setup required. ``path``/``agent_id``/``encryption_key``/``model_dir`` are
all optional: ``path`` defaults to ``./basemyai.bmai``, ``agent_id`` to
``"default"``, and the encryption key is generated at ``~/.basemyai/key`` on
first use if none exists (a notice is printed to stderr — back that file up,
it's the only copy). ``consent_to_fetch=True`` is the one real network op
(fetching the local embedding model once, then cached); without it,
``Memory.open()`` requires a model already cached or an explicit
``model_dir``. Run ``basemyai config set db-path <path>`` /
``basemyai config set agent <id>`` for a multi-agent or scripted setup — it's
never required.

Example
-------
>>> import asyncio, basemyai
>>> async def main():
...     mem = await basemyai.Memory.open(consent_to_fetch=True)
...     await mem.observe([("user", "what does basemyai store?")])
...     return mem.agent()
"""

from ._internal import (  # type: ignore[attr-defined]
    AgentStats,
    BasemyaiError,
    ContextBundle,
    ContextCitation,
    ContextItem,
    ContextSection,
    ContextTrace,
    ContextTraceEvent,
    ContextTraceSummary,
    ContextWarning,
    DedupCluster,
    EncryptionError,
    Entity,
    ExcludedMemory,
    InferenceError,
    Memory,
    MergedMemory,
    Record,
    RetrievalContribution,
    StorageError,
    ValidationError,
    __version__,
)

__all__ = [
    "Memory",
    "Record",
    "AgentStats",
    "Entity",
    "ContextBundle",
    "ContextSection",
    "ContextItem",
    "ContextCitation",
    "ExcludedMemory",
    "MergedMemory",
    "DedupCluster",
    "ContextWarning",
    "RetrievalContribution",
    "ContextTraceEvent",
    "ContextTraceSummary",
    "ContextTrace",
    "BasemyaiError",
    "ValidationError",
    "StorageError",
    "EncryptionError",
    "InferenceError",
    "__version__",
]
