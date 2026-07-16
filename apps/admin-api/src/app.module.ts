import { Inject, MiddlewareConsumer, Module, NestModule, OnModuleDestroy } from "@nestjs/common";
import { JwtModule } from "@nestjs/jwt";

import { AdminStore } from "./admin-store.js";
import { AdminSessionStore } from "./auth/admin-session-store.js";
import { createMetricsCollector } from "./metrics.js";
import { getConfig } from "./config.js";
import { GameAdminClient } from "./game-admin-client.js";
import { RegistryClient } from "./registry-client.js";
import { createDbPool, createGameDbPool } from "./db-client.js";
import { createNatsClient } from "./nats-client.js";
import { createRedisClient } from "./redis-client.js";
import { AuthController } from "./auth/auth.controller.js";
import { AuthService } from "./auth/auth.service.js";
import { JwtAuthGuard } from "./auth/jwt-auth.guard.js";
import { RolesGuard } from "./auth/roles.guard.js";
import { AdminsController } from "./admins/admins.controller.js";
import { AuditController } from "./audit/audit.controller.js";
import { AssetLedgerController } from "./assets/asset-ledger.controller.js";
import { PlayersController } from "./players/players.controller.js";
import { MaintenanceController } from "./maintenance/maintenance.controller.js";
import { GmController } from "./gm/gm.controller.js";
import { GlobalIdController } from "./global-id/global-id.controller.js";
import { MonitoringController } from "./monitoring/monitoring.controller.js";
import { MonitoringService } from "./monitoring/monitoring.service.js";
import { MyforgeStore } from "./myforge/myforge-store.js";
import { MyforgeWebsocketGateway } from "./myforge/myforge-websocket.js";
import { MyforgeOrchestrator } from "./myforge/myforge-orchestrator.js";
import { MyforgeController } from "./myforge/myforge.controller.js";
import { HealthController } from "./health.controller.js";
import { RequestLogMiddleware } from "./common/request-log.middleware.js";
import {
  ADMIN_CONFIG,
  ADMIN_DB_POOL,
  ADMIN_GAME_DB_POOL,
  ADMIN_GAME_ADMIN_CLIENT,
  ADMIN_METRICS,
  ADMIN_NATS,
  ADMIN_REDIS,
  ADMIN_REGISTRY,
  ADMIN_SESSION_STORE,
  ADMIN_STORE,
  MYFORGE_GATEWAY,
  MYFORGE_ORCHESTRATOR,
  MYFORGE_STORE
} from "./tokens.js";

class GameDbPoolShutdown implements OnModuleDestroy {
  constructor(@Inject(ADMIN_GAME_DB_POOL) private readonly gamePool: any) {}

  async onModuleDestroy() {
    await this.gamePool?.end?.();
  }
}

@Module({
  imports: [
    JwtModule.register({})
  ],
  controllers: [
    AuthController,
    AdminsController,
    AuditController,
    AssetLedgerController,
    PlayersController,
    MaintenanceController,
    GmController,
    GlobalIdController,
    MonitoringController,
    MyforgeController,
    HealthController
  ],
  providers: [
    AuthService,
    JwtAuthGuard,
    RolesGuard,
    GameDbPoolShutdown,
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
      provide: ADMIN_GAME_DB_POOL,
      inject: [ADMIN_CONFIG],
      useFactory: (config: any) => createGameDbPool(config)
    },
    {
      provide: ADMIN_STORE,
      inject: [ADMIN_DB_POOL, ADMIN_REDIS, ADMIN_CONFIG, ADMIN_GAME_DB_POOL],
      useFactory: async (pool: any, redis: any, config: any, gamePool: any) => {
        const adminStore = new AdminStore(pool, redis, config, gamePool);
        await adminStore.ensureInitialAdmin(config);
        return adminStore;
      }
    },
    {
      provide: MYFORGE_STORE,
      inject: [ADMIN_DB_POOL, ADMIN_CONFIG],
      useFactory: async (pool: any, config: any) => {
        const store = new MyforgeStore(pool, config.myforge);
        await store.initializeKnownAgents(config.myforge.agents);
        return store;
      }
    },
    {
      provide: MYFORGE_GATEWAY,
      inject: [ADMIN_CONFIG, MYFORGE_STORE, ADMIN_STORE],
      useFactory: (config: any, store: any, adminStore: any) => new MyforgeWebsocketGateway({
        config: config.myforge,
        store,
        adminStore
      })
    },
    {
      provide: MYFORGE_ORCHESTRATOR,
      inject: [ADMIN_CONFIG, MYFORGE_STORE, MYFORGE_GATEWAY],
      useFactory: (config: any, store: any, gateway: any) => {
        const orchestrator = new MyforgeOrchestrator({
          config: config.myforge,
          store,
          gateway
        });
        gateway.setTaskOrchestrator(orchestrator);
        orchestrator.start();
        return orchestrator;
      }
    },
    {
      provide: ADMIN_SESSION_STORE,
      inject: [ADMIN_REDIS, ADMIN_CONFIG],
      useFactory: (redis: any, config: any) => new AdminSessionStore(redis, config)
    },
    {
      provide: ADMIN_GAME_ADMIN_CLIENT,
      inject: [ADMIN_CONFIG, ADMIN_REDIS],
      useFactory: (config: any, redis: any) => new GameAdminClient(config, redis)
    },
    {
      provide: ADMIN_REGISTRY,
      inject: [ADMIN_REDIS, ADMIN_CONFIG],
      useFactory: (redis: any, config: any) => new RegistryClient(redis, config)
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
