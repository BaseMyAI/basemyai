"""Spike test du pont async PyO3 + tokio : remember/recall/stats/invalidate.

Construit la mémoire via ``open_in_memory`` (embedder déterministe, base
:memory: — pas de CMake ni de modèle). Vérifie que chaque opération est bien une
coroutine asyncio pilotée par le runtime tokio interne.
"""

import os

import pytest
from pathlib import Path

import basemyai


@pytest.mark.asyncio
async def test_remember_recall_stats_invalidate():
    mem = await basemyai.Memory.open_in_memory("agent-1")
    assert mem.agent() == "agent-1"

    mid = await mem.remember("the sky is blue", layer="semantic")
    assert isinstance(mid, str) and mid

    hits = await mem.recall("the sky is blue", k=5)
    assert any(h.id == mid and h.text == "the sky is blue" for h in hits)
    assert all(h.layer == "semantic" for h in hits)

    stats = await mem.stats()
    assert stats.semantic == 1
    assert stats.total == 1

    await mem.invalidate(mid)
    after = await mem.recall("the sky is blue", k=5)
    assert all(h.id != mid for h in after)


@pytest.mark.asyncio
async def test_recall_hybrid_surfaces_exact_term():
    mem = await basemyai.Memory.open_in_memory("agent-1")
    await mem.remember("invoice ACME-42 reference number", layer="semantic")
    await mem.remember("grass is green in spring", layer="semantic")

    hits = await mem.recall_hybrid("ACME-42", k=5)
    assert any("ACME-42" in h.text for h in hits)


@pytest.mark.asyncio
async def test_recall_by_layer_filters_results():
    mem = await basemyai.Memory.open_in_memory("agent-1")
    semantic_id = await mem.remember("layered content shared token", layer="semantic")
    episodic_id = await mem.remember("layered content shared token", layer="episodic")

    semantic_hits = await mem.recall_by_layer("layered content", "semantic", k=5)
    assert any(h.id == semantic_id for h in semantic_hits)
    assert all(h.layer == "semantic" for h in semantic_hits)
    assert all(h.id != episodic_id for h in semantic_hits)

    episodic_hits = await mem.recall_by_layer("layered content", "episodic", k=5)
    assert any(h.id == episodic_id for h in episodic_hits)
    assert all(h.layer == "episodic" for h in episodic_hits)
    assert all(h.id != semantic_id for h in episodic_hits)


@pytest.mark.asyncio
async def test_forget_removes_memory():
    mem = await basemyai.Memory.open_in_memory("agent-1")
    mid = await mem.remember("erase this memory", layer="semantic")

    await mem.forget(mid)

    hits = await mem.recall("erase this memory", k=5)
    assert all(h.id != mid for h in hits)
    stats = await mem.stats()
    assert stats.total == 0


@pytest.mark.asyncio
async def test_graph_add_entity_edge_and_recall():
    mem = await basemyai.Memory.open_in_memory("agent-1")

    await mem.add_graph_entity("A", "person", "Alice")
    await mem.add_graph_entity("B", "company", "Beta")
    await mem.add_graph_edge("A", "works_with", "B")

    reached = await mem.recall_graph("A")
    assert any(e.id == "B" and e.label == "Beta" and e.depth == 1 for e in reached)


@pytest.mark.asyncio
async def test_graph_does_not_cross_in_memory_agents():
    a = await basemyai.Memory.open_in_memory("agent-a")
    b = await basemyai.Memory.open_in_memory("agent-b")

    await a.add_graph_entity("A", "person", "Alice")
    await a.add_graph_entity("B", "company", "Beta")
    await a.add_graph_edge("A", "works_with", "B")

    assert await b.recall_graph("A") == []


@pytest.mark.asyncio
async def test_isolation_between_agents():
    a = await basemyai.Memory.open_in_memory("a")
    b = await basemyai.Memory.open_in_memory("b")
    await a.remember("secret of A", layer="semantic")
    hits_b = await b.recall("secret of A", k=5)
    assert hits_b == []


@pytest.mark.asyncio
async def test_same_store_isolates_memory_and_graph_by_agent(tmp_path: Path):
    db_path = str(tmp_path / "shared.db")
    a = await basemyai.Memory.open_test_file(db_path, "agent-a")
    b = await basemyai.Memory.open_test_file(db_path, "agent-b")

    await a.remember("secret of agent A", layer="semantic")
    await b.remember("public note of agent B", layer="semantic")

    hits_b = await b.recall("secret of agent A", k=5)
    assert all(h.text != "secret of agent A" for h in hits_b)
    stats_b = await b.stats()
    assert stats_b.total == 1

    await a.add_graph_entity("alice", "person", "Alice A")
    await a.add_graph_entity("acme", "organization", "Acme A")
    await a.add_graph_edge("alice", "works_at", "acme")

    await b.add_graph_entity("alice", "person", "Alice B")
    await b.add_graph_entity("acme", "organization", "Acme B")
    await b.add_graph_edge("alice", "works_at", "acme")

    seen_a = await a.recall_graph("alice", max_depth=1)
    seen_b = await b.recall_graph("alice", max_depth=1)

    assert [(e.id, e.kind, e.label, e.depth) for e in seen_a] == [("acme", "organization", "Acme A", 1)]
    assert [(e.id, e.kind, e.label, e.depth) for e in seen_b] == [("acme", "organization", "Acme B", 1)]


production_open_enabled = (
    os.environ.get("BASEMYAI_RUN_PRODUCTION_OPEN") == "1"
    and bool(os.environ.get("BASEMYAI_MODEL_PATH"))
    and bool(os.environ.get("BASEMYAI_ENCRYPTION_KEY"))
)


@pytest.mark.asyncio
@pytest.mark.skipif(
    not production_open_enabled,
    reason="set BASEMYAI_RUN_PRODUCTION_OPEN=1, BASEMYAI_MODEL_PATH and BASEMYAI_ENCRYPTION_KEY",
)
async def test_production_open_encrypted_file_with_local_model(tmp_path: Path):
    mem = await basemyai.Memory.open(
        path=str(tmp_path / "production.db"),
        agent_id="python-production-open",
        encryption_key=os.environ["BASEMYAI_ENCRYPTION_KEY"],
        model_dir=os.environ["BASEMYAI_MODEL_PATH"],
        consent_to_fetch=False,
    )

    await mem.remember("production open smoke test", layer="semantic")
    stats = await mem.stats()
    assert stats.total == 1


@pytest.mark.asyncio
async def test_unknown_layer_raises_validation_error():
    mem = await basemyai.Memory.open_in_memory("a")
    with pytest.raises(basemyai.ValidationError):
        await mem.remember("x", layer="bogus")


@pytest.mark.asyncio
async def test_empty_agent_raises_validation_error():
    with pytest.raises(basemyai.ValidationError):
        await basemyai.Memory.open_in_memory("")


def test_exports_and_typing_marker_present():
    assert "Memory" in basemyai.__all__
    assert hasattr(basemyai.Memory, "recall_by_layer")
    assert hasattr(basemyai.Memory, "add_graph_entity")
    assert hasattr(basemyai.Memory, "add_graph_edge")
    package_dir = Path(basemyai.__path__[0])
    assert (package_dir / "__init__.pyi").is_file()
    assert (package_dir / "py.typed").is_file()
