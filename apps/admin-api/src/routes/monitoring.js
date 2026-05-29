/**
 * Compatibility stub for the previous monitoring route factory.
 *
 * admin-api monitoring has been migrated to src/monitoring/*.ts.
 */
export function createMonitoringRoutes() {
  throw new Error("admin-api monitoring routes are deprecated; use MonitoringController instead");
}
