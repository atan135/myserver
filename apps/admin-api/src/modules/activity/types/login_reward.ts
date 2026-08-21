import { LOGIN_REWARD_SCHEMA, validateLoginRewardConfig } from "../../../activity-types.js";

export type LoginRewardEventSource = "game_entry";
export type LoginRewardCycleUnit = "natural_day";
export type LoginRewardProgression = "consecutive" | "cumulative";
export type LoginRewardMissPolicy = "reset" | "carry";
export type LoginRewardClaimMode = "manual" | "automatic";

export interface LoginRewardStageConfig {
  stage_no: number;
  required_count: number;
  reward_group_key: string;
}

export interface LoginRewardConfig {
  schema_version: 1;
  event_source: LoginRewardEventSource;
  cycle_unit: LoginRewardCycleUnit;
  progression: LoginRewardProgression;
  miss_policy: LoginRewardMissPolicy;
  claim_mode: LoginRewardClaimMode;
  stages: LoginRewardStageConfig[];
}

export interface LoginRewardState {
  last_period_key?: string;
  consecutive_count: number;
  cumulative_count: number;
  claimed_stage_ids: string[];
}

export interface LoginRewardView {
  type: "login_reward";
  schema_version: 1;
  event_source: LoginRewardEventSource;
  cycle_unit: LoginRewardCycleUnit;
  progression: LoginRewardProgression;
  miss_policy: LoginRewardMissPolicy;
  claim_mode: LoginRewardClaimMode;
  stages: LoginRewardStageConfig[];
  state?: LoginRewardState;
  contract_only: true;
}

export const loginRewardSchema = LOGIN_REWARD_SCHEMA;

export function validateLoginReward(config: unknown): LoginRewardConfig {
  return validateLoginRewardConfig(config) as LoginRewardConfig;
}

export function buildLoginRewardView(config: LoginRewardConfig, state?: LoginRewardState): LoginRewardView {
  validateLoginReward(config);
  return { type: "login_reward", schema_version: 1, event_source: config.event_source, cycle_unit: config.cycle_unit, progression: config.progression, miss_policy: config.miss_policy, claim_mode: config.claim_mode, stages: config.stages.map((stage) => ({ ...stage })), state, contract_only: true };
}

export function parseLoginRewardState(value: unknown): LoginRewardState {
  const state = value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
  return {
    last_period_key: typeof state.last_period_key === "string" ? state.last_period_key : undefined,
    consecutive_count: Number.isInteger(state.consecutive_count) && Number(state.consecutive_count) >= 0 ? Number(state.consecutive_count) : 0,
    cumulative_count: Number.isInteger(state.cumulative_count) && Number(state.cumulative_count) >= 0 ? Number(state.cumulative_count) : 0,
    claimed_stage_ids: Array.isArray(state.claimed_stage_ids) ? state.claimed_stage_ids.filter((item): item is string => typeof item === "string") : []
  };
}

export const loginRewardHandler = Object.freeze({ type: "login_reward", schemaVersion: 1, supportedActions: ["list", "detail", "claim", "progress"] as const });
