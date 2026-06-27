import { SetMetadata } from "@nestjs/common";

export const ROLES_KEY = "roles";
export const PERMISSIONS_KEY = "permissions";
export type AdminRole = "viewer" | "operator" | "admin" | "super_admin";
export type AdminPermission =
  | "audit.read"
  | "security.read"
  | "players.read"
  | "players.status.update"
  | "players.ban"
  | "gm.broadcast"
  | "gm.send_item"
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
  | "admins.revoke_tokens"
  | "admins.reset_password";

export const ROLE_PERMISSIONS: Record<AdminRole, readonly AdminPermission[] | "*"> = {
  viewer: [
    "audit.read",
    "security.read",
    "players.read",
    "maintenance.read",
    "monitoring.read",
    "id.read"
  ],
  operator: [
    "audit.read",
    "security.read",
    "players.read",
    "players.status.update",
    "maintenance.read",
    "monitoring.read",
    "id.read",
    "gm.broadcast",
    "gm.send_item",
    "gm.kick_player",
    "gm.character_elements.write",
    "gm.character_titles.write",
    "gm.character_disciplines.write"
  ],
  admin: "*",
  super_admin: "*"
};

export const Roles = (...roles: AdminRole[]) => SetMetadata(ROLES_KEY, roles);
export const Permissions = (...permissions: AdminPermission[]) => SetMetadata(PERMISSIONS_KEY, permissions);

export function roleHasPermission(role: unknown, permission: AdminPermission): boolean {
  if (typeof role !== "string" || !(role in ROLE_PERMISSIONS)) {
    return false;
  }

  const permissions = ROLE_PERMISSIONS[role as AdminRole];
  return permissions === "*" || permissions.includes(permission);
}

export function roleHasAllPermissions(role: unknown, permissions: readonly AdminPermission[]): boolean {
  return permissions.every((permission) => roleHasPermission(role, permission));
}
