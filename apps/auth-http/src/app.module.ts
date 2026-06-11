import { MiddlewareConsumer, Module, NestModule } from "@nestjs/common";

import { AuthStore } from "./auth-store.js";
import { RedisBlocklistChecker } from "./blocklist.js";
import { createMetricsCollector } from "./metrics.js";
import { getConfig } from "./config.js";
import { GameAdminClient } from "./game-admin-client.js";
import { MySqlAuthStore } from "./mysql-store.js";
import { MaintenanceStore } from "./maintenance-store.js";
import { AccountLockout, RateLimiter } from "./rate-limiter.js";
import { ServiceDiscovery } from "./service-discovery.js";
import { createMySqlPool } from "./mysql-client.js";
import { createNatsClient } from "./nats-client.js";
import { createRedisClient } from "./redis-client.js";
import { AuthController } from "./auth/auth.controller.js";
import { AuthService } from "./auth/auth.service.js";
import { GameTicketController } from "./game-ticket/game-ticket.controller.js";
import { InternalController } from "./internal/internal.controller.js";
import { MetaController } from "./meta.controller.js";
import { RateLimitMiddleware } from "./common/rate-limit.middleware.js";
import { RedisBlocklistMiddleware } from "./common/redis-blocklist.middleware.js";
import { RequestContextMiddleware } from "./common/request-context.middleware.js";
import {
  AUTH_ACCOUNT_LOCKOUT,
  AUTH_BLOCKLIST,
  AUTH_CONFIG,
  AUTH_GAME_ADMIN_CLIENT,
  AUTH_MAINTENANCE_STORE,
  AUTH_METRICS,
  AUTH_MYSQL_POOL,
  AUTH_MYSQL_STORE,
  AUTH_NATS,
  AUTH_RATE_LIMITER,
  AUTH_REDIS,
  AUTH_SERVICE_DISCOVERY,
  AUTH_STORE
} from "./tokens.js";

@Module({
  controllers: [AuthController, GameTicketController, InternalController, MetaController],
  providers: [
    AuthService,
    {
      provide: AUTH_CONFIG,
      useFactory: () => getConfig()
    },
    {
      provide: AUTH_REDIS,
      inject: [AUTH_CONFIG],
      useFactory: (config: any) => createRedisClient(config)
    },
    {
      provide: AUTH_NATS,
      inject: [AUTH_CONFIG],
      useFactory: (config: any) => createNatsClient(config)
    },
    {
      provide: AUTH_MYSQL_POOL,
      inject: [AUTH_CONFIG],
      useFactory: (config: any) => createMySqlPool(config)
    },
    {
      provide: AUTH_MYSQL_STORE,
      inject: [AUTH_MYSQL_POOL],
      useFactory: (mysqlPool: any) => new MySqlAuthStore(mysqlPool)
    },
    {
      provide: AUTH_BLOCKLIST,
      inject: [AUTH_CONFIG, AUTH_REDIS],
      useFactory: (config: any, redis: any) => new RedisBlocklistChecker(config, redis)
    },
    {
      provide: AUTH_STORE,
      inject: [AUTH_CONFIG, AUTH_REDIS, AUTH_MYSQL_STORE, AUTH_NATS, AUTH_BLOCKLIST],
      useFactory: (config: any, redis: any, mysqlStore: any, nats: any, blocklist: any) =>
        new AuthStore(config, redis, mysqlStore, nats, blocklist)
    },
    {
      provide: AUTH_GAME_ADMIN_CLIENT,
      inject: [AUTH_CONFIG],
      useFactory: (config: any) => new GameAdminClient(config)
    },
    {
      provide: AUTH_RATE_LIMITER,
      inject: [AUTH_REDIS, AUTH_CONFIG],
      useFactory: (redis: any, config: any) => new RateLimiter(redis, config)
    },
    {
      provide: AUTH_ACCOUNT_LOCKOUT,
      inject: [AUTH_REDIS, AUTH_CONFIG],
      useFactory: (redis: any, config: any) => new AccountLockout(redis, config)
    },
    {
      provide: AUTH_SERVICE_DISCOVERY,
      inject: [AUTH_REDIS, AUTH_CONFIG],
      useFactory: (redis: any, config: any) => new ServiceDiscovery(redis, config)
    },
    {
      provide: AUTH_MAINTENANCE_STORE,
      inject: [AUTH_REDIS, AUTH_CONFIG],
      useFactory: (redis: any, config: any) => new MaintenanceStore(redis, config)
    },
    {
      provide: AUTH_METRICS,
      inject: [AUTH_REDIS, AUTH_NATS, AUTH_CONFIG],
      useFactory: (redis: any, nats: any, config: any) =>
        createMetricsCollector(
          redis,
          nats,
          "auth-http",
          config.redisKeyPrefix || "",
          config.serviceInstanceId
        )
    }
  ]
})
export class AppModule implements NestModule {
  configure(consumer: MiddlewareConsumer) {
    consumer.apply(RequestContextMiddleware, RedisBlocklistMiddleware, RateLimitMiddleware).forRoutes("*");
  }
}
