"""Spike test du pont async PyO3 + tokio : remember/recall/stats/invalidate.

Construit la mémoire via ``open_in_memory`` (embedder déterministe, base
:memory: — pas de CMake ni de modèle). Vérifie que chaque opération est bien une
coroutine asyncio pilotée par le runtime tokio interne.
"""

import pytest

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
async def test_isolation_between_agents():
    a = await basemyai.Memory.open_in_memory("a")
    b = await basemyai.Memory.open_in_memory("b")
    await a.remember("secret of A", layer="semantic")
    hits_b = await b.recall("secret of A", k=5)
    assert hits_b == []


@pytest.mark.asyncio
async def test_unknown_layer_raises_validation_error():
    mem = await basemyai.Memory.open_in_memory("a")
    with pytest.raises(basemyai.ValidationError):
        await mem.remember("x", layer="bogus")


@pytest.mark.asyncio
async def test_empty_agent_raises_validation_error():
    with pytest.raises(basemyai.ValidationError):
        await basemyai.Memory.open_in_memory("")
