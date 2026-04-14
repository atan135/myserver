<template>
  <div class="monitoring">
    <div class="header">
      <div class="header-left">
        <el-button @click="goHome" size="small">← 返回</el-button>
        <h2>服务监控</h2>
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
        </div>
      </el-card>
    </div>
  </div>
</template>

<script setup>
import { ref, onMounted, onUnmounted, computed } from "vue";
import { useRouter } from "vue-router";
import { monitoringApi } from "../../api";

const router = useRouter();
const services = ref([]);
const currentWindow = ref("5m");
let pollTimer = null;

const SERVICE_ONLINE_LABELS = {
  "auth-http": "在线会话",
  "game-server": "在线玩家",
  "game-proxy": "连接数",
  "chat-server": "在线玩家",
  "match-service": "匹配池",
  "mail-service": null,
  "admin-api": null
};

function onlineLabel(serviceName) {
  return SERVICE_ONLINE_LABELS[serviceName] || "在线";
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

function goToDetail(serviceName) {
  router.push(`/monitoring/${serviceName}?window=${currentWindow.value}`);
}

function goHome() {
  router.push("/");
}

onMounted(() => {
  fetchServices();
  // Poll every 5 seconds
  pollTimer = setInterval(fetchServices, 5000);
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
</style>
