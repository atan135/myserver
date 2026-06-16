import "reflect-metadata";

import { INestApplication } from "@nestjs/common";
import { NestFactory } from "@nestjs/core";
import { FastifyAdapter, NestFastifyApplication } from "@nestjs/platform-fastify";
import { DocumentBuilder, SwaggerModule } from "@nestjs/swagger";

import { AppModule } from "./app.module.js";
import { HttpExceptionFilter } from "./common/http-exception.filter.js";
import { configureLogger, log } from "./logger.js";
import {
  MAIL_CONFIG,
  MAIL_DB_POOL,
  MAIL_METRICS,
  MAIL_NATS,
  MAIL_REDIS,
  MAIL_REGISTRY
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

  const config = app.get<any>(MAIL_CONFIG);
  configureLogger(config);
  app.useGlobalFilters(new HttpExceptionFilter());

  const fastify = app.getHttpAdapter().getInstance();
  fastify.addHook("onRequest", async (request: any) => {
    request.metricsStartedAt = Date.now();
  });
  fastify.addHook("onResponse", async (request: any) => {
    const metrics = app.get<any>(MAIL_METRICS, { strict: false });
    metrics?.recordRequest?.(Date.now() - (request.metricsStartedAt || Date.now()));
  });

  const swaggerConfig = new DocumentBuilder()
    .setTitle("MyServer Mail Service API")
    .setDescription("NestJS mail-service API for player mail and attachment claim workflows.")
    .setVersion("0.1.0")
    .build();
  const document = SwaggerModule.createDocument(app, swaggerConfig);
  SwaggerModule.setup("/api/docs", app, document);

  await app.init();
  return app;
}

export async function closeNestApp(app: INestApplication) {
  const metrics = app.get<any>(MAIL_METRICS, { strict: false });
  const registryClient = app.get<any>(MAIL_REGISTRY, { strict: false });
  const nats = app.get<any>(MAIL_NATS, { strict: false });
  const redis = app.get<any>(MAIL_REDIS, { strict: false });
  const dbPool = app.get<any>(MAIL_DB_POOL, { strict: false });

  try {
    await metrics?.stop?.();
  } catch (error: any) {
    log("error", "shutdown.metrics_stop_failed", { error: error.message });
  }

  registryClient?.stopHeartbeat?.();

  try {
    await registryClient?.deregister?.();
  } catch (error: any) {
    log("error", "shutdown.deregister_failed", { error: error.message });
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

  try {
    await dbPool?.end?.();
  } catch (error: any) {
    log("error", "shutdown.db_close_failed", { error: error.message });
  }

  await app.close();
}
