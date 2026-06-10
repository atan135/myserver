const SUM_FIELDS = new Set([
  "qps",
  "online_sessions",
  "unique_players",
  "active_sessions_5m",
  "online_players",
  "connections",
  "pool_size",
  "room_count"
]);

const MAX_FIELDS = new Set(["latency_ms"]);

export function parseMetricInt(value) {
  return parseInt(value || "0", 10);
}

export function parseMetricKey(serviceName, key) {
  const prefix = `metrics:${serviceName}:`;
  if (!key.startsWith(prefix)) {
    return null;
  }

  const rest = key.slice(prefix.length).split(":");
  if (rest.length !== 1 && rest.length !== 2) {
    return null;
  }

  const bucket = parseInt(rest[rest.length - 1], 10);
  if (!Number.isFinite(bucket)) {
    return null;
  }

  return {
    bucket,
    instanceId: rest.length === 2 ? rest[0] : null,
    legacy: rest.length === 1
  };
}

export function getOnlineValue(serviceName, data, serviceConfigs) {
  const onlineField = serviceConfigs[serviceName]?.onlineField;
  if (!onlineField) {
    return 0;
  }

  return parseMetricInt(data[onlineField]);
}

function mergeMetricValue(target, key, value) {
  if (value === undefined || value === null) {
    return;
  }

  if (SUM_FIELDS.has(key)) {
    target[key] = String(parseMetricInt(target[key]) + parseMetricInt(value));
    return;
  }

  if (MAX_FIELDS.has(key)) {
    target[key] = String(Math.max(parseMetricInt(target[key]), parseMetricInt(value)));
    return;
  }

  if (target[key] === undefined) {
    target[key] = String(value);
  }
}

export function aggregateMetricRecords(records) {
  const merged = {};
  const instances = new Set();

  for (const record of records) {
    const data = record?.data || {};
    const instanceId = record?.instanceId || data.instance_id;
    if (instanceId) {
      instances.add(String(instanceId));
    }

    for (const [key, value] of Object.entries(data)) {
      if (key === "instance_id") {
        continue;
      }
      mergeMetricValue(merged, key, value);
    }
  }

  if (instances.size > 0) {
    merged.instance_ids = [...instances].sort().join(",");
    merged.instance_count = String(instances.size);
  }

  return merged;
}

export function buildMetricPoint(serviceName, data, serviceConfigs, bucket) {
  const point = {
    timestamp: bucket,
    qps: parseMetricInt(data.qps),
    latency_ms: parseMetricInt(data.latency_ms),
    online_value: getOnlineValue(serviceName, data, serviceConfigs),
    online_sessions: parseMetricInt(data.online_sessions),
    unique_players: parseMetricInt(data.unique_players),
    active_sessions_5m: parseMetricInt(data.active_sessions_5m)
  };

  if (data.instance_count !== undefined) {
    point.instance_count = parseMetricInt(data.instance_count);
  }

  return point;
}
