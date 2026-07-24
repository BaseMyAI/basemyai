import { Memory } from "basemyai";

// No setup required. `path` defaults to `./basemyai.bmai`, `agentId` to
// `"default"`, and the encryption key is generated at `~/.basemyai/key` on
// first use if none exists (a one-line notice goes to stderr — back that
// file up, it's the only copy). Override any of these — or run
// `basemyai config set db-path|agent` once — for a multi-agent or scripted
// setup. `allowModelDownload` is the one real network op (fetching the local
// embedding model, ~90MB): pass `true` once to consent, then it's cached.
async function main(): Promise<void> {
  const memory = await Memory.open({ allowModelDownload: true });

  // Layer defaults to `semantic` — pass one explicitly only when it matters.
  await memory.remember("BaseMyAI stores local memories for an agent.");

  // Hand over raw conversation turns; they land in the `episodic` layer as-is.
  // Background consolidation later promotes durable facts to `semantic`.
  await memory.observe([
    { role: "user", content: "What does BaseMyAI store?" },
    { role: "assistant", content: "Local memories for an agent, across four layers." },
  ]);

  const hits = await memory.recall("local memories", 3);
  const context = await memory.compileContext({
    query: "What does BaseMyAI store?",
    tokenBudget: 256,
    explain: true,
  });

  console.log(hits);
  console.log(context.rendered);
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
