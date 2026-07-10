// Abonnements mémoire en direct (`Memory.watch`, ADR-022) côté binding Node.
// Miroir du couple `watch_delivers_remembered_notification_for_same_agent` /
// `watch_isolates_notifications_from_other_agents` côté MCP
// (crates/basemyai-mcp/tests/watch.rs) et `watch_isolates_events_from_other_agents`
// côté REST (crates/basemyai-rest/tests/api.rs) : ici testé au niveau NAPI —
// callback JS invoqué en direct depuis la tâche tokio de relais.

const { Memory } = require('../index.js');

/** Attend que `predicate()` devienne vrai, ou lève au bout de `timeoutMs`. */
async function waitFor(predicate, timeoutMs = 2000, intervalMs = 10) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  throw new Error('waitFor: timed out');
}

/** Laisse tourner la boucle d'événements un court instant sans rien attendre
 * de précis — utilisé pour les assertions négatives (« rien ne doit arriver »). */
function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

test('watch delivers a remembered event for the same agent', async () => {
  const mem = await Memory.openInMemory('watch-agent-a');
  const events = [];

  const handle = await mem.watch('watch-agent-a', undefined, (event) => {
    events.push(event);
  });

  const id = await mem.remember('watched fact', 'semantic');
  await waitFor(() => events.length > 0);

  expect(events[0].kind).toBe('remembered');
  expect(events[0].agentId).toBe('watch-agent-a');
  expect(events[0].layer).toBe('semantic');
  expect(events[0].id).toBe(id);

  handle.close();
});

test('watch filters out events for a mismatched agent_id (isolation)', async () => {
  // Un seul `Memory`, propriétaire réel de l'agent 'watch-agent-real'. On
  // s'abonne avec un `agent_id` *différent* : c'est exactement le filtre
  // appliqué côté `MemorySubscription::recv` (jamais délégué à l'appelant)
  // qui doit bloquer la livraison, quel que soit ce qui est demandé ici.
  const mem = await Memory.openInMemory('watch-agent-real');
  const events = [];

  const handle = await mem.watch('someone-else', undefined, (event) => {
    events.push(event);
  });

  for (let i = 0; i < 5; i += 1) {
    await mem.remember(`other agent fact ${i}`, 'semantic');
  }
  await sleep(200);

  expect(events).toEqual([]);
  handle.close();
});

test('watch does not leak events between two separate agent memories', async () => {
  const a = await Memory.openInMemory('watch-agent-x');
  const b = await Memory.openInMemory('watch-agent-y');
  const events = [];

  const handle = await a.watch('watch-agent-x', undefined, (event) => {
    events.push(event);
  });

  for (let i = 0; i < 5; i += 1) {
    await b.remember(`agent y fact ${i}`, 'semantic');
  }
  await sleep(200);

  expect(events).toEqual([]);
  handle.close();
});

test('watch respects the optional layer filter', async () => {
  const mem = await Memory.openInMemory('watch-agent-layer');
  const events = [];

  const handle = await mem.watch('watch-agent-layer', 'procedural', (event) => {
    events.push(event);
  });

  await mem.remember('a semantic fact', 'semantic');
  await mem.remember('a procedural fact', 'procedural');
  await waitFor(() => events.length > 0);
  await sleep(100);

  expect(events).toHaveLength(1);
  expect(events[0].layer).toBe('procedural');

  handle.close();
});

test('WatchHandle.close() stops delivering events (unsubscribe)', async () => {
  const mem = await Memory.openInMemory('watch-agent-close');
  const events = [];

  const handle = await mem.watch('watch-agent-close', undefined, (event) => {
    events.push(event);
  });

  await mem.remember('before close', 'semantic');
  await waitFor(() => events.length === 1);

  handle.close();
  // Idempotent : un second appel ne doit pas jeter.
  expect(() => handle.close()).not.toThrow();

  await mem.remember('after close', 'semantic');
  await sleep(200);

  expect(events).toHaveLength(1);
});
