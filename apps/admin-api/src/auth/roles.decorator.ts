import { SetMetadata } from "@nestjs/common";

import type { AdminPolicyScopeRequest } from "./admin-policy.service.js";

export const PERMISSIONS_KEY = "permissions";
export const POLICY_PERMISSION_RESOLVER_KEY = "admin-policy-permission-resolver";
export const POLICY_SCOPE_RESOLVER_KEY = "admin-policy-scope-resolver";
export type AdminPermission =
  | "audit.read"
  | "assets.ledger.read"
  | "security.read"
  | "players.read"
  | "players.status.update"
  | "players.ban"
  | "gm.broadcast"
  | "gm.send_item"
  | "gm.asset_correction.emergency"
  | "gm.kick_player"
  | "gm.ban_player"
  | "gm.character_elements.write"
  | "gm.character_titles.write"
  | "gm.character_disciplines.write"
  | "maintenance.read"
  | "maintenance.write"
  | "monitoring.read"
  | "monitoring.archive"
  | "id.read"
  | "id.manage"
  | "myforge.agent.read"
  | "myforge.task.read"
  | "myforge.task.create"
  | "myforge.task.cancel"
  | "admins.revoke_tokens"
  | "admins.reset_password"
  | "admin.permissions.manage"
  | "breakglass.activate"
  | "game.config.write"
  | "game.room.transfer"
  | "proxy.maintenance.write"
  | "proxy.rollout.write"
  | "proxy.route.write"
  | "activities.read"
  | "activities.write"
  | "activities.publish"
  | "activities.offline"
  | "activities.records.read"
  | "service.shutdown";

export const Permissions = (...permissions: AdminPermission[]) => SetMetadata(PERMISSIONS_KEY, permissions);
export type AdminPermissionResolver = (request: any) => readonly AdminPermission[];
export const PermissionResolver = (resolver: AdminPermissionResolver) => SetMetadata(POLICY_PERMISSION_RESOLVER_KEY, resolver);
export type AdminPolicyScopeResolver = (
  request: any,
  permission: AdminPermission
) => AdminPolicyScopeRequest | Promise<AdminPolicyScopeRequest>;
export const PolicyScopeResolver = (resolver: AdminPolicyScopeResolver) => SetMetadata(POLICY_SCOPE_RESOLVER_KEY, resolver);
