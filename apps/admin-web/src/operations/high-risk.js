function responseData(response) {
  return response?.data && typeof response.data === "object" ? response.data : response;
}

export function createAdminRequestId(prefix = "admin-web") {
  const randomId = globalThis.crypto?.randomUUID?.();
  if (randomId) return `${prefix}-${randomId}`;
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export function highRiskState(response) {
  const body = responseData(response) || {};
  const state = body.state;
  if (state === "preflight" || state === "preflighted") return "preflight";
  if (state === "in_progress") return "in_progress";
  if (state === "terminal") return "terminal";
  if (body.ok === false) return "failed";
  return "succeeded";
}

export function preflightDetails(response) {
  const body = responseData(response) || {};
  const preflight = body.preflight;
  if (!preflight || typeof preflight !== "object" ||
      typeof preflight.nonce !== "string" || !preflight.nonce ||
      typeof preflight.summarySha256 !== "string" || !preflight.summarySha256) {
    throw new Error("ADMIN_OPERATION_PREFLIGHT_INVALID");
  }
  return {
    operation: body.operation || null,
    nonce: preflight.nonce,
    summarySha256: preflight.summarySha256,
    expiresAt: preflight.expiresAt || null,
    impactSummary: preflight.impactSummary || {},
    approvalStatus: preflight.approvalStatus || body.operation?.approvalStatus || "not_required"
  };
}

export function formatHighRiskPreview(preflight) {
  const lines = [];
  if (preflight.operation?.requestId) lines.push(`请求 ID：${preflight.operation.requestId}`);
  if (preflight.expiresAt) lines.push(`确认有效期至：${new Date(preflight.expiresAt).toLocaleString("zh-CN")}`);
  if (preflight.approvalStatus && preflight.approvalStatus !== "not_required") {
    lines.push(`审批状态：${preflight.approvalStatus}`);
  }
  const impact = preflight.impactSummary;
  if (impact && Object.keys(impact).length > 0) {
    lines.push(`影响预览：${JSON.stringify(impact)}`);
  }
  return lines.join("\n") || "服务端未返回额外影响摘要。";
}

export async function runHighRiskOperation({
  invoke,
  payload,
  requestId = createAdminRequestId(),
  confirm
}) {
  if (typeof invoke !== "function") throw new TypeError("invoke is required");

  const basePayload = { ...payload, requestId };
  const initialResponse = await invoke(basePayload);
  const initialState = highRiskState(initialResponse);
  if (initialState !== "preflight") {
    return { phase: initialState, requestId, response: responseData(initialResponse) };
  }

  const preflight = preflightDetails(initialResponse);
  const accepted = await confirm?.(preflight);
  if (accepted !== true) {
    return { phase: "cancelled", requestId, preflight };
  }

  const executionResponse = await invoke({
    ...basePayload,
    preflightNonce: preflight.nonce,
    preflightSummarySha256: preflight.summarySha256
  });
  return {
    phase: highRiskState(executionResponse),
    requestId,
    preflight,
    response: responseData(executionResponse)
  };
}
