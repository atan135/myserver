import { Inject, Injectable } from "@nestjs/common";

import { ApiHttpException } from "../common/http-exception.js";
import { ADMIN_BREAKGLASS, ADMIN_OPERATIONS } from "../tokens.js";
import { AdminPolicyScopeRequest } from "../auth/admin-policy.service.js";
import { containsSensitiveAuditReason } from "./audit-reason.js";

type ProtocolActor = {
  adminId: number | string;
  subject: string;
};

type HighRiskOperationInput<T> = {
  request: any;
  permission: string;
  scope: AdminPolicyScopeRequest;
  targetSummary: Record<string, unknown>;
  payload: unknown;
  impactSummary: Record<string, unknown>;
  reason: string;
  emergency?: boolean;
  failureStatus?: "failed" | "execution_uncertain";
  execute: () => Promise<T>;
  resultSummary?: (result: T) => Record<string, unknown>;
};

type ProtocolResult<T> =
  | { state: "preflight" | "in_progress" | "terminal"; response: Record<string, unknown> }
  | { state: "executed"; result: T };

function protocolError(code: string, statusCode = 400) {
  return new ApiHttpException(statusCode, {
    ok: false,
    error: code,
    message: "High-risk operation rejected"
  });
}

function errorCode(error: any) {
  return typeof error?.code === "string" ? error.code : "ADMIN_OPERATION_FAILED";
}

function protocolStatusForCode(code: string) {
  if (code === "ADMIN_OPERATION_PERSISTENCE_FAILED") {
    return 503;
  }
  if (code === "ADMIN_OPERATION_PERMISSION_DENIED" || code === "ADMIN_OPERATION_SCOPE_DENIED" ||
      code === "ADMIN_BREAKGLASS_ACTIVATE_DENIED" || code === "ADMIN_BREAKGLASS_GRANT_REQUIRED") {
    return 403;
  }
  if (code === "ADMIN_OPERATION_REQUEST_CONFLICT" || code === "ADMIN_OPERATION_STATE_CONFLICT" ||
      code === "ADMIN_OPERATION_NONCE_REPLAYED" || code === "ADMIN_OPERATION_APPROVAL_REQUIRED" ||
      code === "ADMIN_OPERATION_APPROVAL_REJECTED") {
    return 409;
  }
  return 400;
}

function protocolCode(error: any) {
  const code = errorCode(error);
  return code === "ADMIN_OPERATION_FAILED" ? "ADMIN_OPERATION_PERSISTENCE_FAILED" : code;
}

function optionalText(value: unknown) {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function actorFromRequest(request: any): ProtocolActor {
  const adminId = request?.admin?.sub;
  if (adminId === undefined || adminId === null || String(adminId).trim() === "") {
    throw protocolError("ADMIN_OPERATION_ACTOR_REQUIRED", 401);
  }
  return {
    adminId,
    subject: `admin:${String(adminId).trim()}`
  };
}

function protocolFields(request: any) {
  const body = request?.body && typeof request.body === "object" && !Array.isArray(request.body)
    ? request.body
    : {};
  return {
    requestId: optionalText(body.requestId ?? body.request_id),
    nonce: optionalText(body.preflightNonce ?? body.preflight_nonce),
    summarySha256: optionalText(body.preflightSummarySha256 ?? body.preflight_summary_sha256)
  };
}

function operationView(operation: any) {
  return {
    operationId: operation?.operationId || null,
    requestId: operation?.requestId || null,
    status: operation?.status || null,
    approvalStatus: operation?.approvalStatus || null,
    resultSummary: operation?.resultSummary || null,
    errorSummary: operation?.errorSummary || null
  };
}

function persistenceFailure() {
  return protocolError("ADMIN_OPERATION_PERSISTENCE_FAILED", 503);
}

@Injectable()
export class AdminHighRiskOperationService {
  constructor(
    @Inject(ADMIN_OPERATIONS) private readonly operations: any,
    @Inject(ADMIN_BREAKGLASS) private readonly breakglass: any
  ) {}

  private async recordExecutionUncertain(operationId: string, code: string, phase: string) {
    try {
      const errorSummary = { code };
      const details = { phase, recovery: "reconciliation_required" };
      if (typeof this.operations.markExecutionUncertain === "function") {
        await this.operations.markExecutionUncertain({ operationId, errorSummary });
        return true;
      }
      await this.operations.completeExecution({
        operationId,
        status: "execution_uncertain",
        errorSummary,
        details
      });
      return true;
    } catch {
      return false;
    }
  }

  async run<T>(input: HighRiskOperationInput<T>): Promise<ProtocolResult<T>> {
    const actor = actorFromRequest(input.request);
    const fields = protocolFields(input.request);
    if (!fields.requestId) {
      throw protocolError("ADMIN_OPERATION_REQUEST_ID_REQUIRED");
    }
    if (typeof input.reason !== "string" || !input.reason.trim()) {
      throw protocolError("ADMIN_OPERATION_REASON_REQUIRED");
    }
    if (containsSensitiveAuditReason(input.reason)) {
      throw protocolError("ADMIN_OPERATION_SENSITIVE_REASON");
    }

    const base = {
      actor,
      permission: input.permission,
      scope: input.scope,
      requestId: fields.requestId,
      reason: input.reason,
      targetSummary: input.targetSummary,
      payload: input.payload
    };
    const hasNonce = fields.nonce !== null;
    const hasSummary = fields.summarySha256 !== null;
    if (!hasNonce && !hasSummary) {
      try {
        const preflight = await this.operations.preflight({
          ...base,
          impactSummary: input.impactSummary
        });
        return {
          state: "preflight",
          response: {
            ok: true,
            operation: operationView(preflight.operation),
            preflight: preflight.preflight,
            state: preflight.state
          }
        };
      } catch (error: any) {
        const code = protocolCode(error);
        throw protocolError(code, protocolStatusForCode(code));
      }
    }
    if (!hasNonce || !hasSummary) {
      throw protocolError("ADMIN_OPERATION_PREVIEW_REQUIRED");
    }

    if (input.emergency === true) {
      try {
        await this.breakglass.requireActiveGrant({
          actorAdminId: actor.adminId,
          permission: input.permission,
          scope: input.scope,
          targetSummary: input.targetSummary
        });
      } catch (error: any) {
        throw protocolError(errorCode(error), protocolStatusForCode(errorCode(error)));
      }
    }

    let claim: any;
    try {
      claim = await this.operations.claimExecution({
        ...base,
        nonce: fields.nonce,
        preflightSummarySha256: fields.summarySha256
      });
    } catch (error: any) {
      const code = protocolCode(error);
      throw protocolError(code, protocolStatusForCode(code));
    }
    if (claim.state === "terminal" || claim.state === "in_progress") {
      return {
        state: claim.state,
        response: {
          ok: true,
          operation: operationView(claim.operation),
          state: claim.state
        }
      };
    }
    if (claim.state !== "claimed") {
      throw protocolError("ADMIN_OPERATION_STATE_CONFLICT", 409);
    }

    let result: T;
    try {
      result = await input.execute();
    } catch (error: any) {
      try {
        await this.operations.completeExecution({
          operationId: claim.operation.operationId,
          status: input.failureStatus || "execution_uncertain",
          errorSummary: { code: errorCode(error) },
          details: { phase: "handler" }
        });
      } catch {
        await this.recordExecutionUncertain(
          claim.operation.operationId,
          "ADMIN_OPERATION_HANDLER_PERSISTENCE_FAILED",
          "handler_completion"
        );
        throw persistenceFailure();
      }
      throw error;
    }

    try {
      const partialFailure = result && typeof result === "object" && (result as any).ok === false;
      await this.operations.completeExecution({
        operationId: claim.operation.operationId,
        status: partialFailure ? "execution_uncertain" : "succeeded",
        resultSummary: partialFailure ? null : input.resultSummary ? input.resultSummary(result) : { outcome: "succeeded" },
        errorSummary: partialFailure ? { code: typeof (result as any).error === "string" ? (result as any).error : "ADMIN_OPERATION_PARTIAL_FAILURE" } : null,
        details: { phase: "handler" }
      });
    } catch {
      await this.recordExecutionUncertain(
        claim.operation.operationId,
        "ADMIN_OPERATION_RESULT_PERSISTENCE_FAILED",
        "result_completion"
      );
      throw persistenceFailure();
    }
    return { state: "executed", result };
  }
}
