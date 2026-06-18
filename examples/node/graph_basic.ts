import { Memory } from "basemyai";

function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} is required`);
  }
  return value;
}

async function main(): Promise<void> {
  const memory = await Memory.open({
    path: process.env.BASEMYAI_DB_PATH ?? "basemyai.db",
    agentId: process.env.BASEMYAI_AGENT_ID ?? "node-example",
    encryptionKey: requireEnv("BASEMYAI_ENCRYPTION_KEY"),
    modelPath: requireEnv("BASEMYAI_MODEL_PATH"),
    allowModelDownload: false,
  });

  await memory.addGraphEntity("alice", "person", "Alice");
  await memory.addGraphEntity("acme", "organization", "Acme");
  await memory.addGraphEdge("alice", "works_at", "acme");

  const reached = await memory.recallGraph("alice", 2);
  console.log(reached);
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
