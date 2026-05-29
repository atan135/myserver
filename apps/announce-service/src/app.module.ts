import { MiddlewareConsumer, Module, NestModule } from "@nestjs/common";

import { AnnouncementsController } from "./announcements/announcements.controller.js";
import { AnnouncementsService } from "./announcements/announcements.service.js";
import { getConfig } from "./config.js";
import { RequestLogMiddleware } from "./common/request-log.middleware.js";
import { HealthController } from "./health.controller.js";
import { createMetricsCollector } from "./metrics.js";
import { createMySqlPool } from "./mysql-client.js";
import { AnnouncementStore } from "./mysql-store.js";
import { createNatsClient } from "./nats-client.js";
import { createRedisClient } from "./redis-client.js";
import { RegistryClient } from "./registry-client.js";
import {
  ANNOUNCE_CONFIG,
  ANNOUNCE_METRICS,
  ANNOUNCE_MYSQL_POOL,
  ANNOUNCE_NATS,
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
      provide: ANNOUNCE_MYSQL_POOL,
      inject: [ANNOUNCE_CONFIG, ANNOUNCE_NATS, ANNOUNCE_REDIS],
      useFactory: async (config: any, nats: any, redis: any) => {
        try {
          return await createMySqlPool(config);
        } catch (error) {
          await nats.close();
          await redis.quit();
          throw error;
        }
      }
    },
    {
      provide: ANNOUNCE_STORE,
      inject: [ANNOUNCE_MYSQL_POOL],
      useFactory: (mysqlPool: any) => new AnnouncementStore(mysqlPool)
    },
    {
      provide: ANNOUNCE_REGISTRY,
      inject: [ANNOUNCE_REDIS, ANNOUNCE_CONFIG],
      useFactory: (redis: any, config: any) => new RegistryClient(redis, config)
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
