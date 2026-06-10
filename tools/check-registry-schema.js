import fs from "node:fs";
import path from "node:path";
import {
  SERVICE_INSTANCE_FIELDS,
  createServiceInstancePayload,
  pickServiceInstance,
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

const picked = pickServiceInstance([
  { ...sample, id: "unhealthy", healthy: false, weight: 1000 },
  { ...sample, id: "healthy-low", weight: 1 },
  { ...sample, id: "healthy-high", weight: 100 }
]);
if (!picked || picked.healthy === false) {
  errors.push("pickServiceInstance returned an unhealthy or empty instance");
}

if (errors.length > 0) {
  console.error("registry schema check failed:");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("registry schema check passed");
