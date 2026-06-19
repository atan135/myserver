const SUM_FIELDS = new Set([
  "qps",
  "online_sessions",
  "unique_players",
  "active_sessions_5m",
  "online_players",
  "connections",
  "pool_size",
  "room_count",
  "registry_scan_total",
  "registry_scan_duration_ms_total",
  "registry_scan_instance_keys_total",
  "registry_scan_visible_instances_total",
  "registry_discovery_cache_hit_total",
  "registry_discovery_cache_miss_total",
  "register_failed_total",
  "heartbeat_failed_total",
  "deregister_failed_total"
]);

const MAX_FIELDS = new Set([
  "latency_ms",
  "registry_scan_duration_ms_last",
  "registry_scan_duration_ms_max",
  "registry_scan_instance_keys_last",
  "registry_scan_visible_instances_last",
  "registry_discovery_cache_hit_rate_basis_points"
]);

const NUMERIC_OUTPUT_FIELDS = new Set([
  ...SUM_FIELDS,
  ...MAX_FIELDS,
  "active_window_seconds",
  "instance_count"
]);

const LEGACY_INSTANCE_ID = "legacy";

export function parseMetricInt(value) {
  const parsed = parseInt(value || "0", 10);
  return Number.isFinite(parsed) ? parsed : 0;
}

export function parseMetricKey(serviceName, key) {
  const prefix = `metrics:${serviceName}:`;
  if (!key.startsWith(prefix)) {
    return null;
  }

  const rest = key.slice(prefix.length).split(":");
  if (rest.length < 1) {
    return null;
  }

  const bucket = parseInt(rest[rest.length - 1], 10);
  if (!Number.isFinite(bucket)) {
    return null;
  }

  return {
    bucket,
    instanceId: rest.length >= 2 ? rest.slice(0, -1).join(":") : null,
    legacy: rest.length === 1
  };
}

export function parseMetricHeartbeatKey(serviceName, key) {
  const prefix = `metrics:heartbeat:${serviceName}:`;
  if (!key.startsWith(prefix)) {
    return null;
  }

  const instanceId = key.slice(prefix.length);
  if (!instanceId) {
    return null;
  }

  return { instanceId };
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

function metricRecordInstanceId(record) {
  const data = record?.data || {};
  const instanceId = record?.instanceId || data.instance_id;
  if (instanceId) {
    return String(instanceId);
  }

  return record?.legacy ? LEGACY_INSTANCE_ID : null;
}

function normalizeMetricData(data) {
  const normalized = {};

  for (const [key, value] of Object.entries(data || {})) {
    if (key === "instance_id" || value === undefined || value === null) {
      continue;
    }
    normalized[key] = String(value);
  }

  return normalized;
}

function coerceMetricOutput(data) {
  const output = { ...data };
  for (const field of NUMERIC_OUTPUT_FIELDS) {
    if (output[field] !== undefined) {
      output[field] = parseMetricInt(output[field]);
    }
  }

  output.qps = parseMetricInt(data.qps);
  output.latency_ms = parseMetricInt(data.latency_ms);
  return output;
}

export function aggregateMetricRecordsDetailed(records) {
  const merged = {};
  const instances = new Set();
  const instanceRecords = [];

  for (const record of records) {
    const data = record?.data || {};
    const instanceId = metricRecordInstanceId(record);
    const metricData = normalizeMetricData(data);

    if (instanceId && instanceId !== LEGACY_INSTANCE_ID) {
      instances.add(instanceId);
    }

    if (instanceId) {
      instanceRecords.push({
        instance_id: instanceId,
        legacy: record?.legacy === true,
        data: metricData
      });
    }

    for (const [key, value] of Object.entries(metricData)) {
      mergeMetricValue(merged, key, value);
    }
  }

  if (instances.size > 0) {
    merged.instance_ids = [...instances].sort().join(",");
    merged.instance_count = String(instances.size);
  }

  instanceRecords.sort((a, b) => {
    if (a.legacy !== b.legacy) {
      return a.legacy ? 1 : -1;
    }
    return a.instance_id.localeCompare(b.instance_id);
  });

  return {
    data: merged,
    instances: instanceRecords
  };
}

export function aggregateMetricRecords(records) {
  return aggregateMetricRecordsDetailed(records).data;
}

export function buildInstanceMetricPoint(serviceName, instance, serviceConfigs) {
  const data = instance?.data || {};
  const point = {
    instance_id: instance?.instance_id || LEGACY_INSTANCE_ID,
    ...coerceMetricOutput(data),
    online_value: getOnlineValue(serviceName, data, serviceConfigs),
    online_sessions: parseMetricInt(data.online_sessions),
    unique_players: parseMetricInt(data.unique_players),
    active_sessions_5m: parseMetricInt(data.active_sessions_5m)
  };

  if (instance?.legacy) {
    point.legacy = true;
  }

  return point;
}

export function buildMetricPoint(serviceName, data, serviceConfigs, bucket, instances = []) {
  const point = {
    timestamp: bucket,
    ...coerceMetricOutput(data),
    online_value: getOnlineValue(serviceName, data, serviceConfigs),
    online_sessions: parseMetricInt(data.online_sessions),
    unique_players: parseMetricInt(data.unique_players),
    active_sessions_5m: parseMetricInt(data.active_sessions_5m)
  };

  if (data.instance_count !== undefined) {
    point.instance_count = parseMetricInt(data.instance_count);
  }

  if (instances.length > 0) {
    point.instances = instances.map((instance) =>
      buildInstanceMetricPoint(serviceName, instance, serviceConfigs)
    );
  }

  return point;
}
