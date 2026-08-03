<template>
  <AdminLayout>
    <div class="monitoring-detail">
      <div class="header">
        <div class="back-btn">
          <el-button @click="goBack" size="default">
            <el-icon><Back /></el-icon>
            返回
          </el-button>
        </div>
        <div class="detail-heading">
          <h2>{{ serviceName }} 监控详情</h2>
          <el-tag :type="detailStatusType" size="small">{{ detailStatusText }}</el-tag>
          <span class="detail-updated">最近成功 {{ formatUpdatedAt(detailState.lastSuccessAt) }}</span>
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

      <el-row :gutter="16" class="summary-cards">
        <el-col :span="summarySpan">
          <el-card>
            <el-statistic title="当前 QPS" :value="currentQps" />
          </el-card>
        </el-col>
        <el-col :span="summarySpan">
          <el-card>
            <el-statistic title="当前延迟" :value="currentLatency" suffix="ms" />
          </el-card>
        </el-col>
        <el-col :span="summarySpan">
          <el-card>
            <el-statistic :title="onlineLabel" :value="currentOnline" />
          </el-card>
        </el-col>
        <el-col v-if="secondaryMetricLabel" :span="summarySpan">
          <el-card>
            <el-statistic :title="secondaryMetricLabel" :value="currentSecondaryMetric" />
          </el-card>
        </el-col>
      </el-row>

      <el-card class="chart-card">
        <template #header>
          <span>QPS 折线图</span>
        </template>
        <div ref="qpsChartRef" class="chart"></div>
      </el-card>

      <el-card class="chart-card">
        <template #header>
          <span>延迟折线图</span>
        </template>
        <div ref="latencyChartRef" class="chart"></div>
      </el-card>
    </div>
  </AdminLayout>
</template>

<script setup>
import { ref, onMounted, onUnmounted, watch, computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { Back } from "@element-plus/icons-vue";
import * as echarts from "echarts";
import AdminLayout from "../../components/AdminLayout.vue";
import { monitoringApi } from "../../api";
import { createSerialPoller } from "../../utils/serial-poller";

const route = useRoute();
const router = useRouter();

const serviceName = computed(() => route.params.service);
const currentWindow = ref(route.query.window || "5m");
const serviceInfo = ref(null);
const metricsPoints = ref([]);
const currentQps = ref(0);
const currentLatency = ref(0);
const currentOnline = ref(0);
const currentSecondaryMetric = ref(0);
const detailState = ref({
  loading: false,
  lastAttemptAt: null,
  lastSuccessAt: null,
  failedSources: 0
});

const qpsChartRef = ref(null);
const latencyChartRef = ref(null);
let qpsChart = null;
let latencyChart = null;
let poller = null;

const SERVICE_ONLINE_LABELS = {
  "auth-http": "唯一玩家",
  "game-server": "在线玩家",
  "game-proxy": "连接数",
  "chat-server": "在线玩家",
  "match-service": "匹配池",
  "announce-service": "在线",
  "mail-service": "在线",
  "admin-api": "在线"
};

const onlineLabel = computed(() => SERVICE_ONLINE_LABELS[serviceName.value] || "在线");
const secondaryMetricLabel = computed(() => {
  if (serviceName.value === "auth-http") {
    return "5 分钟活跃会话";
  }

  return "";
});
const summarySpan = computed(() => secondaryMetricLabel.value ? 6 : 8);
const detailStatusType = computed(() => {
  if (detailState.value.failedSources > 0) return "warning";
  if (detailState.value.loading) return "info";
  if (!detailState.value.lastSuccessAt) return "info";
  return detailState.value.lastAttemptAt - detailState.value.lastSuccessAt > 45_000 ? "warning" : "success";
});
const detailStatusText = computed(() => {
  if (detailState.value.loading && !detailState.value.lastSuccessAt) return "加载中";
  if (detailState.value.failedSources > 0) return `部分失败（${detailState.value.failedSources}）`;
  if (!detailState.value.lastSuccessAt) return "等待数据";
  if (detailState.value.lastAttemptAt - detailState.value.lastSuccessAt > 45_000) return "数据陈旧";
  return detailState.value.loading ? "刷新中" : "数据正常";
});

async function fetchServiceInfo(signal) {
  try {
    const requestedService = serviceName.value;
    const response = await monitoringApi.getServices({ signal });
    if (response.data.ok !== false) {
      if (requestedService !== serviceName.value) return false;
      serviceInfo.value = response.data.services?.find((s) => s.name === requestedService);
      if (serviceInfo.value) {
        currentQps.value = serviceInfo.value.qps || 0;
        currentLatency.value = serviceInfo.value.latency_ms || 0;
        currentOnline.value = serviceInfo.value.online_value || 0;
        currentSecondaryMetric.value = serviceInfo.value.active_sessions_5m || 0;
      }
      return true;
    }
    return false;
  } catch (error) {
    if (signal.aborted) return false;
    console.error("Failed to fetch service info:", error);
    return false;
  }
}

async function fetchMetrics(signal) {
  try {
    const requestedService = serviceName.value;
    const requestedWindow = currentWindow.value;
    const response = await monitoringApi.getServiceMetrics(requestedService, requestedWindow, { signal });
    if (response.data.ok !== false) {
      if (requestedService !== serviceName.value || requestedWindow !== currentWindow.value) return false;
      metricsPoints.value = response.data.points || [];
      updateCharts();
      return true;
    }
    return false;
  } catch (error) {
    if (signal.aborted) return false;
    console.error("Failed to fetch metrics:", error);
    return false;
  }
}

async function fetchMonitoringDetail({ signal }) {
  detailState.value.loading = true;
  detailState.value.lastAttemptAt = Date.now();
  try {
    const results = await Promise.all([fetchServiceInfo(signal), fetchMetrics(signal)]);
    if (signal.aborted) return false;
    detailState.value.failedSources = results.filter((succeeded) => !succeeded).length;
    if (detailState.value.failedSources === 0) {
      detailState.value.lastSuccessAt = Date.now();
    }
    return detailState.value.failedSources === 0;
  } finally {
    if (!signal.aborted) detailState.value.loading = false;
  }
}

function formatTime(timestamp) {
  const date = new Date(timestamp * 1000);
  return `${date.getHours().toString().padStart(2, "0")}:${date.getMinutes().toString().padStart(2, "0")}:${date.getSeconds().toString().padStart(2, "0")}`;
}

function formatUpdatedAt(timestamp) {
  if (!timestamp) return "--";
  return new Date(timestamp).toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit"
  });
}

function updateCharts() {
  const timestamps = metricsPoints.value.map((p) => formatTime(p.timestamp));
  const qpsData = metricsPoints.value.map((p) => p.qps);
  const latencyData = metricsPoints.value.map((p) => p.latency_ms);

  if (qpsChart) {
    qpsChart.setOption({
      xAxis: {
        type: "category",
        data: timestamps,
        boundaryGap: false
      },
      yAxis: {
        type: "value",
        min: 0
      },
      series: [
        {
          name: "QPS",
          type: "line",
          data: qpsData,
          smooth: true,
          areaStyle: {
            opacity: 0.2
          }
        }
      ]
    });
  }

  if (latencyChart) {
    latencyChart.setOption({
      xAxis: {
        type: "category",
        data: timestamps,
        boundaryGap: false
      },
      yAxis: {
        type: "value",
        min: 0
      },
      series: [
        {
          name: "延迟",
          type: "line",
          data: latencyData,
          smooth: true,
          areaStyle: {
            opacity: 0.2
          }
        }
      ]
    });
  }
}

function initCharts() {
  if (qpsChartRef.value) {
    qpsChart = echarts.init(qpsChartRef.value);
  }
  if (latencyChartRef.value) {
    latencyChart = echarts.init(latencyChartRef.value);
  }
}

function destroyCharts() {
  if (qpsChart) {
    qpsChart.dispose();
    qpsChart = null;
  }
  if (latencyChart) {
    latencyChart.dispose();
    latencyChart = null;
  }
}

function goBack() {
  router.push("/monitoring");
}

watch([serviceName, currentWindow], () => {
  poller?.trigger();
});

function resizeCharts() {
  qpsChart?.resize();
  latencyChart?.resize();
}

onMounted(() => {
  initCharts();
  poller = createSerialPoller({ task: fetchMonitoringDetail });
  poller.start();
  window.addEventListener("resize", resizeCharts);
});

onUnmounted(() => {
  poller?.stop();
  window.removeEventListener("resize", resizeCharts);
  destroyCharts();
});
</script>

<style scoped>
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

.detail-heading {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  flex-wrap: wrap;
}

.detail-updated {
  color: #909399;
  font-size: 13px;
}

.back-btn {
  margin-right: 16px;
}

.summary-cards {
  margin-bottom: 16px;
}

.chart-card {
  margin-bottom: 16px;
}

.chart {
  width: 100%;
  height: 300px;
}
</style>
