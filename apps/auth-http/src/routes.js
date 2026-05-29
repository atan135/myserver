/**
 * Compatibility stub for the previous route factory.
 *
 * auth-http has been migrated to NestJS controllers under src/**.
 * Keep this export only so accidental legacy imports fail loudly instead of
 * reintroducing the old route implementation.
 */
export function createRoutes() {
  throw new Error("auth-http routes.js is deprecated; use the NestJS AppModule/controllers instead");
}
