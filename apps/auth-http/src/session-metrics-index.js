export const SESSION_ACTIVITY_WINDOW_SECONDS = 300;

export function sessionMetricsIndexKeys(redisKeyPrefix = "") {
  const prefix = String(redisKeyPrefix || "");
  return {
    sessions: `${prefix}auth-index:sessions:expires`,
    players: `${prefix}auth-index:players:expires`,
    activity: `${prefix}auth-index:activity:last-seen`
  };
}

export async function readSessionMetricsIndex(redis, redisKeyPrefix = "", nowMs = Date.now()) {
  const nowSeconds = Math.floor(nowMs / 1000);
  const activityCutoff = nowSeconds - SESSION_ACTIVITY_WINDOW_SECONDS;
  const keys = sessionMetricsIndexKeys(redisKeyPrefix);
  const pipeline = redis.pipeline();
  pipeline.zremrangebyscore(keys.sessions, "-inf", nowSeconds);
  pipeline.zremrangebyscore(keys.players, "-inf", nowSeconds);
  pipeline.zremrangebyscore(keys.activity, "-inf", activityCutoff);
  pipeline.zcard(keys.sessions);
  pipeline.zcard(keys.players);
  pipeline.zcard(keys.activity);

  const results = await pipeline.exec();
  const values = results.map((result) => pipelineValue(result));
  const error = values.find((result) => result.error)?.error;
  if (error) {
    throw error;
  }

  return {
    onlineSessions: parseCount(values[3].value),
    uniquePlayers: parseCount(values[4].value),
    activeSessions5m: parseCount(values[5].value)
  };
}

function pipelineValue(result) {
  if (Array.isArray(result)) {
    return { error: result[0] || null, value: result[1] };
  }
  return { error: null, value: result };
}

function parseCount(value) {
  const count = Number.parseInt(String(value ?? "0"), 10);
  return Number.isSafeInteger(count) && count >= 0 ? count : 0;
}
