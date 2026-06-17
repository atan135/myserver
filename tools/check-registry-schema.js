import fs from "node:fs";
import path from "node:path";
import {
  SERVICE_ENDPOINT_FIELDS,
  SERVICE_INSTANCE_FIELDS,
  SERVICE_INSTANCE_SCHEMA_VERSION,
  createServiceInstancePayload,
  discoverAllEndpoints,
  normalizeEndpoint,
  normalizeServiceInstance,
  pickServiceEndpoint,
  pickServiceInstance,
  validateServiceEndpoint,
  validateServiceInstance
} from "../packages/service-registry/node/registry-schema.js";

const schemaPath = path.resolve("packages/service-registry/schema/service-instance.schema.json");
const schema = JSON.parse(fs.readFileSync(schemaPath, "utf8"));
const schemaFields = Object.keys(schema.properties ?? {}).sort();
const helperFields = [...SERVICE_INSTANCE_FIELDS].sort();

const errors = [];
if (JSON.stringify(schema.required?.sort() ?? []) !== JSON.stringify(helperFields)) {
  errors.push("schema.required does not match SERVICE_INSTANCE_FIELDS");
}
if (JSON.stringify(schemaFields) !== JSON.stringify(helperFields)) {
  errors.push("schema.properties does not match SERVICE_INSTANCE_FIELDS");
}
if (schema.properties?.schema_version?.const !== SERVICE_INSTANCE_SCHEMA_VERSION) {
  errors.push("schema schema_version const does not match helper version");
}

const endpointSchemaFields = Object.keys(schema.$defs?.serviceEndpoint?.properties ?? {}).sort();
const endpointHelperFields = [...SERVICE_ENDPOINT_FIELDS].sort();
if (JSON.stringify(endpointSchemaFields) !== JSON.stringify(endpointHelperFields)) {
  errors.push("schema serviceEndpoint.properties does not match SERVICE_ENDPOINT_FIELDS");
}
if (JSON.stringify(schema.$defs?.serviceEndpoint?.required?.sort() ?? []) !== JSON.stringify(endpointHelperFields)) {
  errors.push("schema serviceEndpoint.required does not match SERVICE_ENDPOINT_FIELDS");
}

const sample = createServiceInstancePayload({
  id: "announce-service-local",
  name: "announce-service",
  host: "127.0.0.1",
  port: 9004,
  tags: ["announce", "http"]
});
const validation = validateServiceInstance(sample);
if (!validation.ok) {
  errors.push(...validation.errors);
}
if (sample.schema_version !== SERVICE_INSTANCE_SCHEMA_VERSION) {
  errors.push("createServiceInstancePayload did not default schema_version to v2");
}
if (!sample.endpoints.some((endpoint) => endpoint.name === "client" && endpoint.host === "127.0.0.1" && endpoint.port === 9004)) {
  errors.push("createServiceInstancePayload did not create client endpoint");
}

const legacy = normalizeServiceInstance({
  schema_version: 1,
  id: "game-server-local",
  name: "game-server",
  host: "127.0.0.1",
  port: 7000,
  admin_port: 7500,
  local_socket: "C:/tmp/game-server.sock",
  tags: ["game"],
  weight: 10,
  metadata: {},
  registered_at: 1,
  healthy: true
});
if (!legacy) {
  errors.push("normalizeServiceInstance rejected a valid v1-compatible payload");
} else {
  if (legacy.schema_version !== SERVICE_INSTANCE_SCHEMA_VERSION) {
    errors.push("normalizeServiceInstance did not upgrade explicit v1 schema_version to v2");
  }
  const endpointNames = legacy.endpoints.map((endpoint) => endpoint.name).sort();
  if (JSON.stringify(endpointNames) !== JSON.stringify(["admin", "client", "local_socket"])) {
    errors.push(`v1-compatible payload mapped unexpected endpoints: ${endpointNames.join(", ")}`);
  }
}

const sparseLegacy = normalizeServiceInstance({
  id: "mail-service-local",
  name: "mail-service",
  host: "127.0.0.1",
  port: 9003,
  admin_port: 0,
  local_socket: "",
  tags: [],
  weight: 10,
  metadata: {},
  registered_at: 1,
  healthy: true
});
if (!sparseLegacy || sparseLegacy.endpoints.some((endpoint) => endpoint.name === "admin" || endpoint.name === "local_socket")) {
  errors.push("v1-compatible payload generated endpoint from empty admin_port/local_socket");
}

const explicitV2 = normalizeServiceInstance({
  schema_version: 2,
  id: "game-server-v2",
  name: "game-server",
  host: "127.0.0.1",
  port: 7000,
  admin_port: 7500,
  local_socket: "legacy.sock",
  endpoints: [
    { name: "proxy-local", protocol: "local_socket", host: "", port: 0, socket: "proxy.sock", visibility: "local", metadata: {}, healthy: true }
  ],
  tags: [],
  weight: 10,
  metadata: {},
  registered_at: 1,
  healthy: true
});
if (!explicitV2) {
  errors.push("normalizeServiceInstance rejected explicit v2 payload");
} else {
  const endpointNames = explicitV2.endpoints.map((endpoint) => endpoint.name).sort();
  if (JSON.stringify(endpointNames) !== JSON.stringify(["proxy-local"])) {
    errors.push(`explicit v2 payload should not backfill legacy endpoints: ${endpointNames.join(", ")}`);
  }
}

const missingEndpointsV2 = normalizeServiceInstance({
  schema_version: 2,
  id: "game-server-v2-missing",
  name: "game-server",
  host: "127.0.0.1",
  port: 7000,
  admin_port: 7500,
  local_socket: "legacy.sock",
  tags: [],
  weight: 10,
  metadata: {},
  registered_at: 1,
  healthy: true
});
if (missingEndpointsV2) {
  errors.push("explicit v2 payload without endpoints should not be accepted via legacy backfill");
}

const invalidEndpoints = [
  { name: "bad-protocol", protocol: "ws", host: "127.0.0.1", port: 9000, socket: "", visibility: "public", metadata: {}, healthy: true },
  { name: "bad-port", protocol: "http", host: "127.0.0.1", port: 0, socket: "", visibility: "public", metadata: {}, healthy: true },
  { name: "bad-socket", protocol: "local_socket", host: "", port: 0, socket: "", visibility: "local", metadata: {}, healthy: true }
];
for (const endpoint of invalidEndpoints) {
  if (validateServiceEndpoint(endpoint).ok || normalizeEndpoint(endpoint)) {
    errors.push(`invalid endpoint was accepted: ${endpoint.name}`);
  }
}

const picked = pickServiceInstance([
  { ...sample, id: "unhealthy", healthy: false, weight: 1000 },
  { ...sample, id: "healthy-low", weight: 1 },
  { ...sample, id: "healthy-high", weight: 100 }
]);
if (!picked || picked.healthy === false) {
  errors.push("pickServiceInstance returned an unhealthy or empty instance");
}

const endpointPicked = pickServiceEndpoint([
  { ...sample, id: "unhealthy-endpoint", endpoints: sample.endpoints.map((endpoint) => ({ ...endpoint, healthy: false })), weight: 1000 },
  { ...sample, id: "healthy-endpoint", weight: 10 }
], "client");
if (!endpointPicked || endpointPicked.endpoint.healthy === false || endpointPicked.instance.id !== "healthy-endpoint") {
  errors.push("pickServiceEndpoint returned an unhealthy or empty endpoint");
}
const discovered = discoverAllEndpoints([sample], "client");
if (discovered.length !== 1 || discovered[0].endpoint.name !== "client") {
  errors.push("discoverAllEndpoints did not return the expected endpoint");
}

if (errors.length > 0) {
  console.error("registry schema check failed:");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("registry schema check passed");
