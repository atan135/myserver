<template>
  <AdminLayout>
    <div class="monitoring">
      <div class="header">
        <div class="header-left">
          <h2>服务监控</h2>
          <el-button
            v-if="authStore.hasPermission(P.MONITORING_ARCHIVE)"
            type="primary"
            size="small"
            :loading="archiveLoading"
            @click="handleArchive"
          >
            手动归档
          </el-button>
        </div>
        <div class="window-selector">
          <el-radio-group v-model="currentWindow" size="small">
            <el-radio-button value="1m">1分钟</el-radio-button>
            <el-radio-button value="5m">5分钟</el-radio-button>
            <el-radio-button value="15m">15分钟</el-radio-button>
            <el-radio-button value="1h">1小时</el-radio-button>
          </el-radio-group>
        </div>
      </div>

      <el-card class="rollout-card" :class="`rollout-card-${rolloutDrain.status}`" v-loading="rolloutDrain.loading">
        <template #header>
          <div class="rollout-header">
            <div>
              <span class="rollout-title">Rollout / Drain 状态</span>
              <span class="rollout-subtitle">game-proxy 控制面</span>
            </div>
            <el-tag :type="rolloutTagType" size="small">{{ rolloutStatusText }}</el-tag>
          </div>
        </template>

        <div v-if="rolloutDrain.data" class="rollout-content">
          <div class="rollout-summary">
            <div class="rollout-alert">
              <span class="alert-dot" :class="`alert-${rolloutDrain.data.alert_level}`"></span>
              <span>{{ rolloutDrain.data.alert_message }}</span>
            </div>
            <span class="rollout-updated">更新 {{ formatTimestamp(rolloutDrain.data.updated_at) }}</span>
          </div>

          <div v-if="rolloutDrain.data.rollout" class="rollout-meta">
            <div class="rollout-meta-item">
              <span class="label">Epoch</span>
              <span class="value">{{ rolloutDrain.data.rollout.epoch || "--" }}</span>
            </div>
            <div class="rollout-meta-item">
              <span class="label">Old</span>
              <span class="value">{{ rolloutDrain.data.rollout.old_server || "--" }}</span>
            </div>
            <div class="rollout-meta-item">
              <span class="label">New</span>
              <span class="value">{{ rolloutDrain.data.rollout.new_server || "--" }}</span>
            </div>
            <div class="rollout-meta-item">
              <span class="label">State</span>
              <span class="value">{{ rolloutDrain.data.rollout.state || "--" }}</span>
            </div>
          </div>

          <div v-if="rolloutDrain.data.active" class="blocker-grid">
            <div class="blocker-item">
              <span class="label">旧服房间阻塞</span>
              <span class="value">{{ rolloutDrain.data.blockers.blocked_room_count }}</span>
            </div>
            <div class="blocker-item">
              <span class="label">旧服玩家阻塞</span>
              <span class="value">{{ rolloutDrain.data.blockers.blocked_player_count }}</span>
            </div>
            <div class="blocker-item">
              <span class="label">过期房间路由</span>
              <span class="value">{{ rolloutDrain.data.blockers.stale_room_route_count }}</span>
            </div>
            <div class="blocker-item">
              <span class="label">过期玩家路由</span>
              <span class="value">{{ rolloutDrain.data.blockers.stale_player_route_count }}</span>
            </div>
          </div>

          <div v-if="hasRolloutSamples" class="rollout-samples">
            <div v-if="rolloutDrain.data.blockers.blocked_room_samples.length" class="sample-row">
              <span class="label">房间样本</span>
              <span class="sample-list">{{ rolloutDrain.data.blockers.blocked_room_samples.join(", ") }}</span>
            </div>
            <div v-if="rolloutDrain.data.blockers.blocked_player_samples.length" class="sample-row">
              <span class="label">玩家样本</span>
              <span class="sample-list">{{ rolloutDrain.data.blockers.blocked_player_samples.join(", ") }}</span>
            </div>
          </div>

          <div v-if="rolloutDrain.data.instances.length" class="rollout-instances">
            <div
              v-for="instance in rolloutDrain.data.instances"
              :key="instance.instance_id || `${instance.endpoint.host}:${instance.endpoint.port}`"
              class="rollout-instance"
            >
              <div class="rollout-instance-main">
                <span class="instance-name">{{ instance.instance_id || "unknown" }}</span>
                <span class="instance-endpoint">{{ instance.endpoint.host }}:{{ instance.endpoint.port }}</span>
              </div>
              <el-tag :type="rolloutInstanceTagType(instance)" size="small">
                {{ instance.status }}
              </el-tag>
            </div>
          </div>

          <div v-if="rolloutDrain.data.error" class="rollout-error">
            {{ rolloutDrain.data.error }}：{{ rolloutDrain.data.message }}
          </div>
        </div>
        <div v-else class="rollout-placeholder">等待控制面状态...</div>
      </el-card>

      <el-card class="registry-card" v-loading="registry.loading">
        <template #header>
          <div class="registry-header">
            <div>
              <span class="registry-title">Registry 服务发现</span>
              <span class="registry-subtitle">实例、Endpoint 与 Heartbeat TTL</span>
            </div>
            <span class="registry-updated">更新 {{ formatTimestamp(registry.data?.checked_at) }}</span>
          </div>
        </template>

        <el-table
          :data="registryServices"
          class="registry-table"
          row-key="name"
          size="small"
          empty-text="暂无 registry 数据"
        >
          <el-table-column type="expand">
            <template #default="{ row }">
              <div class="registry-instances">
                <el-table
                  :data="row.instances"
                  row-key="instance_id"
                  size="small"
                  empty-text="暂无健康 registry 实例"
                >
                  <el-table-column prop="instance_id" label="Instance" min-width="180" show-overflow-tooltip />
                  <el-table-column label="Heartbeat TTL" width="130">
                    <template #default="{ row: instance }">
                      <span>{{ formatHeartbeatTtl(instance) }}</span>
                    </template>
                  </el-table-column>
                  <el-table-column label="状态" width="100">
                    <template #default="{ row: instance }">
                      <el-tag :type="heartbeatTagType(instance)" size="small">
                        {{ heartbeatStatusText(instance) }}
                      </el-tag>
                    </template>
                  </el-table-column>
                  <el-table-column label="最后注册" width="150">
                    <template #default="{ row: instance }">
                      {{ formatDateTime(instance.last_registered_at || instance.registered_at) }}
                    </template>
                  </el-table-column>
                  <el-table-column label="Endpoints" min-width="320">
                    <template #default="{ row: instance }">
                      <div class="endpoint-list">
                        <div
                          v-for="endpoint in instance.endpoints"
                          :key="`${endpoint.name}:${endpoint.protocol}:${endpoint.host}:${endpoint.port}:${endpoint.socket}`"
                          class="endpoint-item"
                        >
                          <span class="endpoint-name">{{ endpoint.name }}</span>
                          <span class="endpoint-protocol">{{ endpoint.protocol }}</span>
                          <span class="endpoint-address">{{ formatEndpoint(endpoint) }}</span>
                          <span class="endpoint-visibility">{{ endpoint.visibility }}</span>
                        </div>
                        <span v-if="!instance.endpoints.length" class="empty-text">--</span>
                      </div>
                    </template>
                  </el-table-column>
                </el-table>
              </div>
            </template>
          </el-table-column>
          <el-table-column prop="name" label="服务" min-width="160" show-overflow-tooltip />
          <el-table-column label="状态" width="110">
            <template #default="{ row }">
              <el-tag :type="registryServiceTagType(row)" size="small">
                {{ registryServiceStatusText(row) }}
              </el-tag>
            </template>
          </el-table-column>
          <el-table-column label="实例" width="120">
            <template #default="{ row }">
              {{ row.healthy_instance_count }}/{{ row.instance_count }}
            </template>
          </el-table-column>
          <el-table-column label="Endpoint 摘要" min-width="260">
            <template #default="{ row }">
              <span class="registry-summary">{{ endpointSummary(row) }}</span>
            </template>
          </el-table-column>
          <el-table-column label="最后注册" width="150">
            <template #default="{ row }">
              {{ formatDateTime(lastRegisteredAt(row)) }}
            </template>
          </el-table-column>
        </el-table>
      </el-card>

      <div class="services-grid">
        <el-card
          v-for="service in services"
          :key="service.name"
          class="service-card"
          :class="{ offline: service.status === 'offline' }"
          @click="goToDetail(service.name)"
        >
          <template #header>
            <div class="card-header">
              <span class="service-name">{{ service.name }}</span>
              <el-tag :type="service.status === 'online' ? 'success' : 'danger'" size="small">
                {{ service.status === 'online' ? '在线' : '离线' }}
              </el-tag>
            </div>
          </template>
          <div class="card-content">
            <div class="metric">
              <span class="label">QPS</span>
              <span class="value">{{ service.status === 'online' ? service.qps : '--' }}</span>
            </div>
            <div class="metric">
              <span class="label">延迟</span>
              <span class="value" :class="{ warning: service.latency_ms > 500 }">
                {{ service.status === 'online' ? service.latency_ms + 'ms' : '--' }}
              </span>
            </div>
            <div class="metric" v-if="service.online_value !== undefined">
              <span class="label">{{ onlineLabel(service.name) }}</span>
              <span class="value">
                {{ service.status === 'online' ? service.online_value : '--' }}
              </span>
            </div>
            <div class="metric metric-secondary" v-if="secondaryMetric(service)">
              <span class="label">{{ secondaryMetric(service).label }}</span>
              <span class="value value-secondary">
                {{ service.status === 'online' ? secondaryMetric(service).value : '--' }}
              </span>
            </div>
          </div>
        </el-card>
      </div>
    </div>
  </AdminLayout>
</template>

<script setup>
import { computed, reactive, ref, onMounted, onUnmounted } from "vue";
import { useRouter } from "vue-router";
import { ElMessage } from "element-plus";
import AdminLayout from "../../components/AdminLayout.vue";
import { monitoringApi } from "../../api";
import { useAuthStore } from "../../stores/auth";
import { ADMIN_PERMISSIONS as P } from "../../auth/permissions";

const router = useRouter();
const authStore = useAuthStore();
const services = ref([]);
const currentWindow = ref("5m");
const archiveLoading = ref(false);
const rolloutDrain = reactive({
  loading: false,
  data: null,
  status: "loading"
});
const registry = reactive({
  loading: false,
  data: null
});
let pollTimer = null;

const SERVICE_ONLINE_LABELS = {
  "auth-http": "唯一玩家",
  "game-server": "在线玩家",
  "game-proxy": "连接数",
  "chat-server": "在线玩家",
  "match-service": "匹配池",
  "announce-service": null,
  "mail-service": null,
  "admin-api": null
};

function onlineLabel(serviceName) {
  return SERVICE_ONLINE_LABELS[serviceName] || "在线";
}

function secondaryMetric(service) {
  if (service.name === "auth-http") {
    return {
      label: "5 分钟活跃会话",
      value: service.active_sessions_5m ?? 0
    };
  }

  return null;
}

const rolloutStatusText = computed(() => {
  const status = rolloutDrain.data?.status || rolloutDrain.status;
  const labels = {
    loading: "加载中",
    empty: "无进行中",
    blocked: "阻塞中",
    drained: "已排空",
    interrupted: "已中断",
    error: "不可达"
  };
  return labels[status] || status;
});

const rolloutTagType = computed(() => {
  const level = rolloutDrain.data?.alert_level;
  if (level === "critical") {
    return "danger";
  }
  if (level === "warning") {
    return "warning";
  }
  return "info";
});

const hasRolloutSamples = computed(() => {
  const blockers = rolloutDrain.data?.blockers;
  return Boolean(blockers?.blocked_room_samples?.length || blockers?.blocked_player_samples?.length);
});

const registryServices = computed(() => registry.data?.services || []);

function rolloutInstanceTagType(instance) {
  if (instance.alert_level === "critical") {
    return "danger";
  }
  if (instance.alert_level === "warning") {
    return "warning";
  }
  return "info";
}

async function fetchServices() {
  try {
    const response = await monitoringApi.getServices();
    if (response.data.ok !== false) {
      services.value = response.data.services || [];
    }
  } catch (error) {
    console.error("Failed to fetch services:", error);
  }
}

async function fetchRolloutDrain() {
  rolloutDrain.loading = !rolloutDrain.data;
  try {
    const response = await monitoringApi.getRolloutDrain();
    rolloutDrain.data = normalizeRolloutDrain(response.data);
    rolloutDrain.status = rolloutDrain.data.status;
  } catch (error) {
    console.error("Failed to fetch rollout drain:", error);
    rolloutDrain.data = normalizeRolloutDrain({
      ok: false,
      updated_at: Date.now(),
      status: "error",
      alert_level: "critical",
      alert_message: "控制面不可达",
      error: error.response?.data?.error || "ADMIN_API_UNAVAILABLE",
      message: error.response?.data?.message || error.message
    });
    rolloutDrain.status = "error";
  } finally {
    rolloutDrain.loading = false;
  }
}

async function fetchRegistry() {
  registry.loading = !registry.data;
  try {
    const response = await monitoringApi.getRegistry();
    registry.data = {
      checked_at: response.data?.checked_at || Date.now(),
      services: Array.isArray(response.data?.services) ? response.data.services : []
    };
  } catch (error) {
    console.error("Failed to fetch registry:", error);
    registry.data = {
      checked_at: Date.now(),
      services: []
    };
  } finally {
    registry.loading = false;
  }
}

function normalizeRolloutDrain(data) {
  const blockers = data?.blockers || {};
  return {
    ok: data?.ok !== false,
    updated_at: data?.updated_at || data?.checked_at || Date.now(),
    active: Boolean(data?.active),
    status: data?.status || "error",
    alert_level: data?.alert_level || "critical",
    alert_message: data?.alert_message || "控制面状态异常",
    error: data?.error || "",
    message: data?.message || "",
    rollout: data?.rollout || null,
    instances: Array.isArray(data?.instances) ? data.instances : [],
    blockers: {
      blocked_room_count: blockers.blocked_room_count || 0,
      blocked_player_count: blockers.blocked_player_count || 0,
      stale_room_route_count: blockers.stale_room_route_count || 0,
      stale_player_route_count: blockers.stale_player_route_count || 0,
      blocked_room_samples: blockers.blocked_room_samples || [],
      blocked_player_samples: blockers.blocked_player_samples || []
    }
  };
}

function formatTimestamp(timestamp) {
  if (!timestamp) {
    return "--";
  }

  return new Date(timestamp).toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit"
  });
}

function formatDateTime(timestamp) {
  if (!timestamp) {
    return "--";
  }

  return new Date(timestamp).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit"
  });
}

function formatHeartbeatTtl(instance) {
  const ttl = instance.heartbeat_ttl_seconds;
  if (ttl === null || ttl === undefined) {
    return "unknown";
  }
  if (ttl > 0) {
    return `${ttl}s`;
  }
  return instance.heartbeat_status || "missing";
}

function heartbeatStatusText(instance) {
  const labels = {
    alive: "正常",
    missing: "缺失",
    no_expire: "无过期",
    unknown: "未知"
  };
  return labels[instance.heartbeat_status] || instance.heartbeat_status || "未知";
}

function heartbeatTagType(instance) {
  if (instance.heartbeat_status === "alive" && instance.healthy !== false) {
    return "success";
  }
  if (instance.heartbeat_status === "unknown" || instance.heartbeat_status === "no_expire") {
    return "warning";
  }
  return "danger";
}

function registryServiceStatusText(service) {
  const labels = {
    healthy: "正常",
    unhealthy: "异常",
    missing: "未注册"
  };
  return labels[service.status] || service.status || "未知";
}

function registryServiceTagType(service) {
  if (service.status === "healthy") {
    return "success";
  }
  if (service.status === "missing") {
    return "info";
  }
  return "danger";
}

function formatEndpoint(endpoint) {
  if (endpoint.socket) {
    return endpoint.socket;
  }
  if (endpoint.host || endpoint.port) {
    return `${endpoint.host || "--"}:${endpoint.port || "--"}`;
  }
  return "--";
}

function endpointSummary(service) {
  const endpoints = service.instances.flatMap((instance) => instance.endpoints || []);
  if (!endpoints.length) {
    return "--";
  }
  return endpoints
    .slice(0, 3)
    .map((endpoint) => `${endpoint.name}/${endpoint.protocol} ${formatEndpoint(endpoint)}`)
    .join("，") + (endpoints.length > 3 ? ` 等 ${endpoints.length} 个` : "");
}

function lastRegisteredAt(service) {
  const values = service.instances
    .map((instance) => instance.last_registered_at || instance.registered_at)
    .filter(Boolean);
  return values.length ? Math.max(...values) : null;
}

function fetchMonitoringOverview() {
  fetchServices();
  fetchRolloutDrain();
  fetchRegistry();
}

function goToDetail(serviceName) {
  router.push(`/monitoring/${serviceName}?window=${currentWindow.value}`);
}

async function handleArchive() {
  archiveLoading.value = true;
  try {
    const response = await monitoringApi.triggerArchive();
    const archived = response.data?.archived ?? 0;
    ElMessage.success(`归档完成，写入 ${archived} 条`);
  } catch (error) {
    ElMessage.error(error.response?.data?.message || "归档失败");
  } finally {
    archiveLoading.value = false;
  }
}

onMounted(() => {
  fetchMonitoringOverview();
  // Poll every 5 seconds
  pollTimer = setInterval(fetchMonitoringOverview, 5000);
});

onUnmounted(() => {
  if (pollTimer) {
    clearInterval(pollTimer);
  }
});
</script>

<style scoped>
.monitoring {
  padding: 24px;
}

.header-left {
  display: flex;
  align-items: center;
  gap: 12px;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 24px;
}

.header h2 {
  margin: 0;
  font-size: 20px;
}

.rollout-card {
  margin-bottom: 16px;
  border-left: 4px solid #909399;
}

.registry-card {
  margin-bottom: 16px;
}

.rollout-card-blocked,
.rollout-card-drained {
  border-left-color: #e6a23c;
}

.rollout-card-interrupted,
.rollout-card-error {
  border-left-color: #f56c6c;
}

.rollout-card-empty {
  border-left-color: #409eff;
}

.rollout-header,
.registry-header,
.rollout-summary,
.rollout-meta,
.blocker-grid,
.sample-row,
.rollout-instance,
.rollout-instance-main {
  display: flex;
  align-items: center;
}

.rollout-header {
  justify-content: space-between;
  gap: 12px;
}

.registry-header {
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.rollout-title,
.registry-title {
  font-weight: 600;
  font-size: 15px;
}

.rollout-subtitle,
.registry-subtitle {
  margin-left: 8px;
  color: #909399;
  font-size: 13px;
}

.rollout-content {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.rollout-summary {
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.rollout-alert {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 600;
  color: #303133;
}

.alert-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #909399;
  flex: 0 0 auto;
}

.alert-info {
  background: #409eff;
}

.alert-warning {
  background: #e6a23c;
}

.alert-critical {
  background: #f56c6c;
}

.rollout-updated,
.registry-updated,
.rollout-placeholder {
  color: #909399;
  font-size: 13px;
}

.registry-table {
  width: 100%;
}

.registry-instances {
  padding: 8px 16px;
  background: #fafafa;
}

.endpoint-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
}

.endpoint-item {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  color: #606266;
  line-height: 20px;
}

.endpoint-name,
.endpoint-protocol,
.endpoint-visibility {
  flex: 0 0 auto;
  padding: 0 6px;
  border-radius: 4px;
  background: #f0f2f5;
  color: #606266;
  font-size: 12px;
}

.endpoint-address,
.registry-summary {
  min-width: 0;
  overflow-wrap: anywhere;
  color: #303133;
}

.empty-text {
  color: #909399;
}

.rollout-instances {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.rollout-instance {
  justify-content: space-between;
  gap: 12px;
  padding: 8px 10px;
  border: 1px solid #ebeef5;
  border-radius: 6px;
  background: #fafafa;
}

.rollout-instance-main {
  gap: 10px;
  min-width: 0;
}

.instance-name {
  font-weight: 600;
  color: #303133;
}

.instance-endpoint {
  color: #909399;
  font-size: 13px;
}

.rollout-meta,
.blocker-grid {
  gap: 12px;
  flex-wrap: wrap;
}

.rollout-meta-item,
.blocker-item {
  min-width: 160px;
  padding: 10px 12px;
  background: #f5f7fa;
  border: 1px solid #ebeef5;
  border-radius: 6px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.rollout-meta-item .label,
.blocker-item .label,
.sample-row .label {
  color: #909399;
  font-size: 13px;
}

.rollout-meta-item .value,
.blocker-item .value {
  color: #303133;
  font-size: 16px;
  font-weight: 600;
  overflow-wrap: anywhere;
}

.rollout-samples {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding-top: 4px;
  border-top: 1px dashed #ebeef5;
}

.sample-row {
  gap: 8px;
  flex-wrap: wrap;
}

.sample-list {
  color: #606266;
  overflow-wrap: anywhere;
}

.rollout-error {
  padding: 8px 10px;
  background: #fef0f0;
  color: #c45656;
  border-radius: 6px;
  overflow-wrap: anywhere;
}

.services-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 16px;
}

.service-card {
  cursor: pointer;
  transition: all 0.3s;
}

.service-card:hover {
  transform: translateY(-2px);
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.1);
}

.service-card.offline {
  border-color: #f56c6c;
}

.service-card.offline :deep(.el-card__header) {
  background-color: #fef0f0;
}

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.service-name {
  font-weight: 600;
  font-size: 15px;
}

.card-content {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.metric {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.metric .label {
  color: #909399;
  font-size: 14px;
}

.metric .value {
  font-size: 18px;
  font-weight: 600;
  color: #303133;
}

.metric .value.warning {
  color: #f56c6c;
}

.metric-secondary {
  padding-top: 4px;
  border-top: 1px dashed #ebeef5;
}

.metric .value.value-secondary {
  font-size: 16px;
}
</style>
