import fs from "node:fs";
import path from "node:path";

import {
  SERVICE_ENDPOINT_FIELDS,
  SERVICE_ENDPOINT_PROTOCOLS,
  SERVICE_ENDPOINT_VISIBILITIES,
  SERVICE_INSTANCE_FIELDS,
  SERVICE_INSTANCE_SCHEMA_VERSION
} from "../packages/service-registry/node/registry-schema.js";

const schemaPath = path.resolve("packages/service-registry/schema/service-instance.schema.json");
const schema = JSON.parse(fs.readFileSync(schemaPath, "utf8"));
const errors = [];

function sorted(values) {
  return [...values].sort();
}

function sameSet(actual, expected) {
  return JSON.stringify(sorted(actual ?? [])) === JSON.stringify(sorted(expected ?? []));
}

function assertSameSet(label, actual, expected) {
  if (!sameSet(actual, expected)) {
    errors.push(`${label} mismatch: expected ${sorted(expected).join(", ")}, got ${sorted(actual ?? []).join(", ")}`);
  }
}

assertSameSet("schema.required", schema.required, SERVICE_INSTANCE_FIELDS);
assertSameSet("schema.properties", Object.keys(schema.properties ?? {}), SERVICE_INSTANCE_FIELDS);

if (schema.properties?.schema_version?.const !== SERVICE_INSTANCE_SCHEMA_VERSION) {
  errors.push(
    `schema_version const mismatch: expected ${SERVICE_INSTANCE_SCHEMA_VERSION}, got ${schema.properties?.schema_version?.const}`
  );
}

const endpointSchema = schema.$defs?.serviceEndpoint ?? {};
assertSameSet("serviceEndpoint.required", endpointSchema.required, SERVICE_ENDPOINT_FIELDS);
assertSameSet("serviceEndpoint.properties", Object.keys(endpointSchema.properties ?? {}), SERVICE_ENDPOINT_FIELDS);
assertSameSet("serviceEndpoint.protocol enum", endpointSchema.properties?.protocol?.enum, SERVICE_ENDPOINT_PROTOCOLS);
assertSameSet("serviceEndpoint.visibility enum", endpointSchema.properties?.visibility?.enum, SERVICE_ENDPOINT_VISIBILITIES);

const networkProtocolEnum = endpointSchema.oneOf?.[1]?.properties?.protocol?.enum;
assertSameSet(
  "serviceEndpoint network protocol enum",
  networkProtocolEnum,
  SERVICE_ENDPOINT_PROTOCOLS.filter((protocol) => protocol !== "local_socket")
);

if (endpointSchema.oneOf?.[0]?.properties?.protocol?.const !== "local_socket") {
  errors.push("serviceEndpoint local_socket oneOf branch must require protocol=local_socket");
}
if (endpointSchema.oneOf?.[0]?.properties?.host?.const !== "" || endpointSchema.oneOf?.[0]?.properties?.port?.const !== 0) {
  errors.push("serviceEndpoint local_socket oneOf branch must require empty host and port=0");
}
if (endpointSchema.oneOf?.[1]?.properties?.socket?.const !== "") {
  errors.push("serviceEndpoint network oneOf branch must require empty socket");
}

if (errors.length > 0) {
  console.error("registry schema parity check failed:");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("registry schema parity check passed");
