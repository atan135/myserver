import { MiddlewareConsumer, Module, NestModule } from "@nestjs/common";

import { AnnouncementsController } from "./announcements/announcements.controller.js";
import { AnnouncementsService } from "./announcements/announcements.service.js";
import { AnnounceReadAuthService } from "./announce-auth.js";
import { getConfig } from "./config.js";
import { RequestLogMiddleware } from "./common/request-log.middleware.js";
import { initializeGlobalIdLease, releaseGlobalIdLease } from "./global-id.js";
import { HealthController } from "./health.controller.js";
import { createMetricsCollector } from "./metrics.js";
import { createDbPool } from "./db-client.js";
import { AnnouncementStore } from "./db-store.js";
import { createNatsClient } from "./nats-client.js";
import { createRedisClient } from "./redis-client.js";
import { RegistryClient } from "./registry-client.js";
import {
  ANNOUNCE_CONFIG,
  ANNOUNCE_DB_POOL,
  ANNOUNCE_GLOBAL_ID_LEASE,
  ANNOUNCE_METRICS,
  ANNOUNCE_NATS,
  ANNOUNCE_READ_AUTH,
  ANNOUNCE_REDIS,
  ANNOUNCE_REGISTRY,
  ANNOUNCE_STORE
} from "./tokens.js";

@Module({
  controllers: [HealthController, AnnouncementsController],
  providers: [
    AnnouncementsService,
    {
      provide: ANNOUNCE_CONFIG,
      useFactory: () => getConfig()
    },
    {
      provide: ANNOUNCE_REDIS,
      inject: [ANNOUNCE_CONFIG],
      useFactory: (config: any) => createRedisClient(config)
    },
    {
      provide: ANNOUNCE_NATS,
      inject: [ANNOUNCE_CONFIG],
      useFactory: (config: any) => createNatsClient(config)
    },
    {
      provide: ANNOUNCE_GLOBAL_ID_LEASE,
      inject: [ANNOUNCE_CONFIG, ANNOUNCE_REDIS],
      useFactory: (config: any, redis: any) => initializeGlobalIdLease(config, redis)
    },
    {
      provide: ANNOUNCE_DB_POOL,
      inject: [ANNOUNCE_CONFIG, ANNOUNCE_NATS, ANNOUNCE_REDIS, ANNOUNCE_GLOBAL_ID_LEASE],
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
      provide: ANNOUNCE_STORE,
      inject: [ANNOUNCE_DB_POOL],
      useFactory: (dbPool: any) => new AnnouncementStore(dbPool)
    },
    {
      provide: ANNOUNCE_REGISTRY,
      inject: [ANNOUNCE_REDIS, ANNOUNCE_CONFIG],
      useFactory: (redis: any, config: any) => new RegistryClient(redis, config)
    },
    {
      provide: ANNOUNCE_READ_AUTH,
      inject: [ANNOUNCE_CONFIG, ANNOUNCE_REDIS],
      useFactory: (config: any, redis: any) => new AnnounceReadAuthService(config, redis)
    },
    {
      provide: ANNOUNCE_METRICS,
      inject: [ANNOUNCE_NATS, ANNOUNCE_CONFIG],
      useFactory: (nats: any, config: any) =>
        createMetricsCollector(nats, "announce-service", config.serviceInstanceId)
    }
  ]
})
export class AppModule implements NestModule {
  configure(consumer: MiddlewareConsumer) {
    consumer.apply(RequestLogMiddleware).forRoutes("*");
  }
}
