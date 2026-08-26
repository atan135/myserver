import { runArchiveTaskWithLock } from "./archive.js";

export function metricsArchiveOptions(config) {
  return {
    metricsKeyPrefix: String(config.metricsKeyPrefix ?? config.redisKeyPrefix ?? ""),
    metricsArchiveAfterSeconds: Number(config.metricsArchiveAfterSeconds),
    metricsHistoryRetentionSeconds: Number(config.metricsHistoryRetentionSeconds),
    metricsArchiveBatchSize: Number(config.metricsArchiveBatchSize),
    metricsArchiveLockTtlMs: Number(config.metricsArchiveLockTtlSeconds) * 1000
  };
}

export class MetricsArchiveScheduler {
  constructor(redis, dbPool, config, dependencies = {}) {
    this.redis = redis;
    this.dbPool = dbPool;
    this.config = config;
    this.setIntervalFn = dependencies.setIntervalFn || setInterval;
    this.clearIntervalFn = dependencies.clearIntervalFn || clearInterval;
    this.runArchiveTaskWithLockFn = dependencies.runArchiveTaskWithLockFn || runArchiveTaskWithLock;
    this.timer = null;
    this.currentRun = null;
  }

  start() {
    if (!this.config.metricsArchiveEnabled || this.timer) {
      return;
    }

    const intervalMs = this.config.metricsArchiveIntervalSeconds * 1000;
    this.timer = this.setIntervalFn(() => {
      void this.runOnce().catch(() => {});
    }, intervalMs);
    this.timer?.unref?.();

    console.info("[archive] scheduler started", {
      interval_seconds: this.config.metricsArchiveIntervalSeconds,
      archive_after_seconds: this.config.metricsArchiveAfterSeconds,
      batch_size: this.config.metricsArchiveBatchSize,
      resolution_seconds: 60
    });
    void this.runOnce().catch(() => {});
  }

  async runOnce() {
    if (this.currentRun) {
      return {
        archived: 0,
        failed: 0,
        source_buckets: 0,
        resolution_seconds: 60,
        duration_ms: 0,
        skipped: true,
        reason: "archive_local_run_active"
      };
    }

    this.currentRun = this.runArchiveTaskWithLockFn(
      this.redis,
      this.dbPool,
      metricsArchiveOptions(this.config)
    );
    try {
      const result = await this.currentRun;
      const level = result.failed > 0 ? "error" : "info";
      console[level]("[archive] task completed", result);
      return result;
    } catch (error) {
      console.error("[archive] task failed:", error);
      throw error;
    } finally {
      this.currentRun = null;
    }
  }

  async onModuleDestroy() {
    if (this.timer) {
      this.clearIntervalFn(this.timer);
      this.timer = null;
    }
    if (this.currentRun) {
      try {
        await this.currentRun;
      } catch {
        // The run already logged its failure.
      }
    }
  }
}
