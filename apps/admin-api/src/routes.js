/**
 * Compatibility stub for the previous Express route factory.
 *
 * admin-api has been migrated to NestJS controllers under src/**.
 * Keep this export only so accidental legacy imports fail loudly instead of
 * reintroducing the old Express/JWT route implementation.
 */
export function createRoutes() {
  throw new Error("admin-api routes.js is deprecated; use the NestJS AppModule/controllers instead");
}
