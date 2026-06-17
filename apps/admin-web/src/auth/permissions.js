export const ADMIN_PERMISSIONS = Object.freeze({
  AUDIT_READ: "audit.read",
  SECURITY_READ: "security.read",
  PLAYERS_READ: "players.read",
  PLAYERS_STATUS_UPDATE: "players.status.update",
  PLAYERS_BAN: "players.ban",
  GM_BROADCAST: "gm.broadcast",
  GM_SEND_ITEM: "gm.send_item",
  GM_KICK_PLAYER: "gm.kick_player",
  GM_BAN_PLAYER: "gm.ban_player",
  MAINTENANCE_READ: "maintenance.read",
  MAINTENANCE_WRITE: "maintenance.write",
  MONITORING_READ: "monitoring.read",
  MONITORING_ARCHIVE: "monitoring.archive",
  ID_READ: "id.read",
  ID_MANAGE: "id.manage",
  ADMINS_REVOKE_TOKENS: "admins.revoke_tokens",
  ADMINS_RESET_PASSWORD: "admins.reset_password"
});

export const ALL_ADMIN_PERMISSIONS = Object.freeze(Object.values(ADMIN_PERMISSIONS));

export const ROLE_PERMISSIONS = Object.freeze({
  viewer: Object.freeze([
    ADMIN_PERMISSIONS.AUDIT_READ,
    ADMIN_PERMISSIONS.SECURITY_READ,
    ADMIN_PERMISSIONS.PLAYERS_READ,
    ADMIN_PERMISSIONS.MAINTENANCE_READ,
    ADMIN_PERMISSIONS.MONITORING_READ,
    ADMIN_PERMISSIONS.ID_READ
  ]),
  operator: Object.freeze([
    ADMIN_PERMISSIONS.AUDIT_READ,
    ADMIN_PERMISSIONS.SECURITY_READ,
    ADMIN_PERMISSIONS.PLAYERS_READ,
    ADMIN_PERMISSIONS.PLAYERS_STATUS_UPDATE,
    ADMIN_PERMISSIONS.GM_BROADCAST,
    ADMIN_PERMISSIONS.GM_SEND_ITEM,
    ADMIN_PERMISSIONS.GM_KICK_PLAYER,
    ADMIN_PERMISSIONS.MAINTENANCE_READ,
    ADMIN_PERMISSIONS.MONITORING_READ,
    ADMIN_PERMISSIONS.ID_READ
  ]),
  admin: ALL_ADMIN_PERMISSIONS,
  super_admin: ALL_ADMIN_PERMISSIONS
});

export function permissionsForRole(role) {
  return ROLE_PERMISSIONS[role] || Object.freeze([]);
}

export function hasPermission(userOrRole, permission) {
  if (!permission) {
    return true;
  }

  const role = typeof userOrRole === "string" ? userOrRole : userOrRole?.role;
  return permissionsForRole(role).includes(permission);
}

export function hasAnyPermission(userOrRole, permissions = []) {
  if (!permissions.length) {
    return true;
  }

  return permissions.some((permission) => hasPermission(userOrRole, permission));
}
