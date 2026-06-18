export const REGISTRY_DISABLED_REASON =
  "metrics-collector consumes NATS metrics and writes Redis snapshots, but exposes no connectable service endpoint";

export async function maybeRegisterService(_redis, config) {
  if (config.serviceRegistryRegister !== true) {
    return {
      registered: false,
      reason: REGISTRY_DISABLED_REASON,
      service: config.serviceName,
      instance: config.serviceInstanceId,
      service_name: config.serviceName,
      service_instance_id: config.serviceInstanceId,
      zone: config.serviceZone || "local",
      build_version: config.serviceBuildVersion || "dev"
    };
  }

  throw new Error(
    "SERVICE_REGISTRY_REGISTER=true is not supported for metrics-collector until it exposes a real http/tcp/local_socket endpoint"
  );
}
