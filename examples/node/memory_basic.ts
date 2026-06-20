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
    path: process.env.BASEMYAI_DB_PATH ?? "basemyai.bmai",
    agentId: process.env.BASEMYAI_AGENT_ID ?? "node-example",
    encryptionKey: requireEnv("BASEMYAI_ENCRYPTION_KEY"),
    modelPath: requireEnv("BASEMYAI_MODEL_PATH"),
    allowModelDownload: false,
  });

  await memory.remember("BaseMyAI stores local memories for an agent.", "semantic");
  const hits = await memory.recall("local memories", 3);

  console.log(hits);
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
