/**
 * Compatibility stub for the previous HTTP route factory.
 *
 * announce-service is now implemented with NestJS controllers. Keep this module
 * only so old imports fail with a clear migration message.
 */
export function createRoutes() {
  throw new Error("announce-service routes are provided by NestJS controllers; use createNestApp() instead.");
}
