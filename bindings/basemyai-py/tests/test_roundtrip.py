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
async def test_compile_context_returns_bounded_typed_bundle():
    mem = await basemyai.Memory.open_in_memory("agent-context")
    memory_id = await mem.remember("BaseMyAI stores local agent memory.", layer="semantic")

    bundle = await mem.compile_context("local agent memory", token_budget=128, explain=True)

    assert bundle.estimated_tokens <= 128
    assert "BaseMyAI stores local agent memory." in bundle.rendered
    assert any(citation.memory_id == memory_id for citation in bundle.citations)
    assert bundle.sections[0].kind == "current_facts"
    assert bundle.sections[0].items[0].valid_from > 0


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


@pytest.mark.asyncio
async def test_watch_delivers_remembered_event():
    """ADR-022 seconde vague : `async for event in memory.watch()` (PLAN.md §P2.1)."""
    mem = await basemyai.Memory.open_in_memory("agent-1")
    watcher = mem.watch()

    mid = await mem.remember("watched fact", layer="semantic")

    event = await anext(watcher)
    assert event.kind == "remembered"
    assert event.id == mid
    assert event.agent_id == "agent-1"
    assert event.layer == "semantic"


@pytest.mark.asyncio
async def test_watch_filters_by_layer():
    mem = await basemyai.Memory.open_in_memory("agent-1")
    watcher = mem.watch(layer="episodic")

    # Un souvenir sémantique ne doit pas franchir le filtre de couche.
    await mem.remember("semantic noise", layer="semantic")
    episodic_id = await mem.remember("episodic signal", layer="episodic")

    event = await anext(watcher)
    assert event.id == episodic_id
    assert event.layer == "episodic"


@pytest.mark.asyncio
async def test_watch_isolates_events_from_other_agents():
    """Test adversarial ADR-022 : la mémoire d'un autre agent ne fuite jamais.

    NB : n'utilise volontairement PAS `asyncio.wait_for(..., timeout=...)` pour
    prouver « rien n'arrive » : annuler une coroutine pyo3-async-runtimes en
    attente au milieu d'un `tokio::sync::Mutex`/`broadcast::Receiver::recv().await`
    provoque un access violation sous Windows (crash différé, observé sur le test
    suivant) — cohérent avec le hasard documenté sur l'abandon de tâches tokio en
    plein I/O (voir `crates/basemyai-mcp/tests/sampling.rs`). À la place, on
    prouve l'isolation en montrant que les 5 écritures de l'agent B ne se
    glissent jamais devant l'unique écriture de l'agent A : le premier (et seul)
    événement reçu par le watcher de A est bien celui de A.
    """
    a = await basemyai.Memory.open_in_memory("agent-a")
    b = await basemyai.Memory.open_in_memory("agent-b")
    watcher = a.watch()

    for i in range(5):
        await b.remember(f"other agent fact {i}", layer="semantic")
    a_id = await a.remember("agent a's own fact", layer="semantic")

    event = await anext(watcher)
    assert event.agent_id == "agent-a"
    assert event.id == a_id


def test_exports_and_typing_marker_present():
    assert "Memory" in basemyai.__all__
    assert hasattr(basemyai.Memory, "recall_by_layer")
    assert hasattr(basemyai.Memory, "add_graph_entity")
    assert hasattr(basemyai.Memory, "add_graph_edge")
    assert hasattr(basemyai.Memory, "compile_context")
    assert "ContextBundle" in basemyai.__all__
    package_dir = Path(basemyai.__path__[0])
    assert (package_dir / "__init__.pyi").is_file()
    assert (package_dir / "py.typed").is_file()
