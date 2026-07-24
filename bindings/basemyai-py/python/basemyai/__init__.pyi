from __future__ import annotations

from typing import Literal

__version__: str
MemoryLayer = Literal["short_term", "episodic", "procedural", "semantic"]
CredentialMode = Literal["raw", "passphrase"]
ContextSourcePolicy = Literal["allow_all", "exclude_imported", "user_and_consolidation_only"]
ContextSectionKind = Literal["working_context", "current_facts", "procedures", "recent_events", "unknown"]
ContextTemporalStatus = Literal["current", "scheduled", "expired", "unknown"]
ExclusionReason = Literal["source_filtered", "not_currently_valid", "token_budget", "profile_quota", "unknown"]
# Compilation profile: weights and per-role quotas only, never permissions.
ContextProfile = Literal["balanced", "conversation", "coding", "execution", "safety_critical", "unknown"]
ContextRenderFormat = Literal["text", "markdown", "json", "unknown"]
# Derived only from layer + typed provenance, never from free text.
ContextRole = Literal["fact", "constraint", "procedure", "event", "reference", "uncertain_data", "unknown"]
InclusionReason = Literal["section_reservation", "value_per_token", "local_replacement", "unknown"]
ContextTraceLevel = Literal["compact", "detailed"]
ContextTraceEventKind = Literal["included", "excluded", "deduplicated", "warning", "unknown"]
ContextWarningKind = Literal["incompatible_metadata", "unknown"]

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

class RetrievalContribution:
    """A memory recalled alongside the item's representative before dedup/filtering."""

    memory_id: str
    retrieval_rank: int
    retrieval_score: float

class ContextItem:
    text: str
    source_memory_ids: list[str]
    layer: MemoryLayer
    trust: str
    role: ContextRole
    valid_from: int
    valid_until: int | None
    temporal_status: ContextTemporalStatus
    retrieval_score: float
    retrieval_rank: int
    retrieval_contributions: list[RetrievalContribution]
    estimated_tokens: int
    utility_score: float
    value_per_token: float
    freshness_score: float
    inclusion_reason: InclusionReason

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
    role: ContextRole
    retrieval_contribution: RetrievalContribution

class MergedMemory:
    """One absorbed-memory -> representative pair. See `DedupCluster` for the full groups."""

    memory_id: str
    representative_memory_id: str

class DedupCluster:
    """A complete group produced by exact-text deduplication."""

    representative_memory_id: str
    memory_ids: list[str]

class ContextWarning:
    """Conservative warning derived only from explicit metadata — never an inferred semantic contradiction."""

    kind: ContextWarningKind
    memory_ids: list[str]

class ContextTraceEvent:
    """One event of a detailed trace. `kind` discriminates which of the other fields are populated."""

    kind: ContextTraceEventKind
    memory_id: str | None
    role: ContextRole | None
    inclusion_reason: InclusionReason | None
    contributions: list[RetrievalContribution] | None
    excluded: ExcludedMemory | None
    dedup_cluster: DedupCluster | None
    warning: ContextWarning | None

class ContextTraceSummary:
    """Always-present counters, computed before any detailed-trace truncation."""

    included_items: int
    included_memories: int
    excluded_memories: int
    dedup_clusters: int
    warnings: int

class ContextTrace:
    """Compact by default (summary only); detailed and size-bounded with `explain=True`."""

    level: ContextTraceLevel
    summary: ContextTraceSummary
    events: list[ContextTraceEvent]
    total_events: int
    truncated: bool

class ContextBundle:
    sections: list[ContextSection]
    rendered: str
    estimated_tokens: int
    profile: ContextProfile
    render_format: ContextRenderFormat
    compiled_at: int
    total_utility: float
    citations: list[ContextCitation]
    merged: list[MergedMemory]
    excluded: list[ExcludedMemory]
    dedup_clusters: list[DedupCluster]
    warnings: list[ContextWarning]
    trace: ContextTrace

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
        profile: ContextProfile = "balanced",
        render_format: ContextRenderFormat = "markdown",
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
