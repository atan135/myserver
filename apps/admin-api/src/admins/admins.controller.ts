import { Body, Controller, HttpCode, HttpStatus, Inject, Param, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { getClientIp } from "../common/client-ip.js";
import { ApiHttpException, badRequest, notFound } from "../common/http-exception.js";
import { ADMIN_CONFIG, ADMIN_HIGH_RISK_OPERATIONS, ADMIN_SESSION_STORE, ADMIN_STORE } from "../tokens.js";

const ADMIN_PASSWORD_MIN_LENGTH = 12;
const ADMIN_PASSWORD_MAX_LENGTH = 128;
const REASON_MAX_LENGTH = 512;

function parseAdminId(value: string): string {
  const adminId = String(value || "").trim();
  if (!/^[1-9]\d*$/.test(adminId)) {
    throw badRequest("INVALID_ADMIN_ID", "adminId must be a positive integer");
  }
  return adminId;
}

function normalizeReason(value: any): string {
  if (value === undefined || value === null) {
    throw badRequest("INVALID_REASON", "reason is required");
  }

  if (typeof value !== "string") {
    throw badRequest("INVALID_REASON", "reason must be a string");
  }

  const reason = value.trim();
  if (reason.length === 0) {
    throw badRequest("INVALID_REASON", "reason is required");
  }

  if (reason.length > REASON_MAX_LENGTH) {
    throw badRequest("INVALID_REASON", "reason must be no longer than 512 characters");
  }
  return reason;
}

function normalizeNewPassword(value: any): string {
  if (typeof value !== "string" || value.length === 0) {
    throw badRequest("INVALID_NEW_PASSWORD", "newPassword must be a non-empty string");
  }

  if (value.length < ADMIN_PASSWORD_MIN_LENGTH || value.length > ADMIN_PASSWORD_MAX_LENGTH) {
    throw badRequest("INVALID_NEW_PASSWORD", "newPassword must be between 12 and 128 characters");
  }

  if (/\s/.test(value)) {
    throw badRequest("INVALID_NEW_PASSWORD", "newPassword must not contain whitespace");
  }

  if (!/[a-z]/.test(value) || !/[A-Z]/.test(value) || !/\d/.test(value) || !/[^A-Za-z0-9]/.test(value)) {
    throw badRequest(
      "INVALID_NEW_PASSWORD",
      "newPassword must include uppercase, lowercase, number, and symbol characters"
    );
  }

  return value;
}

function rawTargetValue(value: any): string {
  const targetValue = String(value ?? "").trim();
  return targetValue || "unknown";
}

function errorCode(error: any, fallback: string): string {
  return error?.getResponse?.()?.error || fallback;
}

function toAdminSummary(admin: any) {
  return {
    id: admin.id,
    username: admin.username,
    displayName: admin.displayName,
    role: admin.role,
    status: admin.status
  };
}

@ApiTags("admins")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
@Controller("/api/v1/admins")
export class AdminsController {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_SESSION_STORE) private readonly sessionStore: any,
    @Inject(ADMIN_HIGH_RISK_OPERATIONS) private readonly highRiskOperations: any
  ) {}

  private async runHighRiskOperation(input: any) {
    if (typeof this.highRiskOperations?.run !== "function") {
      throw new ApiHttpException(503, {
        ok: false,
        error: "ADMIN_OPERATION_SERVICE_UNAVAILABLE",
        message: "High-risk operation service is unavailable"
      });
    }
    const outcome = await this.highRiskOperations.run(input);
    return outcome.state === "executed" ? outcome.result : outcome.response;
  }

  @Post(":adminId/revoke-tokens")
  @Permissions("admins.revoke_tokens")
  @HttpCode(HttpStatus.OK)
  async revokeTokens(@Param("adminId") adminIdParam: string, @Body() body: any, @Req() req: any) {
    let adminId: string;
    let reason = "";
    try {
      adminId = parseAdminId(adminIdParam);
      reason = normalizeReason(body?.reason);
    } catch (error) {
      await this.appendAdminTokenAudit(req, {
        action: "admin_tokens_revoke_failed",
        targetValue: rawTargetValue(adminIdParam),
        reason,
        result: "rejected",
        error: errorCode(error, "INVALID_REQUEST")
      });
      throw error;
    }

    const targetAdmin = await this.adminStore.findAdminById(adminId);
    if (!targetAdmin) {
      await this.appendAdminTokenAudit(req, {
        action: "admin_tokens_revoke_failed",
        targetValue: adminId,
        reason,
        result: "rejected",
        error: "ADMIN_NOT_FOUND"
      });
      throw notFound("ADMIN_NOT_FOUND", "Admin not found");
    }

    const currentTokenInvalidated = String(req.admin?.sub) === String(targetAdmin.id);
    return this.runHighRiskOperation({
      request: req,
      permission: "admins.revoke_tokens",
      scope: { targetType: "admin", targetIds: [String(targetAdmin.id)], targetCount: 1 },
      targetSummary: { targetType: "admin", targetIds: [String(targetAdmin.id)] },
      payload: { adminId: String(targetAdmin.id), action: "revoke_tokens" },
      impactSummary: { targetType: "admin", targetCount: 1, action: "revoke_tokens" },
      reason,
      failureStatus: "failed",
      execute: async () => {
        let tokenVersion: number;
        try {
          tokenVersion = await this.sessionStore.bumpTokenVersion(adminId);
        } catch (error) {
          await this.appendAdminTokenAudit(req, {
            action: "admin_tokens_revoke_failed",
            targetValue: String(targetAdmin.id),
            targetAdmin,
            reason,
            result: "failed",
            currentTokenInvalidated,
            error: "TOKEN_VERSION_BUMP_FAILED"
          });
          throw error;
        }

        await this.appendAdminTokenAudit(req, {
          action: "admin_tokens_revoked",
          targetValue: String(targetAdmin.id),
          targetAdmin,
          reason,
          result: "success",
          tokenVersion,
          currentTokenInvalidated
        });

        return {
          ok: true,
          message: currentTokenInvalidated
            ? "Admin tokens revoked. The current request completed, and this token is invalid for future requests."
            : "Admin tokens revoked.",
          targetAdmin: toAdminSummary(targetAdmin),
          tokenVersion,
          currentTokenInvalidated
        };
      },
      resultSummary: () => ({ action: "admins.revoke_tokens", targetCount: 1, outcome: "succeeded" })
    });
  }

  @Post(":adminId/reset-password")
  @Permissions("admins.reset_password")
  @HttpCode(HttpStatus.OK)
  async resetPassword(@Param("adminId") adminIdParam: string, @Body() body: any, @Req() req: any) {
    let adminId: string;
    let reason = "";
    try {
      adminId = parseAdminId(adminIdParam);
      reason = normalizeReason(body?.reason);
    } catch (error) {
      await this.appendAdminTokenAudit(req, {
        action: "admin_password_reset_failed",
        targetValue: rawTargetValue(adminIdParam),
        reason,
        result: "rejected",
        error: errorCode(error, "INVALID_REQUEST")
      });
      throw error;
    }

    const targetAdmin = await this.adminStore.findAdminById(adminId);
    if (!targetAdmin) {
      await this.appendAdminTokenAudit(req, {
        action: "admin_password_reset_failed",
        targetValue: adminId,
        reason,
        result: "rejected",
        error: "ADMIN_NOT_FOUND"
      });
      throw notFound("ADMIN_NOT_FOUND", "Admin not found");
    }

    let newPassword: string;
    try {
      newPassword = normalizeNewPassword(body?.newPassword);
    } catch (error) {
      await this.appendAdminTokenAudit(req, {
        action: "admin_password_reset_failed",
        targetValue: String(targetAdmin.id),
        targetAdmin,
        reason,
        result: "rejected",
        error: "INVALID_NEW_PASSWORD"
      });
      throw error;
    }

    const currentTokenInvalidated = String(req.admin?.sub) === String(targetAdmin.id);
    return this.runHighRiskOperation({
      request: req,
      permission: "admins.reset_password",
      scope: { targetType: "admin", targetIds: [String(targetAdmin.id)], targetCount: 1 },
      targetSummary: { targetType: "admin", targetIds: [String(targetAdmin.id)] },
      // The protocol hashes this value but never persists the raw password in summaries or audit records.
      payload: { adminId: String(targetAdmin.id), newPassword },
      impactSummary: { targetType: "admin", targetCount: 1, action: "reset_password" },
      reason,
      failureStatus: "failed",
      execute: async () => {
        let tokenVersion: number;
        try {
          tokenVersion = await this.sessionStore.bumpTokenVersion(adminId);
        } catch (error) {
          await this.appendAdminTokenAudit(req, {
            action: "admin_password_reset_failed",
            targetValue: String(targetAdmin.id),
            targetAdmin,
            reason,
            result: "failed",
            currentTokenInvalidated,
            error: "TOKEN_VERSION_BUMP_FAILED"
          });
          throw error;
        }

        let updated: boolean;
        try {
          updated = await this.adminStore.updateAdminPassword(adminId, newPassword);
        } catch (error) {
          await this.appendAdminTokenAudit(req, {
            action: "admin_password_reset_failed",
            targetValue: String(targetAdmin.id),
            targetAdmin,
            reason,
            result: "failed",
            tokenVersion,
            currentTokenInvalidated,
            error: "PASSWORD_UPDATE_FAILED"
          });
          throw error;
        }

        if (!updated) {
          await this.appendAdminTokenAudit(req, {
            action: "admin_password_reset_failed",
            targetValue: String(targetAdmin.id),
            targetAdmin,
            reason,
            result: "failed",
            tokenVersion,
            currentTokenInvalidated,
            error: "ADMIN_NOT_FOUND"
          });
          throw notFound("ADMIN_NOT_FOUND", "Admin not found");
        }

        await this.appendAdminTokenAudit(req, {
          action: "admin_password_reset",
          targetValue: String(targetAdmin.id),
          targetAdmin,
          reason,
          result: "success",
          tokenVersion,
          currentTokenInvalidated
        });

        return {
          ok: true,
          message: currentTokenInvalidated
            ? "Admin password reset. The current request completed, and this token is invalid for future requests."
            : "Admin password reset.",
          targetAdmin: toAdminSummary(targetAdmin),
          tokenVersion,
          currentTokenInvalidated
        };
      },
      resultSummary: () => ({ action: "admins.reset_password", targetCount: 1, outcome: "succeeded" })
    });
  }

  private async appendAdminTokenAudit(
    req: any,
    {
      action,
      targetValue,
      targetAdmin,
      reason,
      result,
      tokenVersion,
      currentTokenInvalidated,
      error
    }: {
      action: string;
      targetValue: string;
      targetAdmin?: any;
      reason: string;
      result: string;
      tokenVersion?: number;
      currentTokenInvalidated?: boolean;
      error?: string;
    }
  ) {
    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action,
      targetType: "admin",
      targetValue,
      details: {
        targetAdminId: targetAdmin?.id ?? targetValue,
        targetUsername: targetAdmin?.username ?? null,
        reason,
        result,
        tokenVersion,
        currentTokenInvalidated,
        error
      },
      ip: getClientIp(req, this.config)
    });
  }
}
