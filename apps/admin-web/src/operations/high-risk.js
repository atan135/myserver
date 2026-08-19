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
  const operationStatus = body.operation?.status || body.status;
  if (operationStatus === "execution_uncertain") return "execution_uncertain";
  if (operationStatus === "failed" || operationStatus === "cancelled") return "failed";
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

const ERROR_MESSAGES = Object.freeze({
  permission_denied: {
    title: "无权限执行",
    description: "当前账号或授权范围不允许执行此操作。"
  },
  approval_required: {
    title: "等待独立审批",
    description: "该高风险操作需要其他管理员审批后才能执行。"
  },
  preflight_expired: {
    title: "预检已过期",
    description: "影响预览已过期，请重新发起预检。"
  },
  nonce_replayed: {
    title: "确认凭据已使用",
    description: "该预检确认凭据不可重复使用，请重新发起预检。"
  },
  execution_uncertain: {
    title: "执行结果待核实",
    description: "服务端无法确认最终结果，请以审计和监控状态为准，勿直接重试。"
  },
  request_conflict: {
    title: "请求标识冲突",
    description: "同一请求 ID 已绑定其他操作，请使用新的请求 ID。"
  },
  failed: {
    title: "操作失败",
    description: "服务端拒绝了该操作，请查看错误详情。"
  }
});

function operationErrorCode(error) {
  const data = error?.response?.data;
  return typeof data?.error === "string" ? data.error : "";
}

function isApprovalRequiredError(error) {
  return operationErrorCode(error) === "ADMIN_OPERATION_APPROVAL_REQUIRED";
}

export function classifyHighRiskError(error) {
  const code = operationErrorCode(error);
  const status = error?.response?.status;
  if (status === 403 || code === "ADMIN_OPERATION_PERMISSION_DENIED" || code === "ADMIN_OPERATION_SCOPE_DENIED") {
    return "permission_denied";
  }
  if (code === "ADMIN_OPERATION_APPROVAL_REQUIRED") return "approval_required";
  if (code === "ADMIN_OPERATION_PREVIEW_EXPIRED") return "preflight_expired";
  if (code === "ADMIN_OPERATION_NONCE_REPLAYED") return "nonce_replayed";
  if (code === "ADMIN_OPERATION_REQUEST_CONFLICT") return "request_conflict";
  if (code === "ADMIN_OPERATION_PERSISTENCE_FAILED" || code === "ADMIN_OPERATION_RESULT_PERSISTENCE_FAILED") {
    return "execution_uncertain";
  }
  return "failed";
}

export function normalizeHighRiskError(error) {
  const kind = classifyHighRiskError(error);
  const fallback = ERROR_MESSAGES[kind] || ERROR_MESSAGES.failed;
  const serverMessage = typeof error?.response?.data?.message === "string"
    ? error.response.data.message.trim()
    : "";
  return {
    kind,
    code: operationErrorCode(error) || null,
    title: fallback.title,
    description: fallback.description,
    serverMessage: serverMessage || null,
    status: error?.response?.status || null
  };
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
  if (preflight.expiresAt && Number.isFinite(Date.parse(preflight.expiresAt)) && Date.parse(preflight.expiresAt) <= Date.now()) {
    return { phase: "expired", requestId, preflight };
  }
  const accepted = await confirm?.(preflight);
  if (accepted !== true) {
    return { phase: "cancelled", requestId, preflight };
  }

  let executionResponse;
  try {
    executionResponse = await invoke({
      ...basePayload,
      preflightNonce: preflight.nonce,
      preflightSummarySha256: preflight.summarySha256
    });
  } catch (error) {
    if (isApprovalRequiredError(error)) {
      return { phase: "approval_required", requestId, preflight, response: responseData(error?.response) };
    }
    throw error;
  }
  return {
    phase: highRiskState(executionResponse),
    requestId,
    preflight,
    response: responseData(executionResponse)
  };
}

export async function resumeHighRiskOperation({ invoke, payload, requestId, preflight }) {
  if (typeof invoke !== "function") throw new TypeError("invoke is required");
  if (!preflight?.nonce || !preflight?.summarySha256 || !requestId) {
    throw new Error("ADMIN_OPERATION_PREFLIGHT_INVALID");
  }
  try {
    const response = await invoke({
      ...payload,
      requestId,
      preflightNonce: preflight.nonce,
      preflightSummarySha256: preflight.summarySha256
    });
    return { phase: highRiskState(response), requestId, preflight, response: responseData(response) };
  } catch (error) {
    if (isApprovalRequiredError(error)) {
      return { phase: "approval_required", requestId, preflight, response: responseData(error?.response) };
    }
    throw error;
  }
}
