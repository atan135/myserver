import { MiddlewareConsumer, Module, NestModule } from "@nestjs/common";

import { getConfig } from "./config.js";
import { RequestLogMiddleware } from "./common/request-log.middleware.js";
import { GameAdminClient } from "./game-admin-client.js";
import { HealthController } from "./health.controller.js";
import { MailsController } from "./mails/mails.controller.js";
import { MailsService } from "./mails/mails.service.js";
import { createMetricsCollector } from "./metrics.js";
import { createMySqlPool } from "./mysql-client.js";
import { MySqlMailStore } from "./mysql-store.js";
import { createNatsClient } from "./nats-client.js";
import { PubSubClient } from "./pubsub-client.js";
import { createRedisClient } from "./redis-client.js";
import { RegistryClient } from "./registry-client.js";
import {
  MAIL_CONFIG,
  MAIL_GAME_ADMIN_CLIENT,
  MAIL_METRICS,
  MAIL_MYSQL_POOL,
  MAIL_NATS,
  MAIL_PUBSUB_CLIENT,
  MAIL_REDIS,
  MAIL_REGISTRY,
  MAIL_STORE
} from "./tokens.js";

@Module({
  controllers: [HealthController, MailsController],
  providers: [
    MailsService,
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
      provide: MAIL_MYSQL_POOL,
      inject: [MAIL_CONFIG, MAIL_NATS, MAIL_REDIS],
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
      provide: MAIL_STORE,
      inject: [MAIL_MYSQL_POOL],
      useFactory: (mysqlPool: any) => new MySqlMailStore(mysqlPool)
    },
    {
      provide: MAIL_PUBSUB_CLIENT,
      inject: [MAIL_NATS, MAIL_REDIS, MAIL_CONFIG],
      useFactory: (nats: any, redis: any, config: any) =>
        new PubSubClient(nats, redis, { redisKeyPrefix: config.redisKeyPrefix })
    },
    {
      provide: MAIL_GAME_ADMIN_CLIENT,
      inject: [MAIL_CONFIG],
      useFactory: (config: any) => new GameAdminClient(config)
    },
    {
      provide: MAIL_REGISTRY,
      inject: [MAIL_REDIS, MAIL_CONFIG],
      useFactory: (redis: any, config: any) => new RegistryClient(redis, config)
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
