<template>
  <AdminLayout>
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
  </AdminLayout>
</template>

<script setup>
import { ref, reactive, onMounted } from "vue";
import { ElMessage } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import { securityApi } from "../api";

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

onMounted(() => {
  fetchLogs();
});
</script>
