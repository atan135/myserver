import { MAINTENANCE_STATE_KEY } from "./admin-store/constants.js";
import { hashPassword, verifyPassword, hashToken } from "./admin-store/crypto.js";
import {
  maintenanceStateKey,
  normalizeMaintenanceState,
  parseMaintenanceState
} from "./admin-store/maintenance-utils.js";
import { MaintenanceStore } from "./admin-store/stores/maintenance-store.js";
import { AssetStore } from "./admin-store/stores/asset-store.js";
import { PlayerStore } from "./admin-store/stores/player-store.js";
import { AuthStore } from "./admin-store/stores/auth-store.js";
import { AuditStore } from "./admin-store/stores/audit-store.js";
import { WorldStore } from "./admin-store/stores/world-store.js";
import { PolicyStore } from "./admin-store/stores/policy-store.js";
import { OperationStore } from "./admin-store/stores/operation-store.js";
import { CharacterStore } from "./admin-store/stores/character-store.js";

export class AdminStore {
  constructor(pool, redis = null, config = {}, gamePool = null) {
    this.pool = pool;
    this.gamePool = gamePool || pool;
    this.redis = redis;
    this.redisKeyPrefix = config.redisKeyPrefix || "";
    this.maintenance = new MaintenanceStore(pool, redis, this.redisKeyPrefix);
    this.asset = new AssetStore(this.gamePool);
    this.player = new PlayerStore(pool);
    this.auth = new AuthStore(pool);
    this.audit = new AuditStore(pool);
    this.world = new WorldStore(pool);
    this.operation = new OperationStore(pool);
    this.policy = new PolicyStore(pool, this.operation);
    this.character = new CharacterStore(pool, this.gamePool);
  }

  // ============================================================
  // Maintenance Mode
  // ============================================================

  async setMaintenanceMode(...args) { return this.maintenance.setMaintenanceMode(...args); }
  async getMaintenanceStatus(...args) { return this.maintenance.getMaintenanceStatus(...args); }
  async getAssetLedger(...args) { return this.asset.getAssetLedger(...args); }
  async countAssetLedger(...args) { return this.asset.countAssetLedger(...args); }
  async findPlayerById(...args) { return this.player.findPlayerById(...args); }
  async findPlayers(...args) { return this.player.findPlayers(...args); }
  async countPlayers(...args) { return this.player.countPlayers(...args); }
  async updatePlayerStatus(...args) { return this.player.updatePlayerStatus(...args); }
  async verifyPassword(...args) { return this.auth.verifyPassword(...args); }
  async findAdminByUsername(...args) { return this.auth.findAdminByUsername(...args); }
  async findAdminById(...args) { return this.auth.findAdminById(...args); }
  async createAdmin(...args) { return this.auth.createAdmin(...args); }
  async createAdminWithClient(...args) { return this.auth.createAdminWithClient(...args); }
  async ensureInitialAdmin(...args) { return this.auth.ensureInitialAdmin(...args); }
  async grantBootstrapAdminRole(...args) { return this.auth.grantBootstrapAdminRole(...args); }
  async grantBootstrapAdminRoleInTransaction(...args) { return this.auth.grantBootstrapAdminRoleInTransaction(...args); }
  async updateLastLogin(...args) { return this.auth.updateLastLogin(...args); }
  async updateAdminPassword(...args) { return this.auth.updateAdminPassword(...args); }
  async appendAuditLog(...args) { return this.audit.appendAuditLog(...args); }
  async appendSecurityAuditLog(...args) { return this.audit.appendSecurityAuditLog(...args); }
  async getSecurityLogs(...args) { return this.audit.getSecurityLogs(...args); }
  async countSecurityLogs(...args) { return this.audit.countSecurityLogs(...args); }
  async getAuditLogs(...args) { return this.audit.getAuditLogs(...args); }
  async countAuditLogs(...args) { return this.audit.countAuditLogs(...args); }
  async countRecentAdminAuditActions(...args) { return this.audit.countRecentAdminAuditActions(...args); }
  async findIdOrigin(...args) { return this.world.findIdOrigin(...args); }
  async findWorldMembershipAt(...args) { return this.world.findWorldMembershipAt(...args); }
  async findCurrentWorldMembership(...args) { return this.world.findCurrentWorldMembership(...args); }
  async findMergeContext(...args) { return this.world.findMergeContext(...args); }
  async findIdOrigins(...args) { return this.world.findIdOrigins(...args); }
  async countIdOrigins(...args) { return this.world.countIdOrigins(...args); }
  async findWorlds(...args) { return this.world.findWorlds(...args); }
  async countWorlds(...args) { return this.world.countWorlds(...args); }
  async findWorldMergeEvents(...args) { return this.world.findWorldMergeEvents(...args); }
  async countWorldMergeEvents(...args) { return this.world.countWorldMergeEvents(...args); }

  async findAdminPolicyPermission(...args) { return this.policy.findAdminPolicyPermission(...args); }
  async listEffectiveAdminPolicyGrants(...args) { return this.policy.listEffectiveAdminPolicyGrants(...args); }
  async grantAdminPermission(...args) { return this.policy.grantAdminPermission(...args); }
  async grantAdminRole(...args) { return this.policy.grantAdminRole(...args); }
  async revokeAdminPermission(...args) { return this.policy.revokeAdminPermission(...args); }
  async revokeAdminRole(...args) { return this.policy.revokeAdminRole(...args); }
  async createAdminBreakglassGrant(...args) { return this.policy.createAdminBreakglassGrant(...args); }
  async revokeAdminBreakglassGrant(...args) { return this.policy.revokeAdminBreakglassGrant(...args); }
  async listActiveAdminBreakglassGrants(...args) { return this.policy.listActiveAdminBreakglassGrants(...args); }
  async insertAdminOperationAuditEvent(...args) { return this.operation.insertAdminOperationAuditEvent(...args); }
  async reserveAdminOperationPreflight(...args) { return this.operation.reserveAdminOperationPreflight(...args); }
  async getAdminOperationByRequestId(...args) { return this.operation.getAdminOperationByRequestId(...args); }
  async listPendingAdminOperations(...args) { return this.operation.listPendingAdminOperations(...args); }
  async claimAdminOperationExecution(...args) { return this.operation.claimAdminOperationExecution(...args); }
  async completeAdminOperation(...args) { return this.operation.completeAdminOperation(...args); }
  async markAdminOperationExecutionUncertain(...args) { return this.operation.markAdminOperationExecutionUncertain(...args); }
  async decideAdminOperationApproval(...args) { return this.operation.decideAdminOperationApproval(...args); }
  async listAdminOperationAuditEvents(...args) { return this.operation.listAdminOperationAuditEvents(...args); }
  async findCharacterById(...args) { return this.character.findCharacterById(...args); }
  async findCharactersByAccountPlayerId(...args) { return this.character.findCharactersByAccountPlayerId(...args); }
  async countCharactersByAccountPlayerId(...args) { return this.character.countCharactersByAccountPlayerId(...args); }
  async findCharacterElementLogs(...args) { return this.character.findCharacterElementLogs(...args); }
  async findCharacterDisciplineLogs(...args) { return this.character.findCharacterDisciplineLogs(...args); }
  async findCharacterProfileOverview(...args) { return this.character.findCharacterProfileOverview(...args); }
  async withGameTransaction(...args) { return this.character.withGameTransaction(...args); }
  async setCharacterElementsForAdmin(...args) { return this.character.setCharacterElementsForAdmin(...args); }
  async applyCharacterTitleForAdmin(...args) { return this.character.applyCharacterTitleForAdmin(...args); }
  async setCharacterDisciplineForAdmin(...args) { return this.character.setCharacterDisciplineForAdmin(...args); }
  async runCharacterUnlockCheckForAdmin(...args) { return this.character.runCharacterUnlockCheckForAdmin(...args); }
  async restoreCharacterForAdmin(...args) { return this.character.restoreCharacterForAdmin(...args); }
  async findCharacterTitleOverview(...args) { return this.character.findCharacterTitleOverview(...args); }
}

export {
  MAINTENANCE_STATE_KEY,
  hashPassword,
  maintenanceStateKey,
  normalizeMaintenanceState,
  parseMaintenanceState,
  verifyPassword,
  hashToken
};

