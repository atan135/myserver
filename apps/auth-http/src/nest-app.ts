import "reflect-metadata";

import { INestApplication } from "@nestjs/common";
import { NestFactory } from "@nestjs/core";
import { SwaggerModule, DocumentBuilder } from "@nestjs/swagger";

import { AppModule } from "./app.module.js";
import { HttpExceptionFilter } from "./common/http-exception.filter.js";
import { configureLogger, log } from "./logger.js";
import {
  AUTH_CONFIG,
  AUTH_METRICS,
  AUTH_MYSQL_POOL,
  AUTH_NATS,
  AUTH_REDIS
} from "./tokens.js";

export async function createNestApp() {
  const app = await NestFactory.create(AppModule, {
    logger: false,
    bodyParser: false
  });

  const config = app.get<any>(AUTH_CONFIG);
  configureLogger(config);
  app.useGlobalFilters(new HttpExceptionFilter());

  const expressApp = app.getHttpAdapter().getInstance();
  expressApp.disable("x-powered-by");
  (app as any).useBodyParser("json", { limit: "64kb" });
  expressApp.use((req: any, _res: any, next: () => void) => {
    const metrics = app.get<any>(AUTH_METRICS, { strict: false });
    return metrics.middleware()(req, _res, next);
  });

  const swaggerConfig = new DocumentBuilder()
    .setTitle("MyServer Auth HTTP API")
    .setDescription("NestJS auth-http API for player auth, sessions, and game tickets.")
    .setVersion("0.1.0")
    .addBearerAuth()
    .build();
  const document = SwaggerModule.createDocument(app, swaggerConfig);
  SwaggerModule.setup("/api/docs", app, document);

  await app.init();
  return app;
}

export async function closeNestApp(app: INestApplication) {
  const metrics = app.get<any>(AUTH_METRICS, { strict: false });
  const redis = app.get<any>(AUTH_REDIS, { strict: false });
  const nats = app.get<any>(AUTH_NATS, { strict: false });
  const mysqlPool = app.get<any>(AUTH_MYSQL_POOL, { strict: false });

  try {
    await metrics?.stop?.();
  } catch (error: any) {
    log("error", "shutdown.metrics_stop_failed", { error: error.message });
  }

  try {
    await nats?.close?.();
  } catch (error: any) {
    log("error", "shutdown.nats_close_failed", { error: error.message });
  }

  try {
    await redis?.quit?.();
  } catch (error: any) {
    log("error", "shutdown.redis_close_failed", { error: error.message });
  }

  if (mysqlPool) {
    try {
      await mysqlPool.end();
    } catch (error: any) {
      log("error", "shutdown.mysql_close_failed", { error: error.message });
    }
  }

  await app.close();
}
