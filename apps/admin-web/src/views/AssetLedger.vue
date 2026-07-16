<template>
  <AdminLayout>
    <h3>资产流水</h3>

    <el-form class="filters" :inline="true" @submit.prevent="search">
      <el-form-item label="角色">
        <el-input v-model="filters.character_id" clearable />
      </el-form-item>
      <el-form-item label="请求">
        <el-input v-model="filters.request_id" clearable />
      </el-form-item>
      <el-form-item label="来源类型">
        <el-input v-model="filters.origin_type" clearable />
      </el-form-item>
      <el-form-item label="交付 ID">
        <el-input v-model="filters.delivery_id" clearable />
      </el-form-item>
      <el-form-item>
        <el-button type="primary" native-type="submit" :loading="loading">查询</el-button>
      </el-form-item>
    </el-form>

    <el-table :data="entries" v-loading="loading" stripe>
      <el-table-column prop="createdAt" label="时间" width="176">
        <template #default="{ row }">{{ formatTime(row.createdAt) }}</template>
      </el-table-column>
      <el-table-column prop="characterId" label="角色" min-width="156" />
      <el-table-column prop="requestId" label="请求" min-width="176" />
      <el-table-column prop="itemId" label="物品" width="88" />
      <el-table-column label="变更" width="132">
        <template #default="{ row }">
          {{ row.quantityBefore }} -> {{ row.quantityAfter }} ({{ signed(row.quantityDelta) }})
        </template>
      </el-table-column>
      <el-table-column prop="container" label="容器" width="100" />
      <el-table-column prop="originType" label="来源" width="112" />
      <el-table-column prop="deliveryMethod" label="交付" width="96" />
      <el-table-column prop="mailId" label="邮件" min-width="132">
        <template #default="{ row }">{{ row.mailId || "-" }}</template>
      </el-table-column>
      <el-table-column prop="fallbackReason" label="Fallback" min-width="140">
        <template #default="{ row }">{{ row.fallbackReason || "-" }}</template>
      </el-table-column>
    </el-table>

    <el-pagination
      v-model:current-page="pagination.page"
      v-model:page-size="pagination.limit"
      :total="pagination.total"
      :page-sizes="[20, 50, 100]"
      layout="total, sizes, prev, pager, next"
      class="pagination"
      @size-change="search"
      @current-change="search"
    />
  </AdminLayout>
</template>

<script setup>
import { reactive, ref } from "vue";
import { ElMessage } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import { assetApi } from "../api";

const loading = ref(false);
const entries = ref([]);
const filters = reactive({
  character_id: "",
  request_id: "",
  origin_type: "",
  delivery_id: ""
});
const pagination = reactive({ page: 1, limit: 50, total: 0 });

function formatTime(value) {
  return value ? new Date(value).toLocaleString("zh-CN") : "-";
}

function signed(value) {
  return Number(value) > 0 ? `+${value}` : String(value);
}

async function search() {
  const hasFilter = Object.values(filters).some((value) => value.trim());
  if (!hasFilter) {
    ElMessage.warning("请输入至少一个查询条件");
    return;
  }
  loading.value = true;
  try {
    const { data } = await assetApi.getLedger({
      ...filters,
      limit: pagination.limit,
      offset: (pagination.page - 1) * pagination.limit
    });
    entries.value = data.entries;
    pagination.total = data.total;
  } catch (error) {
    ElMessage.error(error.response?.data?.message || "获取资产流水失败");
  } finally {
    loading.value = false;
  }
}
</script>

<style scoped>
.filters {
  margin: 16px 0;
}

.pagination {
  margin-top: 16px;
}
</style>
