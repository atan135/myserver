export const SALT_ROUNDS = 10;
export const MAINTENANCE_STATE_KEY = "maintenance:global";
export const UNIQUE_VIOLATION = "23505";
export const ELEMENT_KEYS = ["earth", "fire", "water", "wind"];
export const AFFINITY_TOTAL = 10000;
export const DISCIPLINE_TIER_ORDER = [
  "novice",
  "apprentice",
  "adept",
  "expert",
  "master",
  "grandmaster"
];
export const BOOTSTRAP_POLICY_SCOPE = Object.freeze({
  world_ids: ["*"],
  service_names: ["*"],
  instance_ids: ["*"],
  field_allowlist: ["*"],
  target_types: ["*"],
  target_ids: ["*"],
  max_targets: 10000
});
