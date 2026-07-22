import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

export {
  main,
  parseControlPlaneArgs,
  runControlPlaneRoomTransfer,
  validateControlPlaneOptions
} from "./rollout-control-plane-cli.js";
import { main } from "./rollout-control-plane-cli.js";

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  main().catch((error) => {
    console.log(JSON.stringify({
      ok: false,
      stage: "control_plane",
      errorCode: error?.code || "CONTROL_PLANE_REQUEST_FAILED",
      error: error?.message || String(error)
    }, null, 2));
    process.exitCode = 1;
  });
}
