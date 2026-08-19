<template>
  <AdminLayout>
    <div class="drain-page">
      <div class="page-header">
        <div>
          <h2>Rollout / Drain 操作台</h2>
          <p>仅通过 admin-api 控制面操作已注册的 game-server 实例。此页面不提供停服、强制断线或发布收尾操作。</p>
        </div>
        <el-button :loading="loading" @click="refreshNow">刷新状态</el-button>
      </div>

      <el-alert
        v-if="loadError"
        type="warning"
        :title="loadError"
        :closable="false"
        show-icon
        class="page-alert"
      />

      <el-card class="target-card" v-loading="loading && !instances.length">
        <template #header>
          <div class="card-header">
            <span>目标实例</span>
            <el-tag v-if="selectedInstance" type="info" size="small">来自 Registry</el-tag>
          </div>
        </template>
        <div v-if="instances.length" class="target-form">
          <el-form label-position="top">
            <el-form-item label="game-server 实例">
              <el-select v-model="selectedInstance" filterable class="instance-select" :disabled="operation.loading">
                <el-option
                  v-for="instance in instances"
                  :key="instance.instanceId"
                  :label="`${instance.instanceId} · ${instance.status}`"
                  :value="instance.instanceId"
                />
              </el-select>
            </el-form-item>
            <el-form-item label="非敏感操作原因" required>
              <el-input
                v-model="reason"
                type="textarea"
                :rows="3"
                maxlength="256"
                show-word-limit
                placeholder="例如：例行版本切换前排空连接"
                :disabled="operation.loading"
              />
              <div v-if="reason && !safeReason" class="reason-error">原因不能为空、最多 256 个字符，且不得包含 token、密码或其他凭据。</div>
            </el-form-item>
          </el-form>
          <div class="action-row">
            <el-button
              type="warning"
              :loading="operation.loading && operation.targetEnabled === true"
              :disabled="operation.loading || !canSubmit"
              @click="submitDrain(true)"
            >
              开启排空
            </el-button>
            <el-button
              type="primary"
              plain
              :loading="operation.loading && operation.targetEnabled === false"
              :disabled="operation.loading || !canSubmit"
              @click="submitDrain(false)"
            >
              关闭排空
            </el-button>
          </div>
          <el-alert
            class="action-warning"
            type="warning"
            title="执行前会生成服务端影响预检；预检确认凭据只使用一次，失败或过期后请重新发起。"
            :closable="false"
            show-icon
          />
        </div>
        <el-empty v-else description="没有可操作的 game-server Registry 实例" />
      </el-card>

      <el-card class="status-card" v-loading="loading">
        <template #header>
          <div class="card-header">
            <span>目标状态与影响摘要</span>
            <span class="updated">{{ formatTime(lastUpdated) }}</span>
          </div>
        </template>
        <div v-if="selectedInstance" class="status-grid">
          <div class="status-item">
            <span class="label">实例连接数</span>
            <strong>{{ drainStatus.available ? drainStatus.connectionCount : "--" }}</strong>
            <small>{{ drainStatus.available ? "game-server 控面" : "控制面状态不可用" }}</small>
          </div>
          <div class="status-item" :class="{ unavailable: !drainStatus.available }">
            <span class="label">Owned rooms</span>
            <strong>{{ drainStatus.available ? drainStatus.ownedRoomCount : "--" }}</strong>
            <small>{{ drainStatus.available ? "当前持有房间" : "控制面状态不可用" }}</small>
          </div>
          <div class="status-item" :class="{ unavailable: !drainStatus.available }">
            <span class="label">Migrating rooms</span>
            <strong>{{ drainStatus.available ? drainStatus.migratingRoomCount : "--" }}</strong>
            <small>{{ drainStatus.available ? "迁移中房间" : "控制面状态不可用" }}</small>
          </div>
          <div class="status-item" :class="{ unavailable: !drainStatus.available }">
            <span class="label">Drain mode</span>
            <strong>{{ drainStatus.available ? (drainStatus.drainModeEnabled ? "开启" : "关闭") : "--" }}</strong>
            <small>{{ drainStatus.available ? "game-server 真实状态" : "控制面状态不可用" }}</small>
          </div>
        </div>
        <el-alert
          v-if="observation.routeBlockers"
          class="blocker-alert"
          :type="routeBlockerCount ? 'warning' : 'success'"
          :title="routeBlockerCount ? '存在路由阻塞，请在排空前处理' : '当前未观测到路由阻塞'"
          :closable="false"
        >
          <div class="blocker-list">
            <span>房间阻塞 {{ observation.routeBlockers.blockedRoomCount }}</span>
            <span>玩家阻塞 {{ observation.routeBlockers.blockedPlayerCount }}</span>
            <span>过期房间路由 {{ observation.routeBlockers.staleRoomRouteCount }}</span>
            <span>过期玩家路由 {{ observation.routeBlockers.stalePlayerRouteCount }}</span>
          </div>
        </el-alert>
        <div v-if="drainStatus.available" class="status-footnote">
          可接管空房 {{ drainStatus.transferableEmptyRoomCount }} · 已 retired {{ drainStatus.retiredRoomCount }} · 路由样本 {{ drainStatus.routeCount }}
        </div>
        <el-empty v-if="!selectedInstance" description="选择实例后查看状态" />
      </el-card>

      <el-alert
        v-if="operation.message"
        class="operation-alert"
        :type="operation.alertType"
        :title="operation.title"
        :closable="false"
        show-icon
      >
        <div>{{ operation.message }}</div>
        <div v-if="operation.requestId" class="request-id">请求 ID：{{ operation.requestId }}</div>
        <el-button
          v-if="operation.phase === 'approval_required' && operation.pendingPreflight"
          type="primary"
          size="small"
          :loading="operation.loading"
          @click="resumeApprovedDrain"
        >审批后确认执行</el-button>
      </el-alert>
    </div>
  </AdminLayout>
</template>

<script setup>
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import AdminLayout from "../../components/AdminLayout.vue";
import { monitoringApi, rolloutApi } from "../../api";
import { normalizeHighRiskError, formatHighRiskPreview, resumeHighRiskOperation, runHighRiskOperation } from "../../operations/high-risk";
import {
  gameServerInstances,
  normalizeDrainStatus,
  isSafeDrainReason,
  normalizeRolloutObservation,
  selectDefaultGameServerInstance
} from "../../operations/rollout-drain";
import { createSerialPoller } from "../../utils/serial-poller";

const instances = ref([]);
const selectedInstance = ref("");
const reason = ref("");
const loading = ref(false);
const loadError = ref("");
const lastUpdated = ref(null);
const source = reactive({ instances: null, services: null, rollout: null });
const drainStatus = reactive(normalizeDrainStatus(null, ""));
const operation = reactive({
  loading: false,
  targetEnabled: null,
  phase: "idle",
  title: "",
  message: "",
  requestId: "",
  alertType: "info",
  pendingPreflight: null,
  pendingPayload: null
});
let poller = null;

const observation = computed(() => normalizeRolloutObservation({
  services: source.services,
  rollout: source.rollout,
  instanceId: selectedInstance.value
}));
const routeBlockerCount = computed(() => {
  const blockers = observation.value.routeBlockers;
  return blockers
    ? blockers.blockedRoomCount + blockers.blockedPlayerCount + blockers.staleRoomRouteCount + blockers.stalePlayerRouteCount
    : 0;
});
const safeReason = computed(() => isSafeDrainReason(reason.value));
const canSubmit = computed(() => Boolean(selectedInstance.value && safeReason.value));

function formatTime(value) {
  return value ? new Date(value).toLocaleTimeString("zh-CN") : "等待数据";
}

async function fetchState({ signal } = {}) {
  loading.value = true;
  loadError.value = "";
  try {
    const [instancesResponse, rolloutResponse] = await Promise.all([
      rolloutApi.getInstances({ signal }),
      monitoringApi.getRolloutDrain({ signal }).catch(() => ({ data: null }))
    ]);
    if (signal?.aborted) return false;
    source.instances = instancesResponse.data;
    source.rollout = rolloutResponse.data;
    instances.value = gameServerInstances({ services: [{ name: "game-server", instances: source.instances?.instances || [] }] });
    if (!instances.value.some((instance) => instance.instanceId === selectedInstance.value)) {
      selectedInstance.value = selectDefaultGameServerInstance(instances.value);
    }
    if (selectedInstance.value) {
      const statusResponse = await rolloutApi.getDrainStatus(selectedInstance.value, { signal });
      Object.assign(drainStatus, normalizeDrainStatus(statusResponse.data, selectedInstance.value));
    }
    lastUpdated.value = Date.now();
    return true;
  } catch (error) {
    if (signal?.aborted) return false;
    loadError.value = error?.response?.data?.message || "无法加载 Registry 或监控状态。";
    return false;
  } finally {
    if (!signal?.aborted) loading.value = false;
  }
}

async function refreshNow() {
  poller?.trigger();
  if (!poller) await fetchState();
}

function resetOperation() {
  operation.loading = false;
  operation.targetEnabled = null;
}

async function resumeApprovedDrain() {
  if (operation.loading || !operation.pendingPreflight || !operation.pendingPayload) return;
  operation.loading = true;
  operation.phase = "preflight";
  operation.title = "审批已通过，确认执行";
  operation.message = "将复用原请求 ID、预检 nonce 和摘要哈希执行，不会创建新操作。";
  operation.alertType = "warning";
  try {
    const outcome = await resumeHighRiskOperation({
      invoke: (body) => rolloutApi.setDrain(operation.pendingPayload.instanceId, body),
      payload: operation.pendingPayload.payload,
      requestId: operation.pendingPayload.requestId,
      preflight: operation.pendingPreflight
    });
    operation.phase = outcome.phase;
    operation.requestId = outcome.requestId;
    if (outcome.phase !== "approval_required") {
      operation.pendingPreflight = null;
      operation.pendingPayload = null;
    }
    if (outcome.phase === "approval_required") {
      operation.title = "仍在等待独立审批";
      operation.message = "审批尚未完成，原预检凭据已保留，不会自动重试。";
      operation.alertType = "warning";
    } else {
      operation.title = outcome.phase === "execution_uncertain" ? "执行结果待核实" : "排空操作已提交";
      operation.message = outcome.phase === "execution_uncertain"
        ? "服务端无法确认最终结果，请以审计和监控状态为准。"
        : "审批后的原操作已提交执行。";
      operation.alertType = outcome.phase === "execution_uncertain" ? "warning" : "success";
      await fetchState();
    }
  } catch (error) {
    const normalized = normalizeHighRiskError(error);
    operation.phase = normalized.kind;
    operation.title = normalized.title;
    operation.message = normalized.description;
    operation.alertType = "warning";
  } finally {
    resetOperation();
  }
}

async function submitDrain(enabled) {
  if (!canSubmit.value || operation.loading) return;
  operation.loading = true;
  operation.targetEnabled = enabled;
  operation.phase = "preflight";
  operation.title = enabled ? "开启排空" : "关闭排空";
  operation.message = "正在生成服务端影响预检。";
  operation.alertType = "warning";
  operation.requestId = "";
  const requestReason = reason.value.trim();
  try {
    const outcome = await runHighRiskOperation({
      invoke: (body) => rolloutApi.setDrain(selectedInstance.value, body),
      payload: { enabled, reason: requestReason },
      confirm: async (preflight) => {
        operation.message = "已生成影响预览，等待明确确认。";
        try {
          await ElMessageBox.confirm(
            formatHighRiskPreview(preflight),
            enabled ? "开启排空确认" : "关闭排空确认",
            { type: enabled ? "warning" : "info", confirmButtonText: "确认执行", cancelButtonText: "取消" }
          );
          return true;
        } catch {
          return false;
        }
      }
    });
    operation.phase = outcome.phase;
    operation.requestId = outcome.requestId || "";
    if (outcome.phase === "approval_required") {
      operation.pendingPreflight = outcome.preflight;
      operation.pendingPayload = {
        instanceId: selectedInstance.value,
        requestId: outcome.requestId,
        payload: { enabled, reason: requestReason }
      };
      operation.title = "等待独立审批";
      operation.message = "操作已提交审批。审批通过后可在此复用同一预检凭据执行，不会自动重试。";
      operation.alertType = "warning";
    } else if (outcome.phase === "cancelled") {
      operation.title = "已取消预检";
      operation.message = "未执行任何服务端变更。";
      operation.alertType = "info";
    } else if (outcome.phase === "expired") {
      operation.title = "预检已过期";
      operation.message = "确认前预检已过期，请重新发起，不会自动重试。";
      operation.alertType = "warning";
    } else if (outcome.phase === "in_progress") {
      operation.title = "操作正在执行";
      operation.message = "服务端已接收该请求，请勿重复提交。";
      operation.alertType = "warning";
    } else if (outcome.phase === "execution_uncertain") {
      operation.title = "执行结果待核实";
      operation.message = "服务端无法确认最终结果，请以监控和审计状态为准，勿直接重试。";
      operation.alertType = "warning";
    } else if (outcome.phase === "terminal") {
      operation.title = "请求已返回终态";
      operation.message = "该请求已有终态记录，请查看审计记录确认结果。";
      operation.alertType = "info";
    } else {
      operation.title = enabled ? "排空开启请求已提交" : "排空关闭请求已提交";
      operation.message = "操作已由 admin-api 接收并记录审计。";
      operation.alertType = "success";
      await fetchState();
      ElMessage.success(operation.title);
    }
  } catch (error) {
    const normalized = normalizeHighRiskError(error);
    operation.phase = normalized.kind;
    operation.title = normalized.title;
    operation.message = normalized.description;
    operation.alertType = ["permission_denied", "execution_uncertain"].includes(normalized.kind) ? "warning" : "error";
  } finally {
    resetOperation();
  }
}

onMounted(() => {
  poller = createSerialPoller({ task: fetchState, intervalMs: 15000, maxIntervalMs: 60000 });
  poller.start();
});

onUnmounted(() => poller?.stop());
</script>

<style scoped>
.drain-page {
  max-width: 1180px;
  margin: 0 auto;
  padding: 24px;
}

.page-header,
.card-header,
.action-row,
.blocker-list {
  display: flex;
  align-items: center;
}

.page-header {
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 18px;
}

.page-header h2 {
  margin: 0 0 6px;
  font-size: 22px;
}

.page-header p {
  color: #606266;
  font-size: 13px;
  line-height: 1.6;
}

.page-alert,
.target-card,
.status-card,
.operation-alert {
  margin-bottom: 16px;
}

.card-header {
  justify-content: space-between;
  gap: 12px;
  font-weight: 600;
}

.updated {
  color: #909399;
  font-size: 12px;
  font-weight: 400;
}

.target-form {
  max-width: 720px;
}

.instance-select {
  width: min(100%, 440px);
}

.action-row {
  gap: 10px;
  flex-wrap: wrap;
  margin-top: 4px;
}

.action-warning {
  margin-top: 16px;
}

.reason-error {
  margin-top: 6px;
  color: #f56c6c;
  font-size: 12px;
  line-height: 1.4;
}

.status-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
  margin-bottom: 16px;
}

.status-item {
  min-width: 0;
  padding: 14px;
  border: 1px solid #ebeef5;
  border-radius: 6px;
  background: #f8fafc;
}

.status-item .label,
.status-item small {
  display: block;
  color: #909399;
  font-size: 12px;
}

.status-item strong {
  display: block;
  margin: 8px 0 4px;
  color: #303133;
  font-size: 22px;
}

.status-item.unavailable strong {
  color: #909399;
}

.blocker-alert {
  margin-top: 6px;
}

.blocker-list {
  gap: 16px;
  flex-wrap: wrap;
  line-height: 1.6;
}

.request-id {
  margin-top: 6px;
  color: #606266;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
  overflow-wrap: anywhere;
}

@media (max-width: 760px) {
  .drain-page {
    padding: 12px;
  }

  .page-header {
    align-items: flex-start;
    flex-direction: column;
  }

  .status-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 420px) {
  .status-grid {
    grid-template-columns: 1fr;
  }
}
</style>
