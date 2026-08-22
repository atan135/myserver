import { LOGIN_REWARD_SCHEMA } from "../../api/activity-types.js";
import { LOTTERY_SCHEMA } from "../../api/activity-types.js";

const definitions = {
  login_reward: Object.freeze({
    type: "login_reward",
    label: "登录奖励",
    schema: LOGIN_REWARD_SCHEMA,
    createDraft: (key) => ({
      publicConfig: { title: key },
      typeConfig: { schema_version: 1, event_source: "game_entry", cycle_unit: "natural_day", progression: "consecutive", miss_policy: "reset", claim_mode: "manual", stages: [{ stage_no: 1, required_count: 1, reward_group_key: "default" }] },
      stages: [{ stageId: "stage-1", stageNo: 1, rewardGroupKey: "default", qualification: {} }],
      rewardGroups: [{ key: "default", selectionMode: "fixed", items: [{ item_id: 1001, quantity: 1 }] }]
    }),
    editor: () => import("./LoginRewardTypeEditor.vue"),
    module: () => import("./types/login_reward.ts")
  }),
  lottery: Object.freeze({
    type: "lottery",
    label: "随机抽奖",
    schema: LOTTERY_SCHEMA,
    createDraft: (key) => ({
      publicConfig: { title: key },
      typeConfig: { schema_version: 1, draw_source: "player_action", pool_version: 1, free_draw_count: 0, daily_draw_limit: 1, total_draw_limit: 1, pool_items: [{ item_id: 1001, quantity: 1, weight: 1 }] },
      stages: [],
      rewardGroups: [{ key: "default", selectionMode: "fixed", items: [{ item_id: 1001, quantity: 1 }] }]
    }),
    editor: () => import("./LotteryTypeEditor.vue"),
    module: () => import("./types/lottery.ts")
  })
};

export function listActivityTypeDefinitions() {
  return Object.values(definitions);
}

export function resolveActivityTypeDefinition(type) {
  return definitions[String(type)] || null;
}

export function buildActivityDraftTemplate(type, key) {
  const definition = resolveActivityTypeDefinition(type);
  if (!definition) throw Object.assign(new Error(`unknown activity type: ${type}`), { code: "ACTIVITY_UNKNOWN_TYPE" });
  return definition.createDraft(String(key || "activity"));
}

export async function loadActivityTypeEditor(type) {
  const definition = resolveActivityTypeDefinition(type);
  if (!definition) throw Object.assign(new Error(`unknown activity type: ${type}`), { code: "ACTIVITY_UNKNOWN_TYPE" });
  await definition.module();
  const module = await definition.editor();
  return module.default || module;
}

export const ACTIVITY_TYPE_REGISTRY = Object.freeze(definitions);
