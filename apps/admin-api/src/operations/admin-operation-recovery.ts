import { ApiHttpException } from "../common/http-exception.js";

type RecoveryPlan = {
  classification: "reversible" | "irreversible";
  command: Record<string, unknown> | null;
  backup: { required: boolean; reference: string | null };
  recoveryConditions: string[];
  riskSummary: string;
};

function recoveryError(code: string) {
  return new ApiHttpException(400, {
    ok: false,
    error: code,
    message: "High-risk operation recovery evidence is required"
  });
}

function backupReference(request: any) {
  const body = request?.body && typeof request.body === "object" && !Array.isArray(request.body) ? request.body : {};
  const value = body.backupReference ?? body.backup_reference;
  const normalized = typeof value === "string" ? value.trim() : "";
  if (!/^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/.test(normalized)) {
    throw recoveryError("ADMIN_OPERATION_BACKUP_REFERENCE_REQUIRED");
  }
  return normalized;
}

function reversible(command: Record<string, unknown>, conditions: string[], riskSummary: string): RecoveryPlan {
  return {
    classification: "reversible",
    command: { ...command, createsNewOperation: true, preservesOriginalAudit: true },
    backup: { required: false, reference: null },
    recoveryConditions: conditions,
    riskSummary
  };
}

function irreversible(request: any, conditions: string[], riskSummary: string): RecoveryPlan {
  return {
    classification: "irreversible",
    command: null,
    backup: { required: true, reference: backupReference(request) },
    recoveryConditions: conditions,
    riskSummary
  };
}

export function highRiskRecoveryPlan(permission: string, request: any): RecoveryPlan {
  switch (permission) {
    case "maintenance.write":
      return reversible(
        { permission: "maintenance.write", route: "/api/v1/maintenance", method: "POST", body: { enabled: false } },
        ["A new preflight, approval decision and request_id are required before disabling maintenance."],
        "Maintenance changes player admission globally until a separate disable command succeeds."
      );
    case "players.ban":
      return reversible(
        { permission: "players.status.update", route: "/api/v1/players/:playerId/status", method: "PUT", body: { status: "active" } },
        ["The player record must still be banned and the compensating request is separately authorized."],
        "The player is blocked until a separately audited status change is accepted."
      );
    case "myforge.task.create":
      return reversible(
        { permission: "myforge.task.cancel", route: "/api/v1/myforge/tasks/:requestId/cancel", method: "POST", body: {} },
        ["Only queued, dispatched or running tasks can be cancelled; a running agent must acknowledge cancellation."],
        "Generated artifacts may already exist when cancellation is requested and require manual review."
      );
    default:
      return irreversible(
        request,
        ["The backup reference must be reviewed before execution.", "Recovery requires a new authorized operation; historical audit records remain immutable."],
        "This action can affect player state, credentials or externally delivered content and has no automatic rollback."
      );
  }
}
