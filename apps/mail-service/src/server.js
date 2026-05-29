import { register } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";

async function main() {
  process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../tsconfig.json", import.meta.url));
  process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
  register("ts-node/esm", pathToFileURL("./"));
  const { bootstrap } = await import("./main.ts");
  await bootstrap();
}

main().catch((error) => {
  console.error("Failed to start mail-service:", error);
  process.exitCode = 1;
});
