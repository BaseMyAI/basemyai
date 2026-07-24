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

test('compileContext returns a bounded typed bundle', async () => {
  const mem = await Memory.openInMemory('agent-context');
  const id = await mem.remember('BaseMyAI stores local agent memory.', 'semantic');

  const bundle = await mem.compileContext({
    query: 'local agent memory',
    tokenBudget: 128,
    explain: true,
  });

  expect(bundle.estimatedTokens).toBeLessThanOrEqual(128);
  expect(bundle.rendered).toContain('BaseMyAI stores local agent memory.');
  expect(bundle.citations.some((citation) => citation.memoryId === id)).toBe(true);
  expect(bundle.sections[0].kind).toBe('current_facts');
  expect(bundle.sections[0].items[0].validFrom).toBeGreaterThan(0);
  expect(bundle.sections[0].items[0].role).toBe('fact');
  expect(bundle.sections[0].items[0].inclusionReason).not.toBe('unknown');
  expect(bundle.sections[0].items[0].retrievalContributions.length).toBeGreaterThan(0);
  // Defaults when profile/renderFormat are omitted (R1.6/R1.7).
  expect(bundle.profile).toBe('balanced');
  expect(bundle.renderFormat).toBe('markdown');
  // `explain: true` requests a detailed trace, bounded but non-empty here.
  expect(bundle.trace.level).toBe('detailed');
  expect(bundle.trace.summary.includedItems).toBeGreaterThan(0);
  expect(bundle.trace.events.length).toBeGreaterThan(0);
});

test('compileContext honors profile and renderFormat (R1.6/R1.7)', async () => {
  const mem = await Memory.openInMemory('agent-context-profile');
  await mem.remember('call the deploy script before merging', 'semantic');

  const text = await mem.compileContext({
    query: 'deploy script',
    tokenBudget: 256,
    profile: 'coding',
    renderFormat: 'text',
  });
  expect(text.profile).toBe('coding');
  expect(text.renderFormat).toBe('text');
  expect(text.rendered).not.toContain('#');

  const json = await mem.compileContext({
    query: 'deploy script',
    tokenBudget: 256,
    renderFormat: 'json',
  });
  expect(json.renderFormat).toBe('json');
  expect(() => JSON.parse(json.rendered)).not.toThrow();
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
