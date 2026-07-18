from __future__ import annotations

from typing import Literal

__version__: str
MemoryLayer = Literal["short_term", "episodic", "procedural", "semantic"]
CredentialMode = Literal["raw", "passphrase"]
ContextSourcePolicy = Literal["allow_all", "exclude_imported", "user_and_consolidation_only"]
ContextSectionKind = Literal["working_context", "current_facts", "procedures", "recent_events", "unknown"]
ContextTemporalStatus = Literal["current", "scheduled", "expired", "unknown"]
ExclusionReason = Literal["source_filtered", "not_currently_valid", "token_budget", "unknown"]

class BasemyaiError(Exception): ...
class ValidationError(ValueError): ...
class StorageError(BasemyaiError): ...
class EncryptionError(BasemyaiError): ...
class InferenceError(BasemyaiError): ...

class Record:
    id: str
    text: str
    layer: MemoryLayer
    score: float
    source: str
    trust: str
    valid_from: int
    valid_until: int | None

class AgentStats:
    short_term: int
    episodic: int
    procedural: int
    semantic: int
    total: int

class Entity:
    id: str
    kind: str
    label: str
    depth: int

class WatchEvent:
    agent_id: str
    kind: Literal["remembered", "invalidated", "forgotten", "consolidated", "unknown"]
    layer: MemoryLayer
    id: str

class ContextItem:
    text: str
    source_memory_ids: list[str]
    layer: MemoryLayer
    trust: str
    valid_from: int
    valid_until: int | None
    temporal_status: ContextTemporalStatus
    retrieval_score: float
    retrieval_rank: int
    estimated_tokens: int
    utility_score: float
    value_per_token: float
    freshness_score: float

class ContextSection:
    kind: ContextSectionKind
    items: list[ContextItem]

class ContextCitation:
    memory_id: str
    section: ContextSectionKind

class ExcludedMemory:
    memory_id: str
    reason: ExclusionReason
    temporal_status: ContextTemporalStatus

class MergedMemory:
    memory_id: str
    representative_memory_id: str

class ContextBundle:
    sections: list[ContextSection]
    rendered: str
    estimated_tokens: int
    compiled_at: int
    total_utility: float
    citations: list[ContextCitation]
    merged: list[MergedMemory]
    excluded: list[ExcludedMemory]

class MemoryWatch:
    def __aiter__(self) -> MemoryWatch: ...
    def __anext__(self) -> WatchEvent: ...

class Memory:
    @staticmethod
    async def open(
        path: str | None = None,
        agent_id: str | None = None,
        *,
        encryption_key: str | None = None,
        credential_mode: CredentialMode | None = None,
        model_dir: str | None = None,
        device: str = "auto",
        consent_to_fetch: bool = False,
    ) -> Memory: ...
    def agent(self) -> str: ...
    async def remember(self, text: str, layer: MemoryLayer = "semantic") -> str: ...
    async def observe(self, turns: list[tuple[str, str]]) -> list[str]: ...
    async def recall(self, query: str, k: int = 5) -> list[Record]: ...
    async def recall_by_layer(self, query: str, layer: MemoryLayer, k: int = 5) -> list[Record]: ...
    async def recall_hybrid(self, query: str, k: int = 5) -> list[Record]: ...
    async def compile_context(
        self,
        query: str,
        token_budget: int,
        *,
        candidate_limit: int = 64,
        include_procedural: bool = False,
        source_policy: ContextSourcePolicy = "exclude_imported",
        explain: bool = False,
    ) -> ContextBundle: ...
    async def invalidate(self, id: str) -> None: ...
    async def forget(self, id: str) -> None: ...
    async def stats(self) -> AgentStats: ...
    async def add_graph_entity(self, id: str, kind: str, label: str) -> None: ...
    async def add_graph_edge(self, src: str, relation: str, dst: str, weight: float = 1.0) -> None: ...
    async def recall_graph(self, start: str, max_depth: int = 2) -> list[Entity]: ...
    def watch(self, layer: MemoryLayer | None = None) -> MemoryWatch: ...

__all__: list[str]
