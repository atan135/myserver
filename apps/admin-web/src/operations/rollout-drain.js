const INSTANCE_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/;
const SENSITIVE_REASON_PATTERN = /\b(?:password|passwd|pwd|token|secret|api[-_]?key|private[-_]?key|authorization|cookie|ticket|session(?:[-_]?id)?)\b\s*(?:=|:)|\b(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}\b|-----BEGIN(?: [A-Z0-9]+)* PRIVATE KEY-----|\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/i;

function asInstanceId(value) {
  const normalized = typeof value === "string" ? value.trim() : "";
  return INSTANCE_ID_PATTERN.test(normalized) ? normalized : "";
}

export function gameServerInstances(registryData) {
  const service = Array.isArray(registryData?.services)
    ? registryData.services.find((item) => item?.name === "game-server")
    : null;
  const instances = Array.isArray(service?.instances) ? service.instances : [];
  const seen = new Set();
  return instances
    .map((instance) => {
      const instanceId = asInstanceId(instance?.instance_id ?? instance?.instanceId);
      if (!instanceId || seen.has(instanceId)) return null;
      seen.add(instanceId);
      return {
        instanceId,
        status: instance.status || instance.registry_state || "unknown",
        healthy: instance.healthy !== false,
        metricsState: instance.metrics_state || "missing"
      };
    })
    .filter((instance) => instance && instance.healthy && ["healthy", "degraded"].includes(instance.status))
    .sort((left, right) => left.instanceId.localeCompare(right.instanceId));
}

export function selectDefaultGameServerInstance(instances) {
  return (Array.isArray(instances) ? instances : []).find((instance) =>
    instance.healthy && ["healthy", "degraded"].includes(instance.status)
  )?.instanceId || "";
}

export function isSafeDrainReason(value) {
  const reason = typeof value === "string" ? value.trim() : "";
  return Boolean(reason) && reason.length <= 256 && !SENSITIVE_REASON_PATTERN.test(reason);
}

export function gameServerMetric(servicesData, instanceId) {
  const service = Array.isArray(servicesData?.services)
    ? servicesData.services.find((item) => item?.name === "game-server")
    : null;
  const instance = Array.isArray(service?.instances)
    ? service.instances.find((item) => asInstanceId(item?.instance_id ?? item?.instanceId) === instanceId)
    : null;
  if (!instance) return { available: false, onlineValue: null, status: "missing" };
  const onlineValue = Number.isFinite(Number(instance.online_value)) ? Number(instance.online_value) : null;
  return {
    available: onlineValue !== null,
    onlineValue,
    status: instance.status || "unknown",
    metricsState: instance.metrics_state || "missing",
    reportedAt: instance.last_reported_at || instance.last_reported || null
  };
}

export function normalizeRouteBlockers(rolloutData) {
  const blockers = rolloutData?.blockers;
  if (!blockers || typeof blockers !== "object") return null;
  return {
    available: true,
    blockedRoomCount: Number(blockers.blocked_room_count) || 0,
    blockedPlayerCount: Number(blockers.blocked_player_count) || 0,
    staleRoomRouteCount: Number(blockers.stale_room_route_count) || 0,
    stalePlayerRouteCount: Number(blockers.stale_player_route_count) || 0
  };
}

export function normalizeRolloutObservation({ services, rollout, instanceId }) {
  const metric = gameServerMetric(services, instanceId);
  return {
    instanceId,
    metric,
    routeBlockers: normalizeRouteBlockers(rollout),
    controlPlane: {
      available: false,
      message: "控制面状态待接入"
    }
  };
}

export function normalizeDrainStatus(data, instanceId) {
  const value = data && typeof data === "object" ? data : {};
  return {
    available: value.ok === true,
    instanceId,
    errorCode: value.errorCode || "",
    connectionCount: Number.isFinite(Number(value.connectionCount)) ? Number(value.connectionCount) : null,
    ownedRoomCount: Number.isFinite(Number(value.ownedRoomCount)) ? Number(value.ownedRoomCount) : null,
    migratingRoomCount: Number.isFinite(Number(value.migratingRoomCount)) ? Number(value.migratingRoomCount) : null,
    drainModeEnabled: typeof value.drainModeEnabled === "boolean" ? value.drainModeEnabled : null,
    retiredRoomCount: Number.isFinite(Number(value.retiredRoomCount)) ? Number(value.retiredRoomCount) : null,
    transferableEmptyRoomCount: Number.isFinite(Number(value.transferableEmptyRoomCount))
      ? Number(value.transferableEmptyRoomCount)
      : null,
    routeCount: Number.isFinite(Number(value.routeCount)) ? Number(value.routeCount) : null,
    transferableEmptyRoomSampleCount: Number.isFinite(Number(value.transferableEmptyRoomSampleCount))
      ? Number(value.transferableEmptyRoomSampleCount)
      : null,
    drainModeReason: typeof value.drainModeReason === "string" ? value.drainModeReason : "",
    drainModeSource: typeof value.drainModeSource === "string" ? value.drainModeSource : ""
  };
}
