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
        <h2>{{ serviceName }} 监控详情</h2>
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

const qpsChartRef = ref(null);
const latencyChartRef = ref(null);
let qpsChart = null;
let latencyChart = null;
let pollTimer = null;

const SERVICE_ONLINE_LABELS = {
  "auth-http": "唯一玩家",
  "game-server": "在线玩家",
  "game-proxy": "连接数",
  "chat-server": "在线玩家",
  "match-service": "匹配池",
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

async function fetchServiceInfo() {
  try {
    const response = await monitoringApi.getServices();
    if (response.data.ok !== false) {
      serviceInfo.value = response.data.services?.find((s) => s.name === serviceName.value);
      if (serviceInfo.value) {
        currentQps.value = serviceInfo.value.qps || 0;
        currentLatency.value = serviceInfo.value.latency_ms || 0;
        currentOnline.value = serviceInfo.value.online_value || 0;
        currentSecondaryMetric.value = serviceInfo.value.active_sessions_5m || 0;
      }
    }
  } catch (error) {
    console.error("Failed to fetch service info:", error);
  }
}

async function fetchMetrics() {
  try {
    const response = await monitoringApi.getServiceMetrics(serviceName.value, currentWindow.value);
    if (response.data.ok !== false) {
      metricsPoints.value = response.data.points || [];
      updateCharts();
    }
  } catch (error) {
    console.error("Failed to fetch metrics:", error);
  }
}

function formatTime(timestamp) {
  const date = new Date(timestamp * 1000);
  return `${date.getHours().toString().padStart(2, "0")}:${date.getMinutes().toString().padStart(2, "0")}:${date.getSeconds().toString().padStart(2, "0")}`;
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

watch(currentWindow, () => {
  fetchMetrics();
});

onMounted(() => {
  initCharts();
  fetchServiceInfo();
  fetchMetrics();
  pollTimer = setInterval(() => {
    fetchServiceInfo();
    fetchMetrics();
  }, 5000);

  window.addEventListener("resize", () => {
    qpsChart?.resize();
    latencyChart?.resize();
  });
});

onUnmounted(() => {
  if (pollTimer) {
    clearInterval(pollTimer);
  }
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
