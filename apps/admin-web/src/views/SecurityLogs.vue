<template>
  <div class="dashboard">
    <el-container>
      <el-header class="header">
        <h2>MyServer 管理后台</h2>
        <div class="user-info">
          <span>{{ authStore.displayName }} ({{ authStore.role }})</span>
          <el-button type="danger" size="small" @click="handleLogout">
            退出登录
          </el-button>
        </div>
      </el-header>

      <el-container>
        <el-aside width="200px" class="sidebar">
          <el-menu :default-active="$route.name" router>
            <el-menu-item index="/">
              <span>概览</span>
            </el-menu-item>
            <el-menu-item index="/audit-logs">
              <span>审计日志</span>
            </el-menu-item>
            <el-menu-item index="/security-logs">
              <span>安全日志</span>
            </el-menu-item>
            <el-menu-item index="/players">
              <span>玩家管理</span>
            </el-menu-item>
            <el-menu-item index="/gm" v-if="authStore.isOperator">
              <span>GM 命令</span>
            </el-menu-item>
          </el-menu>
        </el-aside>

        <el-main>
          <h3>安全日志</h3>

          <el-card style="margin-top: 20px">
            <el-form :inline="true" @submit.prevent="handleSearch">
              <el-form-item label="事件类型">
                <el-input v-model="filters.eventType" placeholder="如: ip_rate_limited" clearable />
              </el-form-item>
              <el-form-item label="严重级别">
                <el-select v-model="filters.severity" placeholder="全部" clearable>
                  <el-option label="info" value="info" />
                  <el-option label="warning" value="warning" />
                  <el-option label="critical" value="critical" />
                </el-select>
              </el-form-item>
              <el-form-item>
                <el-button type="primary" @click="handleSearch">查询</el-button>
              </el-form-item>
            </el-form>

            <el-table :data="logs" v-loading="loading" stripe style="margin-top: 16px">
              <el-table-column prop="created_at" label="时间" width="180">
                <template #default="{ row }">
                  {{ formatTime(row.created_at) }}
                </template>
              </el-table-column>
              <el-table-column prop="event_type" label="事件类型" width="200">
                <template #default="{ row }">
                  <el-tag size="small" :type="severityType(row.severity)">
                    {{ row.event_type }}
                  </el-tag>
                </template>
              </el-table-column>
              <el-table-column prop="target_type" label="目标类型" width="100" />
              <el-table-column prop="target_value" label="目标" width="150" />
              <el-table-column prop="client_ip" label="IP" width="140" />
              <el-table-column prop="severity" label="级别" width="100">
                <template #default="{ row }">
                  <el-tag size="small" :type="severityType(row.severity)">
                    {{ row.severity }}
                  </el-tag>
                </template>
              </el-table-column>
              <el-table-column prop="details_json" label="详情">
                <template #default="{ row }">
                  <pre v-if="row.details_json" style="font-size: 11px; margin: 0; white-space: pre-wrap">
                    {{ formatJson(row.details_json) }}
                  </pre>
                </template>
              </el-table-column>
            </el-table>

            <el-pagination
              v-model:current-page="pagination.page"
              v-model:page-size="pagination.limit"
              :total="pagination.total"
              :page-sizes="[20, 50, 100]"
              layout="total, sizes, prev, pager, next"
              style="margin-top: 16px"
              @size-change="fetchLogs"
              @current-change="fetchLogs"
            />
          </el-card>
        </el-main>
      </el-container>
    </el-container>
  </div>
</template>

<script setup>
import { ref, reactive, onMounted } from "vue";
import { useRouter } from "vue-router";
import { ElMessage } from "element-plus";
import { useAuthStore } from "../stores/auth";
import { securityApi } from "../api";

const router = useRouter();
const authStore = useAuthStore();

const logs = ref([]);
const loading = ref(false);
const filters = reactive({
  eventType: "",
  severity: ""
});
const pagination = ref({
  page: 1,
  limit: 50,
  total: 0
});

function formatTime(time) {
  return new Date(time).toLocaleString("zh-CN");
}

function formatJson(json) {
  try {
    return JSON.stringify(JSON.parse(json), null, 2);
  } catch {
    return json;
  }
}

function severityType(severity) {
  switch (severity) {
    case "critical":
      return "danger";
    case "warning":
      return "warning";
    default:
      return "info";
  }
}

async function fetchLogs() {
  loading.value = true;
  try {
    const params = {
      limit: pagination.value.limit,
      offset: (pagination.value.page - 1) * pagination.value.limit
    };
    if (filters.eventType) params.event_type = filters.eventType;
    if (filters.severity) params.severity = filters.severity;

    const { data } = await securityApi.getLogs(params);
    logs.value = data.logs;
    pagination.value.total = data.total;
  } catch (err) {
    ElMessage.error("获取日志失败");
  } finally {
    loading.value = false;
  }
}

function handleSearch() {
  pagination.value.page = 1;
  fetchLogs();
}

async function handleLogout() {
  await authStore.logout();
  ElMessage.success("已退出登录");
  router.push("/login");
}

onMounted(() => {
  fetchLogs();
});
</script>

<style scoped>
.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  background: #fff;
  border-bottom: 1px solid #e4e7ed;
}

.header h2 {
  margin: 0;
  font-size: 18px;
}

.user-info {
  display: flex;
  align-items: center;
  gap: 16px;
}

.sidebar {
  background: #f5f7fa;
  border-right: 1px solid #e4e7ed;
  min-height: calc(100vh - 60px);
}

.dashboard {
  min-height: 100vh;
}

.el-main {
  background: #f5f7fa;
}
</style>
