from __future__ import annotations

from typing import Literal

__version__: str
MemoryLayer = Literal["short_term", "episodic", "procedural", "semantic"]

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

class MemoryWatch:
    def __aiter__(self) -> MemoryWatch: ...
    def __anext__(self) -> WatchEvent: ...

class Memory:
    @staticmethod
    async def open(
        path: str,
        agent_id: str,
        encryption_key: str,
        *,
        model_dir: str | None = None,
        device: str = "auto",
        consent_to_fetch: bool = False,
    ) -> Memory: ...
    def agent(self) -> str: ...
    async def remember(self, text: str, layer: MemoryLayer = "semantic") -> str: ...
    async def recall(self, query: str, k: int = 5) -> list[Record]: ...
    async def recall_by_layer(self, query: str, layer: MemoryLayer, k: int = 5) -> list[Record]: ...
    async def recall_hybrid(self, query: str, k: int = 5) -> list[Record]: ...
    async def invalidate(self, id: str) -> None: ...
    async def forget(self, id: str) -> None: ...
    async def stats(self) -> AgentStats: ...
    async def add_graph_entity(self, id: str, kind: str, label: str) -> None: ...
    async def add_graph_edge(self, src: str, relation: str, dst: str, weight: float = 1.0) -> None: ...
    async def recall_graph(self, start: str, max_depth: int = 2) -> list[Entity]: ...
    def watch(self, layer: MemoryLayer | None = None) -> MemoryWatch: ...

__all__: list[str]
