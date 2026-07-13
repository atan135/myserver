import { MiddlewareConsumer, Module, NestModule } from "@nestjs/common";

import { getConfig } from "./config.js";
import { ClaimRecoveryWorker } from "./claim-recovery.worker.js";
import { RequestLogMiddleware } from "./common/request-log.middleware.js";
import { GameAdminClient } from "./game-admin-client.js";
import { initializeGlobalIdLease, releaseGlobalIdLease } from "./global-id.js";
import { HealthController } from "./health.controller.js";
import { MailPlayerAuthService } from "./mail-auth.js";
import { MailsController } from "./mails/mails.controller.js";
import { MailsService } from "./mails/mails.service.js";
import { createMetricsCollector } from "./metrics.js";
import { createDbPool } from "./db-client.js";
import { DbMailStore } from "./db-store.js";
import { createNatsClient } from "./nats-client.js";
import { PubSubClient } from "./pubsub-client.js";
import { createRedisClient } from "./redis-client.js";
import { RegistryClient } from "./registry-client.js";
import {
  MAIL_CONFIG,
  MAIL_DB_POOL,
  MAIL_GLOBAL_ID_LEASE,
  MAIL_GAME_ADMIN_CLIENT,
  MAIL_METRICS,
  MAIL_NATS,
  MAIL_PLAYER_AUTH,
  MAIL_PUBSUB_CLIENT,
  MAIL_REDIS,
  MAIL_REGISTRY,
  MAIL_STORE
} from "./tokens.js";

@Module({
  controllers: [HealthController, MailsController],
  providers: [
    MailsService,
    ClaimRecoveryWorker,
    {
      provide: MAIL_CONFIG,
      useFactory: () => getConfig()
    },
    {
      provide: MAIL_REDIS,
      inject: [MAIL_CONFIG],
      useFactory: (config: any) => createRedisClient(config)
    },
    {
      provide: MAIL_NATS,
      inject: [MAIL_CONFIG],
      useFactory: (config: any) => createNatsClient(config)
    },
    {
      provide: MAIL_GLOBAL_ID_LEASE,
      inject: [MAIL_CONFIG, MAIL_REDIS],
      useFactory: (config: any, redis: any) => initializeGlobalIdLease(config, redis)
    },
    {
      provide: MAIL_DB_POOL,
      inject: [MAIL_CONFIG, MAIL_NATS, MAIL_REDIS, MAIL_GLOBAL_ID_LEASE],
      useFactory: async (config: any, nats: any, redis: any, _lease: any) => {
        try {
          return await createDbPool(config);
        } catch (error) {
          await releaseGlobalIdLease();
          await nats.close();
          await redis.quit();
          throw error;
        }
      }
    },
    {
      provide: MAIL_STORE,
      inject: [MAIL_DB_POOL, MAIL_CONFIG],
      useFactory: (dbPool: any, config: any) => new DbMailStore(dbPool, {
        outboxMaxAttempts: config.outboxMaxAttempts,
        outboxLeaseMs: config.outboxLeaseMs,
        outboxLeaseOwner: config.serviceInstanceId
      })
    },
    {
      provide: MAIL_PUBSUB_CLIENT,
      inject: [MAIL_NATS, MAIL_REDIS, MAIL_CONFIG],
      useFactory: (nats: any, redis: any, config: any) =>
        new PubSubClient(nats, redis, { redisKeyPrefix: config.redisKeyPrefix })
    },
    {
      provide: MAIL_GAME_ADMIN_CLIENT,
      inject: [MAIL_CONFIG, MAIL_REDIS],
      useFactory: (config: any, redis: any) => new GameAdminClient(config, redis)
    },
    {
      provide: MAIL_REGISTRY,
      inject: [MAIL_REDIS, MAIL_CONFIG],
      useFactory: (redis: any, config: any) => new RegistryClient(redis, config)
    },
    {
      provide: MAIL_PLAYER_AUTH,
      inject: [MAIL_CONFIG, MAIL_REDIS],
      useFactory: (config: any, redis: any) => new MailPlayerAuthService(config, redis)
    },
    {
      provide: MAIL_METRICS,
      inject: [MAIL_NATS, MAIL_CONFIG],
      useFactory: (nats: any, config: any) =>
        createMetricsCollector(nats, "mail-service", config.serviceInstanceId)
    }
  ]
})
export class AppModule implements NestModule {
  configure(consumer: MiddlewareConsumer) {
    consumer.apply(RequestLogMiddleware).forRoutes("*");
  }
}
