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

export interface LoginRewardStageDisplay {
  title?: string;
  description?: string;
}

export interface LoginRewardStageEditorRow extends LoginRewardStageConfig {
  display?: LoginRewardStageDisplay;
  reward_group_exists: boolean;
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

export interface LoginRewardDetailView extends LoginRewardView {
  current_stage_id?: string;
  consecutive_days: number;
  cumulative_days: number;
  today_status: "logged_in" | "not_logged_in";
  stage_views: Array<LoginRewardStageEditorRow & { achieved: boolean; claimable: boolean; claimed: boolean }>;
}

export const loginRewardSchema = LOGIN_REWARD_SCHEMA;

export function validateLoginReward(config: unknown): LoginRewardConfig {
  return validateLoginRewardConfig(config) as LoginRewardConfig;
}

export function buildLoginRewardView(config: LoginRewardConfig, state?: LoginRewardState): LoginRewardView {
  validateLoginReward(config);
  return { type: "login_reward", schema_version: 1, event_source: config.event_source, cycle_unit: config.cycle_unit, progression: config.progression, miss_policy: config.miss_policy, claim_mode: config.claim_mode, stages: config.stages.map((stage) => ({ ...stage })), state, contract_only: true };
}

export function sortLoginRewardStages(stages: LoginRewardStageConfig[]): LoginRewardStageConfig[] {
  return stages.map((stage) => ({ ...stage })).sort((left, right) => left.stage_no - right.stage_no);
}

export function buildLoginRewardStageEditor(config: LoginRewardConfig, rewardGroupKeys: string[]): LoginRewardStageEditorRow[] {
  const groups = new Set(rewardGroupKeys);
  return sortLoginRewardStages(config.stages).map((stage) => ({ ...stage, reward_group_exists: groups.has(stage.reward_group_key) }));
}

export function buildLoginRewardDetailView(config: LoginRewardConfig, state: LoginRewardState = parseLoginRewardState(undefined), currentStageId?: string, activityVersion = config.schema_version, timezone = "UTC", now = new Date()): LoginRewardDetailView {
  validateLoginReward(config);
  const count = config.progression === "cumulative" ? state.cumulative_count : state.consecutive_count;
  const claimed = new Set(state.claimed_stage_ids);
  const stage_views = sortLoginRewardStages(config.stages).map((stage) => {
    const claimKey = `stage_id=${stage.stage_no};period_key=${state.last_period_key ?? ""};activity_version=${activityVersion}`;
    const achieved = count >= stage.required_count;
    return { ...stage, reward_group_exists: true, achieved, claimable: achieved && !claimed.has(claimKey), claimed: claimed.has(claimKey) };
  });
  const period = new Intl.DateTimeFormat("en-CA", { timeZone: timezone, year: "numeric", month: "2-digit", day: "2-digit" }).format(now);
  return { ...buildLoginRewardView(config, state), current_stage_id: currentStageId, consecutive_days: state.consecutive_count, cumulative_days: state.cumulative_count, today_status: state.last_period_key === period ? "logged_in" : "not_logged_in", stage_views };
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
