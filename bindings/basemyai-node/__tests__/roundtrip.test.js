// Spike test du pont async NAPI-RS + tokio : remember/recall/stats/invalidate.
// Mémoire construite via openInMemory (embedder déterministe, base :memory:).

const { Memory } = require('../index.js');

test('remember / recall / stats / invalidate roundtrip', async () => {
  const mem = await Memory.openInMemory('agent-1');
  expect(mem.agent()).toBe('agent-1');

  const id = await mem.remember('the sky is blue', 'semantic');
  expect(typeof id).toBe('string');
  expect(id.length).toBeGreaterThan(0);

  const hits = await mem.recall('the sky is blue', 5);
  expect(hits.some((h) => h.id === id && h.text === 'the sky is blue')).toBe(true);
  expect(hits.every((h) => h.layer === 'semantic')).toBe(true);

  const stats = await mem.stats();
  expect(stats.semantic).toBe(1);
  expect(stats.total).toBe(1);

  await mem.invalidate(id);
  const after = await mem.recall('the sky is blue', 5);
  expect(after.every((h) => h.id !== id)).toBe(true);
});

test('recallHybrid surfaces exact term via BM25', async () => {
  const mem = await Memory.openInMemory('agent-1');
  await mem.remember('invoice ACME-42 reference number', 'semantic');
  await mem.remember('grass is green in spring', 'semantic');

  const hits = await mem.recallHybrid('ACME-42', 5);
  expect(hits.some((h) => h.text.includes('ACME-42'))).toBe(true);
});

test('isolation between agents', async () => {
  const a = await Memory.openInMemory('a');
  const b = await Memory.openInMemory('b');
  await a.remember('secret of A', 'semantic');
  const hitsB = await b.recall('secret of A', 5);
  expect(hitsB).toEqual([]);
});

test('unknown layer rejects', async () => {
  const mem = await Memory.openInMemory('a');
  await expect(mem.remember('x', 'bogus')).rejects.toThrow();
});

test('empty agent id rejects', async () => {
  await expect(Memory.openInMemory('')).rejects.toThrow();
});

test('remember defaults to semantic layer', async () => {
  const mem = await Memory.openInMemory('a');
  await mem.remember('no explicit layer');
  const stats = await mem.stats();
  expect(stats.semantic).toBe(1);
});

test('recallByLayer filters to one memory layer', async () => {
  const mem = await Memory.openInMemory('agent-layer');
  await mem.remember('deploy runbook uses cargo clippy', 'procedural');
  await mem.remember('cargo clippy is part of the baseline', 'semantic');

  const hits = await mem.recallByLayer('cargo clippy', 'procedural', 5);
  expect(hits.length).toBeGreaterThan(0);
  expect(hits.every((h) => h.layer === 'procedural')).toBe(true);
  expect(hits.some((h) => h.text.includes('deploy runbook'))).toBe(true);
});

test('forget physically removes a memory', async () => {
  const mem = await Memory.openInMemory('agent-forget');
  const id = await mem.remember('delete me permanently', 'semantic');

  await mem.forget(id);

  const hits = await mem.recall('delete me permanently', 5);
  expect(hits.every((h) => h.id !== id)).toBe(true);
  const stats = await mem.stats();
  expect(stats.total).toBe(0);
});
