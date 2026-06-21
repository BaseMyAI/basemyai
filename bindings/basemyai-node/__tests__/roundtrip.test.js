// Spike test du pont async NAPI-RS + tokio : remember/recall/stats/invalidate.
// Mémoire construite via openInMemory (embedder déterministe, base :memory:).

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { Memory } = require('../index.js');

function tempDbPath(prefix) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  return path.join(dir, 'memory.db');
}

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

test('graph entity and edge roundtrip', async () => {
  const mem = await Memory.openInMemory('agent-graph');

  await mem.addGraphEntity('A', 'person', 'Alice');
  await mem.addGraphEntity('B', 'org', 'Beta');
  await mem.addGraphEdge('A', 'knows', 'B');

  const reached = await mem.recallGraph('A');
  expect(reached).toEqual([
    {
      id: 'B',
      kind: 'org',
      label: 'Beta',
      depth: 1,
    },
  ]);
});

test('graph data does not leak between in-memory agents', async () => {
  const a = await Memory.openInMemory('graph-a');
  const b = await Memory.openInMemory('graph-b');

  await a.addGraphEntity('A', 'person', 'Alice');
  await a.addGraphEntity('B', 'org', 'Beta');
  await a.addGraphEdge('A', 'knows', 'B');

  const reachedB = await b.recallGraph('A');
  expect(reachedB).toEqual([]);
});

test('same store isolates memory and graph by agent', async () => {
  const dbPath = tempDbPath('basemyai-node-shared-');
  const a = await Memory.openTestFile(dbPath, 'agent-a');
  const b = await Memory.openTestFile(dbPath, 'agent-b');

  await a.remember('secret of agent A', 'semantic');
  await b.remember('public note of agent B', 'semantic');

  const hitsB = await b.recall('secret of agent A', 5);
  expect(hitsB.every((h) => h.text !== 'secret of agent A')).toBe(true);
  const statsB = await b.stats();
  expect(statsB.total).toBe(1);

  await a.addGraphEntity('alice', 'person', 'Alice A');
  await a.addGraphEntity('acme', 'organization', 'Acme A');
  await a.addGraphEdge('alice', 'works_at', 'acme');

  await b.addGraphEntity('alice', 'person', 'Alice B');
  await b.addGraphEntity('acme', 'organization', 'Acme B');
  await b.addGraphEdge('alice', 'works_at', 'acme');

  expect(await a.recallGraph('alice', 1)).toEqual([
    { id: 'acme', kind: 'organization', label: 'Acme A', depth: 1 },
  ]);
  expect(await b.recallGraph('alice', 1)).toEqual([
    { id: 'acme', kind: 'organization', label: 'Acme B', depth: 1 },
  ]);
});

const productionOpenEnabled =
  process.env.BASEMYAI_RUN_PRODUCTION_OPEN === '1' &&
  Boolean(process.env.BASEMYAI_MODEL_PATH) &&
  Boolean(process.env.BASEMYAI_ENCRYPTION_KEY);

(productionOpenEnabled ? test : test.skip)('production Memory.open uses encrypted file and local model', async () => {
  const dbPath = tempDbPath('basemyai-node-prod-');
  const mem = await Memory.open({
    path: dbPath,
    agentId: 'node-production-open',
    encryptionKey: process.env.BASEMYAI_ENCRYPTION_KEY,
    modelPath: process.env.BASEMYAI_MODEL_PATH,
    allowModelDownload: false,
  });

  await mem.remember('production open smoke test', 'semantic');
  const stats = await mem.stats();
  expect(stats.total).toBe(1);
});
