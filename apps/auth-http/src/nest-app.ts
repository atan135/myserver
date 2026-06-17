import "reflect-metadata";

import { INestApplication } from "@nestjs/common";
import { NestFactory } from "@nestjs/core";
import { FastifyAdapter, NestFastifyApplication } from "@nestjs/platform-fastify";
import { SwaggerModule, DocumentBuilder } from "@nestjs/swagger";

import { AppModule } from "./app.module.js";
import { HttpExceptionFilter } from "./common/http-exception.filter.js";
import { configureLogger, log } from "./logger.js";
import { releaseGlobalIdLease } from "./global-id.js";
import {
  AUTH_CONFIG,
  AUTH_DB_POOL,
  AUTH_METRICS,
  AUTH_NATS,
  AUTH_REDIS
} from "./tokens.js";

export async function createNestApp() {
  const app = await NestFactory.create<NestFastifyApplication>(
    AppModule,
    new FastifyAdapter({
      bodyLimit: 64 * 1024
    }),
    {
      logger: false,
      abortOnError: false
    }
  );

  const config = app.get<any>(AUTH_CONFIG);
  configureLogger(config);
  app.useGlobalFilters(new HttpExceptionFilter());

  const fastify = app.getHttpAdapter().getInstance();
  fastify.addHook("onRequest", async (request: any) => {
    request.metricsStartedAt = Date.now();
  });
  fastify.addHook("onResponse", async (request: any) => {
    const metrics = app.get<any>(AUTH_METRICS, { strict: false });
    metrics?.recordRequest?.(Date.now() - (request.metricsStartedAt || Date.now()));
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
  const dbPool = app.get<any>(AUTH_DB_POOL, { strict: false });

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
    await releaseGlobalIdLease();
  } catch (error: any) {
    log("error", "shutdown.global_id_lease_release_failed", { error: error.message });
  }

  try {
    await redis?.quit?.();
  } catch (error: any) {
    log("error", "shutdown.redis_close_failed", { error: error.message });
  }

  if (dbPool) {
    try {
      await dbPool.end();
    } catch (error: any) {
      log("error", "shutdown.db_close_failed", { error: error.message });
    }
  }

  await app.close();
}
