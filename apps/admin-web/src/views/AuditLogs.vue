<template>
  <AdminLayout>
    <h3>管理操作审计日志</h3>

    <el-card style="margin-top: 20px">
      <el-table :data="logs" v-loading="loading" stripe>
        <el-table-column prop="created_at" label="时间" width="180">
          <template #default="{ row }">
            {{ formatTime(row.created_at) }}
          </template>
        </el-table-column>
        <el-table-column prop="admin_username" label="管理员" width="120" />
        <el-table-column prop="action" label="操作" width="150">
          <template #default="{ row }">
            <el-tag size="small">{{ row.action }}</el-tag>
          </template>
        </el-table-column>
        <el-table-column prop="target_type" label="目标类型" width="100" />
        <el-table-column prop="target_value" label="目标" />
        <el-table-column prop="ip" label="IP" width="140" />
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

    <el-card class="operation-audit-card">
      <template #header>
        <div class="card-header">
          <span>高风险操作事件</span>
          <el-button size="small" :loading="operationAuditLoading" @click="fetchOperationEvents">刷新</el-button>
        </div>
      </template>
      <el-table :data="operationEvents" v-loading="operationAuditLoading" stripe>
        <el-table-column prop="createdAt" label="时间" width="180">
          <template #default="{ row }">{{ formatTime(row.createdAt) }}</template>
        </el-table-column>
        <el-table-column prop="requestId" label="请求 ID" min-width="160" show-overflow-tooltip />
        <el-table-column prop="eventType" label="事件" min-width="150">
          <template #default="{ row }"><el-tag size="small">{{ row.eventType }}</el-tag></template>
        </el-table-column>
        <el-table-column prop="permissionKey" label="权限" min-width="150" />
        <el-table-column prop="result" label="终态" width="140" />
      </el-table>
      <el-empty v-if="!operationAuditLoading && !operationEvents.length" description="当前时间窗口没有操作事件" :image-size="56" />
    </el-card>
  </AdminLayout>
</template>

<script setup>
import { ref, onMounted } from "vue";
import { ElMessage } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import { auditApi } from "../api";

const logs = ref([]);
const loading = ref(false);
const operationAuditLoading = ref(false);
const operationEvents = ref([]);
const pagination = ref({
  page: 1,
  limit: 50,
  total: 0
});

function formatTime(time) {
  return new Date(time).toLocaleString("zh-CN");
}

async function fetchLogs() {
  loading.value = true;
  try {
    const { data } = await auditApi.getLogs({
      limit: pagination.value.limit,
      offset: (pagination.value.page - 1) * pagination.value.limit
    });
    logs.value = data.logs;
    pagination.value.total = data.total;
  } catch (err) {
    ElMessage.error("获取日志失败");
  } finally {
    loading.value = false;
  }
}

async function fetchOperationEvents() {
  operationAuditLoading.value = true;
  try {
    const { data } = await auditApi.getOperationEvents({
      from: new Date(Date.now() - 7 * 24 * 60 * 60 * 1000).toISOString(),
      to: new Date().toISOString(),
      limit: 100
    });
    operationEvents.value = Array.isArray(data?.events) ? data.events : [];
  } catch (error) {
    ElMessage.error(error?.response?.data?.message || "获取操作审计事件失败");
  } finally {
    operationAuditLoading.value = false;
  }
}

onMounted(() => {
  fetchLogs();
  fetchOperationEvents();
});
</script>

<style scoped>
.operation-audit-card { margin-top: 20px; }
.card-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
</style>
