import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

export * from "../../rollout/rollout-fault-drill-cli.js";
import { main } from "../../rollout/rollout-fault-drill-cli.js";

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  main().catch((error) => {
    console.error(error.message);
    console.error("Run with --help for usage.");
    process.exitCode = 1;
  });
}
