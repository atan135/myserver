import { MiddlewareConsumer, Module, NestModule } from "@nestjs/common";
import { JwtModule } from "@nestjs/jwt";

import { AdminStore } from "./admin-store.js";
import { AdminSessionStore } from "./auth/admin-session-store.js";
import { createMetricsCollector } from "./metrics.js";
import { getConfig } from "./config.js";
import { GameAdminClient } from "./game-admin-client.js";
import { createDbPool } from "./db-client.js";
import { createNatsClient } from "./nats-client.js";
import { createRedisClient } from "./redis-client.js";
import { AuthController } from "./auth/auth.controller.js";
import { AuthService } from "./auth/auth.service.js";
import { JwtAuthGuard } from "./auth/jwt-auth.guard.js";
import { RolesGuard } from "./auth/roles.guard.js";
import { AdminsController } from "./admins/admins.controller.js";
import { AuditController } from "./audit/audit.controller.js";
import { PlayersController } from "./players/players.controller.js";
import { MaintenanceController } from "./maintenance/maintenance.controller.js";
import { GmController } from "./gm/gm.controller.js";
import { MonitoringController } from "./monitoring/monitoring.controller.js";
import { MonitoringService } from "./monitoring/monitoring.service.js";
import { HealthController } from "./health.controller.js";
import { RequestLogMiddleware } from "./common/request-log.middleware.js";
import {
  ADMIN_CONFIG,
  ADMIN_DB_POOL,
  ADMIN_GAME_ADMIN_CLIENT,
  ADMIN_METRICS,
  ADMIN_NATS,
  ADMIN_REDIS,
  ADMIN_SESSION_STORE,
  ADMIN_STORE
} from "./tokens.js";

@Module({
  imports: [
    JwtModule.register({})
  ],
  controllers: [
    AuthController,
    AdminsController,
    AuditController,
    PlayersController,
    MaintenanceController,
    GmController,
    MonitoringController,
    HealthController
  ],
  providers: [
    AuthService,
    JwtAuthGuard,
    RolesGuard,
    MonitoringService,
    {
      provide: ADMIN_CONFIG,
      useFactory: () => getConfig()
    },
    {
      provide: ADMIN_REDIS,
      inject: [ADMIN_CONFIG],
      useFactory: (config: any) => createRedisClient(config)
    },
    {
      provide: ADMIN_NATS,
      inject: [ADMIN_CONFIG],
      useFactory: (config: any) => createNatsClient(config)
    },
    {
      provide: ADMIN_DB_POOL,
      inject: [ADMIN_CONFIG],
      useFactory: (config: any) => createDbPool(config)
    },
    {
      provide: ADMIN_STORE,
      inject: [ADMIN_DB_POOL, ADMIN_REDIS, ADMIN_CONFIG],
      useFactory: async (pool: any, redis: any, config: any) => {
        const adminStore = new AdminStore(pool, redis, config);
        await adminStore.ensureInitialAdmin(config);
        return adminStore;
      }
    },
    {
      provide: ADMIN_SESSION_STORE,
      inject: [ADMIN_REDIS, ADMIN_CONFIG],
      useFactory: (redis: any, config: any) => new AdminSessionStore(redis, config)
    },
    {
      provide: ADMIN_GAME_ADMIN_CLIENT,
      inject: [ADMIN_CONFIG],
      useFactory: (config: any) => new GameAdminClient(config)
    },
    {
      provide: ADMIN_METRICS,
      inject: [ADMIN_NATS, ADMIN_CONFIG],
      useFactory: (nats: any, config: any) =>
        createMetricsCollector(nats, "admin-api", config.serviceInstanceId)
    }
  ]
})
export class AppModule implements NestModule {
  configure(consumer: MiddlewareConsumer) {
    consumer.apply(RequestLogMiddleware).forRoutes("*");
  }
}
