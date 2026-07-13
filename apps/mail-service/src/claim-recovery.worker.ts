import { randomBytes } from "node:crypto";

import { Inject, Injectable, OnModuleDestroy, OnModuleInit } from "@nestjs/common";

import { log } from "./logger.js";
import {
  MAIL_CONFIG,
  MAIL_GAME_ADMIN_CLIENT,
  MAIL_METRICS,
  MAIL_STORE
} from "./tokens.js";

function recoveryBackoffMs(attempt: number, config: any) {
  const baseMs = config.claimRecoveryBackoffBaseMs || 1000;
  const maxMs = config.claimRecoveryBackoffMaxMs || 300_000;
  return Math.min(maxMs, baseMs * (2 ** Math.min(Math.max(0, attempt - 1), 30)));
}

function queryEvidence(result: any) {
  if (!result) return {};
  return {
    traceId: result?.traceId,
    queryStatus: result?.queryStatus,
    queryFingerprint: result?.requestFingerprint,
    queryErrorCode: result?.errorCode || null,
    queryResultState: result?.resultState,
    queryInstanceIds: result?.instanceIds || []
  };
}

@Injectable()
export class ClaimRecoveryWorker implements OnModuleInit, OnModuleDestroy {
  private timer: NodeJS.Timeout | null = null;
  private activeScan: Promise<any> | null = null;
  private stopping = false;

  constructor(
    @Inject(MAIL_STORE) private readonly mailStore: any,
    @Inject(MAIL_GAME_ADMIN_CLIENT) private readonly gameAdminClient: any,
    @Inject(MAIL_CONFIG) private readonly config: any = {},
    @Inject(MAIL_METRICS) private readonly metrics: any = null
  ) {}

  async onModuleInit() {
    if (this.config.claimRecoveryEnabled === false) return;
    try {
      await this.processRecoveries("startup");
    } catch (error: any) {
      log("error", "mail.claim_recovery_startup_failed", { error: error.message });
    }
    if (this.stopping) return;
    this.timer = setInterval(() => {
      this.processRecoveries("periodic").catch((error: any) => {
        log("error", "mail.claim_recovery_scan_failed", { error: error.message });
      });
    }, this.config.claimRecoveryPollIntervalMs || 5000);
    this.timer.unref?.();
  }

  async onModuleDestroy() {
    if (this.stopping) return;
    this.stopping = true;
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
    if (!this.activeScan) return;
    const timeoutMs = this.config.claimRecoveryShutdownTimeoutMs || 10_000;
    let timer: NodeJS.Timeout | null = null;
    await Promise.race([
      this.activeScan.catch(() => undefined),
      new Promise<void>((resolve) => {
        timer = setTimeout(resolve, timeoutMs);
      })
    ]);
    if (timer) clearTimeout(timer);
  }

  async processRecoveries(trigger = "manual") {
    if (this.stopping) return { acquired: 0, recovered: 0, deferred: 0, manual: 0, skipped: true };
    if (this.activeScan) return { acquired: 0, recovered: 0, deferred: 0, manual: 0, skipped: true };
    const scan = this.runRecoveryScan(trigger);
    this.activeScan = scan;
    try {
      return await scan;
    } finally {
      if (this.activeScan === scan) this.activeScan = null;
    }
  }

  private async runRecoveryScan(trigger: string) {
    const batch = await this.mailStore.reserveMailClaimRecoveries(
      this.config.claimRecoveryBatchSize || 20,
      {
        leaseMs: this.config.claimRecoveryLeaseMs || 60_000,
        leaseOwner: this.config.serviceInstanceId || "mail-service",
        maxAttempts: this.config.claimRecoveryMaxAttempts || 12
      }
    );
    const workflows = batch.workflows || [];
    for (let index = 0; index < workflows.length; index += 1) {
      this.metrics?.recordMailClaimRecoveryAcquired?.();
      if (workflows[index].recovery_lease_taken_over) {
        this.metrics?.recordMailClaimRecoveryLeaseTakeover?.();
      }
    }
    for (let index = 0; index < (batch.manualReviewCount || 0); index += 1) {
      this.metrics?.recordMailClaimRecoveryManualReview?.();
    }

    const outcomes = await Promise.all(workflows.map((workflow: any) => this.recoverOne(workflow)));
    const summary = {
      acquired: workflows.length,
      recovered: outcomes.filter((outcome) => outcome === "recovered").length,
      deferred: outcomes.filter((outcome) => outcome === "deferred").length,
      manual: (batch.manualReviewCount || 0) + outcomes.filter((outcome) => outcome === "manual").length,
      skipped: false
    };
    if (summary.acquired > 0 || summary.manual > 0) {
      log("info", "mail.claim_recovery_scan_completed", { trigger, ...summary });
    }
    return summary;
  }

  private async recoverOne(workflow: any) {
    const recoveryAgeAnchor = workflow.recovery_started_at || workflow.updated_at || workflow.created_at;
    const ageMs = Math.max(0, Date.now() - new Date(recoveryAgeAnchor).getTime());
    if (workflow.recovery_mode === "query") {
      this.metrics?.recordMailClaimRecoveryUnknownAge?.(ageMs);
      const query = await this.gameAdminClient.queryMailAttachmentGrant(
        workflow.claim_request_id,
        workflow.attachments_fingerprint,
        {
          traceId: randomBytes(16).toString("hex"),
          characterId: workflow.character_id,
          items: workflow.attachments_snapshot
        }
      );
      this.metrics?.recordMailClaimRecoveryQueryResult?.(query.queryStatus);
      if (query.queryStatus === "succeeded") {
        const completed = await this.mailStore.completeMailClaimRecovery(
          workflow.mail_id,
          workflow.recovery_lease_token,
          {
            ...queryEvidence(query),
            resultSummary: query.resultSummary,
            instanceId: query.instanceIds?.length === 1 ? query.instanceIds[0] : ""
          }
        );
        if (!completed.claimed) return "stale";
        this.recordRecovered(workflow);
        return "recovered";
      }
      if (query.queryStatus === "conflict") {
        return this.moveToManual(workflow, {
          ...queryEvidence(query),
          errorCode: query.errorCode || "REQUEST_FINGERPRINT_CONFLICT",
          errorCategory: "PERMANENT_FAILURE",
          resultState: "not_applied",
          message: "game-server grant result fingerprint conflicts with the frozen mail claim"
        });
      }
      if (query.queryStatus !== "not_seen") {
        return this.deferOrManual(workflow, {
          ...queryEvidence(query),
          status: "reconciliation_pending",
          errorCode: query.errorCode || "GRANT_RESULT_QUERY_UNAVAILABLE",
          errorCategory: "RESULT_UNKNOWN",
          resultState: "unknown",
          retryable: true,
          message: "game-server grant result is temporarily unavailable"
        });
      }
      return this.retryGrant(workflow, query);
    }
    return this.retryGrant(workflow, null);
  }

  private async retryGrant(workflow: any, query: any) {
    const traceId = randomBytes(16).toString("hex");
    const prepared = await this.mailStore.prepareMailClaimRecoveryGrant(
      workflow.mail_id,
      workflow.recovery_lease_token,
      { ...queryEvidence(query), traceId }
    );
    if (!prepared) return "stale";
    this.metrics?.recordMailClaimRecoveryGrantRetry?.();
    try {
      const grant = await this.gameAdminClient.grantMailAttachments(
        workflow.character_id,
        workflow.claim_request_id,
        workflow.attachments_snapshot,
        "recover mail attachment claim",
        {
          traceId,
          requestFingerprint: workflow.attachments_fingerprint
        }
      );
      const completed = await this.mailStore.completeMailClaimRecovery(
        workflow.mail_id,
        workflow.recovery_lease_token,
        {
          ...queryEvidence(query),
          traceId: grant.traceId || traceId,
          resultSummary: grant.resultSummary,
          instanceId: grant.instanceId || ""
        }
      );
      if (!completed.claimed) return "stale";
      this.recordRecovered(workflow);
      return "recovered";
    } catch (error: any) {
      const resultState = error?.resultState || (error?.requestWritten === true ? "unknown" : "not_applied");
      const errorCategory = error?.errorCategory || (resultState === "unknown" ? "RESULT_UNKNOWN" : "RETRYABLE_FAILURE");
      if (resultState === "unknown" || errorCategory === "RESULT_UNKNOWN") {
        return this.deferOrManual(workflow, {
          ...queryEvidence(query),
          status: "reconciliation_pending",
          traceId: error?.traceId || traceId,
          errorCode: error?.code || "GAME_SERVER_GRANT_RESULT_UNKNOWN",
          errorCategory: "RESULT_UNKNOWN",
          resultState: "unknown",
          retryable: true,
          message: error?.message || "game-server grant result is unknown",
          instanceId: error?.instanceId || ""
        });
      }
      if (error?.retryable === false || errorCategory === "PERMANENT_FAILURE") {
        return this.moveToManual(workflow, {
          ...queryEvidence(query),
          traceId: error?.traceId || traceId,
          errorCode: error?.code || "MAIL_CLAIM_PERMANENT_FAILURE",
          errorCategory,
          resultState: "not_applied",
          message: error?.message || "mail claim grant requires manual review",
          instanceId: error?.instanceId || ""
        });
      }
      return this.deferOrManual(workflow, {
        ...queryEvidence(query),
        status: "retryable_failure",
        traceId: error?.traceId || traceId,
        errorCode: error?.code || "GAME_SERVER_GRANT_FAILED",
        errorCategory,
        resultState: "not_applied",
        retryable: true,
        message: error?.message || "game-server grant was not applied",
        instanceId: error?.instanceId || ""
      });
    }
  }

  private async deferOrManual(workflow: any, outcome: any) {
    if (workflow.recovery_attempts >= (this.config.claimRecoveryMaxAttempts || 12)) {
      return this.moveToManual(workflow, outcome);
    }
    const updated = await this.mailStore.rescheduleMailClaimRecovery(
      workflow.mail_id,
      workflow.recovery_lease_token,
      {
        ...outcome,
        delayMs: recoveryBackoffMs(workflow.recovery_attempts, this.config)
      }
    );
    return updated ? "deferred" : "stale";
  }

  private async moveToManual(workflow: any, outcome: any) {
    const updated = await this.mailStore.markMailClaimRecoveryManualReview(
      workflow.mail_id,
      workflow.recovery_lease_token,
      outcome
    );
    if (!updated) return "stale";
    this.metrics?.recordMailClaimRecoveryManualReview?.();
    return "manual";
  }

  private recordRecovered(workflow: any) {
    this.metrics?.recordMailClaimRecovered?.();
    const startedAt = new Date(workflow.recovery_started_at || workflow.updated_at || workflow.created_at).getTime();
    this.metrics?.recordMailClaimRecoveryDuration?.(Math.max(0, Date.now() - startedAt));
  }
}

export { recoveryBackoffMs };
