export const REDIRECT_JOIN_FALLBACK_ERROR_CODES = new Set([
  "ROOM_NOT_FOUND",
  "PLAYER_NOT_IN_ROOM",
  "PLAYER_NOT_FOUND",
  "PLAYER_NOT_OFFLINE"
]);

export function validateServerRedirectPush(redirect, expectedRoomId = "") {
  if (!redirect || typeof redirect !== "object") {
    throw new Error("server redirect push is empty");
  }
  if (expectedRoomId && redirect.roomId !== expectedRoomId) {
    throw new Error(`redirect room mismatch: expected ${expectedRoomId}, got ${redirect.roomId}`);
  }
  if (!redirect.reconnectRequired) {
    throw new Error("redirect push did not require reconnect");
  }
  if (!redirect.targetHost || !redirect.targetPort) {
    throw new Error("redirect push missing target host or port");
  }
}

export function buildRedirectReconnectOptions(baseOptions, redirect) {
  validateServerRedirectPush(redirect, baseOptions?.roomId || "");

  return {
    ...baseOptions,
    host: redirect.targetHost,
    gameHost: redirect.targetHost,
    port: redirect.targetPort
  };
}

export function shouldFallbackToJoin(reconnectRes, options = {}) {
  if (reconnectRes?.ok) {
    return false;
  }
  if (!options.allowRedirectJoinFallback) {
    return false;
  }
  return REDIRECT_JOIN_FALLBACK_ERROR_CODES.has(reconnectRes?.errorCode || "");
}

export function summarizeRedirectReconnectResult({
  login,
  redirect,
  reconnectRes,
  joinRes = null,
  finalMode
}) {
  const finalResponse = finalMode === "join" ? joinRes : reconnectRes;

  return {
    ok: true,
    accountPlayerId: login.playerId,
    characterId: login.characterId,
    redirect: {
      reason: redirect.reason,
      roomId: redirect.roomId,
      rolloutEpoch: redirect.rolloutEpoch,
      reconnectRequired: redirect.reconnectRequired,
      retryAfterMs: redirect.retryAfterMs,
      targetHost: redirect.targetHost,
      targetPort: redirect.targetPort,
      targetServerId: redirect.targetServerId,
      transport: redirect.transport
    },
    reconnect: reconnectRes,
    join: joinRes,
    finalMode,
    finalRoomId: finalResponse?.roomId || redirect.roomId
  };
}
