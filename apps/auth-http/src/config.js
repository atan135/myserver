export function getConfig() {
  return {
    appName: "auth-http",
    env: process.env.NODE_ENV || "development",
    host: process.env.HOST || "127.0.0.1",
    port: Number.parseInt(process.env.PORT || "3000", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    sessionTtlSeconds: Number.parseInt(
      process.env.SESSION_TTL_SECONDS || "86400",
      10
    ),
    ticketSecret:
      process.env.TICKET_SECRET || "dev-only-change-this-ticket-secret",
    ticketTtlSeconds: Number.parseInt(
      process.env.TICKET_TTL_SECONDS || "300",
      10
    )
  };
}
