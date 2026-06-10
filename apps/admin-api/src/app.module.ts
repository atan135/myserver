import { MiddlewareConsumer, Module, NestModule } from "@nestjs/common";
import { JwtModule } from "@nestjs/jwt";

import { AdminStore } from "./admin-store.js";
import { createMetricsCollector } from "./metrics.js";
import { getConfig } from "./config.js";
import { GameAdminClient } from "./game-admin-client.js";
import { createMySqlPool } from "./mysql-client.js";
import { createNatsClient } from "./nats-client.js";
import { createRedisClient } from "./redis-client.js";
import { AuthController } from "./auth/auth.controller.js";
import { AuthService } from "./auth/auth.service.js";
import { JwtAuthGuard } from "./auth/jwt-auth.guard.js";
import { RolesGuard } from "./auth/roles.guard.js";
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
  ADMIN_GAME_ADMIN_CLIENT,
  ADMIN_METRICS,
  ADMIN_MYSQL_POOL,
  ADMIN_NATS,
  ADMIN_REDIS,
  ADMIN_STORE
} from "./tokens.js";

@Module({
  imports: [
    JwtModule.register({})
  ],
  controllers: [
    AuthController,
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
      provide: ADMIN_MYSQL_POOL,
      inject: [ADMIN_CONFIG],
      useFactory: (config: any) => createMySqlPool(config)
    },
    {
      provide: ADMIN_STORE,
      inject: [ADMIN_MYSQL_POOL, ADMIN_CONFIG],
      useFactory: async (pool: any, config: any) => {
        const adminStore = new AdminStore(pool);
        await adminStore.ensureInitialAdmin(config);
        return adminStore;
      }
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
