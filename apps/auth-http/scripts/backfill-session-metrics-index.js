#!/usr/bin/env node
import process from "node:process";

import Redis from "ioredis";

import { sessionMetricsIndexKeys } from "../src/session-metrics-index.js";

const CONFIRMATION = "backfill-session-metrics-index";
const ZADD_MAX_SCORE_LUA = `
  local current = redis.call("ZSCORE", KEYS[1], ARGV[2])
  if not current or tonumber(ARGV[1]) > tonumber(current) then
    return redis.call("ZADD", KEYS[1], ARGV[1], ARGV[2])
  end
  return 0
`;

function parseArgs(argv) {
  const options = {
    apply: false,
    confirm: "",
    batchSize: 100,
    delayMs: 25
  };
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--apply") {
      options.apply = true;
    } else if (value === "--confirm") {
      options.confirm = argv[++index] || "";
    } else if (value === "--batch-size") {
      options.batchSize = Number.parseInt(argv[++index] || "", 10);
    } else if (value === "--delay-ms") {
      options.delayMs = Number.parseInt(argv[++index] || "", 10);
    } else if (value === "--help") {
      options.help = true;
    } else {
      throw new Error(`unknown argument: ${value}`);
    }
  }
  if (!Number.isSafeInteger(options.batchSize) || options.batchSize < 10 || options.batchSize > 1000) {
    throw new Error("--batch-size must be between 10 and 1000");
  }
  if (!Number.isSafeInteger(options.delayMs) || options.delayMs < 0 || options.delayMs > 1000) {
    throw new Error("--delay-ms must be between 0 and 1000");
  }
  if (options.apply && options.confirm !== CONFIRMATION) {
    throw new Error(`--apply requires --confirm ${CONFIRMATION}`);
  }
  return options;
}

function printHelp() {
  console.log(`Usage: node apps/auth-http/scripts/backfill-session-metrics-index.js [options]

Reads existing session keys and builds bounded session metrics indexes.
Dry-run is the default and never writes Redis.

Options:
  --batch-size <10..1000>  SCAN and pipeline batch size (default: 100)
  --delay-ms <0..1000>     Delay between SCAN batches (default: 25)
  --apply                  Write index members
  --confirm ${CONFIRMATION}
  --help

Environment:
  REDIS_URL                Explicit Redis target
  REDIS_KEY_PREFIX         Optional environment key prefix`);
}

async function run() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printHelp();
    return;
  }

  const redisUrl = process.env.REDIS_URL;
  if (!redisUrl) {
    throw new Error("REDIS_URL is required");
  }
  const keyPrefix = process.env.REDIS_KEY_PREFIX || "";
  const sessionPrefix = `${keyPrefix}session:`;
  const indexKeys = sessionMetricsIndexKeys(keyPrefix);
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    enableOfflineQueue: false,
    maxRetriesPerRequest: 1
  });
  const summary = {
    ok: false,
    mode: options.apply ? "apply" : "dry-run",
    scanned: 0,
    eligible: 0,
    invalid: 0,
    expired: 0,
    indexed_sessions: 0,
    indexed_players: 0,
    indexed_activity: 0
  };

  try {
    await redis.connect();
    let cursor = "0";
    do {
      const [nextCursor, keys] = await redis.scan(
        cursor,
        "MATCH",
        `${sessionPrefix}*`,
        "COUNT",
        options.batchSize
      );
      cursor = nextCursor;
      summary.scanned += keys.length;
      if (keys.length === 0) {
        if (options.delayMs > 0 && cursor !== "0") {
          await delay(options.delayMs);
        }
        continue;
      }

      const readPipeline = redis.pipeline();
      for (const key of keys) {
        const token = key.slice(sessionPrefix.length);
        readPipeline.get(key);
        readPipeline.ttl(key);
        readPipeline.get(`${keyPrefix}session-activity:${token}`);
      }
      const readResults = await readPipeline.exec();
      const eligible = [];
      for (let offset = 0; offset < keys.length; offset += 1) {
        const key = keys[offset];
        const token = key.slice(sessionPrefix.length);
        const raw = pipelineValue(readResults[offset * 3]);
        const ttl = Number(pipelineValue(readResults[offset * 3 + 1]));
        const activityAtMs = Number(pipelineValue(readResults[offset * 3 + 2]));
        if (!raw || !Number.isSafeInteger(ttl) || ttl <= 0) {
          summary.expired += 1;
          continue;
        }
        try {
          const session = JSON.parse(raw);
          if (!token || typeof session.playerId !== "string" || !session.playerId) {
            throw new Error("invalid session");
          }
          eligible.push({ token, playerId: session.playerId, ttl, activityAtMs });
          summary.eligible += 1;
        } catch {
          summary.invalid += 1;
        }
      }

      if (!options.apply || eligible.length === 0) {
        if (options.delayMs > 0 && cursor !== "0") {
          await delay(options.delayMs);
        }
        continue;
      }
      const nowSeconds = Math.floor(Date.now() / 1000);
      const writePipeline = redis.pipeline();
      for (const session of eligible) {
        const expiresAt = nowSeconds + session.ttl;
        writePipeline.zadd(indexKeys.sessions, expiresAt, session.token);
        writePipeline.eval(
          ZADD_MAX_SCORE_LUA,
          1,
          indexKeys.players,
          expiresAt,
          session.playerId
        );
        summary.indexed_sessions += 1;
        summary.indexed_players += 1;
        const activityAtSeconds = Math.floor(session.activityAtMs / 1000);
        if (Number.isSafeInteger(activityAtSeconds) && activityAtSeconds > nowSeconds - 300) {
          writePipeline.zadd(indexKeys.activity, activityAtSeconds, session.token);
          summary.indexed_activity += 1;
        }
      }
      const writeResults = await writePipeline.exec();
      const writeError = writeResults.find((result) => Array.isArray(result) && result[0]);
      if (writeError) {
        throw writeError[0];
      }
      if (options.delayMs > 0 && cursor !== "0") {
        await delay(options.delayMs);
      }
    } while (cursor !== "0");

    summary.ok = true;
    console.log(JSON.stringify(summary));
  } finally {
    redis.disconnect();
  }
}

function pipelineValue(result) {
  if (Array.isArray(result)) {
    if (result[0]) {
      throw result[0];
    }
    return result[1];
  }
  return result;
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

run().catch((error) => {
  console.error(JSON.stringify({ ok: false, error: error.message }));
  process.exitCode = 1;
});
