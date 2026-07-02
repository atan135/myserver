import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

export * from "../../rollout/rollout-transfer-cli.js";
import { main } from "../../rollout/rollout-transfer-cli.js";

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  main().catch((error) => {
    console.log(JSON.stringify({
      ok: false,
      mode: "transfer-fatal-error",
      errorCode: error?.code || error?.errorCode || "FATAL_ERROR",
      error: error?.message || String(error)
    }, null, 2));
    process.exitCode = 1;
  });
}
