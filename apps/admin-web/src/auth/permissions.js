export const ADMIN_PERMISSIONS = Object.freeze({
  AUDIT_READ: "audit.read",
  ASSET_LEDGER_READ: "assets.ledger.read",
  SECURITY_READ: "security.read",
  PLAYERS_READ: "players.read",
  PLAYERS_STATUS_UPDATE: "players.status.update",
  PLAYERS_BAN: "players.ban",
  GM_BROADCAST: "gm.broadcast",
  GM_SEND_ITEM: "gm.send_item",
  GM_ASSET_CORRECTION_EMERGENCY: "gm.asset_correction.emergency",
  GM_KICK_PLAYER: "gm.kick_player",
  GM_BAN_PLAYER: "gm.ban_player",
  GM_CHARACTER_ELEMENTS_WRITE: "gm.character_elements.write",
  GM_CHARACTER_TITLES_WRITE: "gm.character_titles.write",
  GM_CHARACTER_DISCIPLINES_WRITE: "gm.character_disciplines.write",
  MAINTENANCE_READ: "maintenance.read",
  MAINTENANCE_WRITE: "maintenance.write",
  MONITORING_READ: "monitoring.read",
  MONITORING_ARCHIVE: "monitoring.archive",
  ID_READ: "id.read",
  ID_MANAGE: "id.manage",
  MYFORGE_AGENT_READ: "myforge.agent.read",
  MYFORGE_TASK_READ: "myforge.task.read",
  MYFORGE_TASK_CREATE: "myforge.task.create",
  MYFORGE_TASK_CANCEL: "myforge.task.cancel",
  ADMINS_REVOKE_TOKENS: "admins.revoke_tokens",
  ADMINS_RESET_PASSWORD: "admins.reset_password"
});

export const ALL_ADMIN_PERMISSIONS = Object.freeze(Object.values(ADMIN_PERMISSIONS));

export const MYFORGE_ENTRY_PERMISSIONS = Object.freeze([
  ADMIN_PERMISSIONS.MYFORGE_AGENT_READ,
  ADMIN_PERMISSIONS.MYFORGE_TASK_READ
]);

export function effectivePermissions(user) {
  if (!Array.isArray(user?.permissions)) return Object.freeze([]);
  return Object.freeze([...new Set(user.permissions.filter((permission) =>
    typeof permission === "string" && ALL_ADMIN_PERMISSIONS.includes(permission)
  ))]);
}

export function hasPermission(user, permission) {
  if (!permission) {
    return true;
  }
  return effectivePermissions(user).includes(permission);
}

export function hasAnyPermission(user, permissions = []) {
  if (!permissions.length) {
    return true;
  }
  return permissions.some((permission) => hasPermission(user, permission));
}
