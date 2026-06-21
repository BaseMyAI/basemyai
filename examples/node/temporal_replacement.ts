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
    path: process.env.BASEMYAI_DB_PATH ?? "basemyai-temporal-demo.bmai",
    agentId: process.env.BASEMYAI_AGENT_ID ?? "temporal-demo",
    encryptionKey: requireEnv("BASEMYAI_ENCRYPTION_KEY"),
    modelPath: requireEnv("BASEMYAI_MODEL_PATH"),
    allowModelDownload: false,
  });

  const oldId = await memory.remember("The user is on the Free billing plan.", "semantic");
  await memory.invalidate(oldId);
  await memory.remember("The user is on the Pro billing plan.", "semantic");

  const hits = await memory.recallHybrid("current billing plan", 5);
  console.log("Recall for `current billing plan`:");
  for (const hit of hits) {
    console.log(`${hit.score.toFixed(4)} [${hit.layer}] ${hit.text}`);
  }

  if (!hits.some((hit) => hit.text.includes("Pro billing plan"))) {
    throw new Error("expected current Pro plan fact");
  }
  if (hits.some((hit) => hit.text.includes("Free billing plan"))) {
    throw new Error("invalidated Free plan fact was recalled");
  }
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
