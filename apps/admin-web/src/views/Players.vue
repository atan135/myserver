<template>
  <AdminLayout>
    <h3>玩家管理</h3>

    <el-card style="margin-top: 20px">
      <el-form :inline="true" @submit.prevent="handleSearch">
        <el-form-item label="登录名">
          <el-input v-model="filters.loginName" placeholder="模糊搜索" clearable />
        </el-form-item>
        <el-form-item label="Guest ID">
          <el-input v-model="filters.guestId" placeholder="模糊搜索" clearable />
        </el-form-item>
        <el-form-item label="状态">
          <el-select v-model="filters.status" placeholder="全部" clearable>
            <el-option label="正常" value="active" />
            <el-option label="禁用" value="disabled" />
            <el-option label="封禁" value="banned" />
          </el-select>
        </el-form-item>
        <el-form-item>
          <el-button type="primary" @click="handleSearch">查询</el-button>
        </el-form-item>
      </el-form>

      <el-table :data="players" v-loading="loading" stripe style="margin-top: 16px">
        <el-table-column prop="player_id" label="Player ID" width="220" />
        <el-table-column prop="login_name" label="登录名" width="120">
          <template #default="{ row }">
            {{ row.login_name || "-" }}
          </template>
        </el-table-column>
        <el-table-column prop="guest_id" label="Guest ID" width="180">
          <template #default="{ row }">
            {{ row.guest_id || "-" }}
          </template>
        </el-table-column>
        <el-table-column prop="account_type" label="账号类型" width="100" />
        <el-table-column prop="status" label="状态" width="100">
          <template #default="{ row }">
            <el-tag size="small" :type="statusType(row.status)">
              {{ statusLabel(row.status) }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column prop="last_login_at" label="最后登录" width="170">
          <template #default="{ row }">
            {{ formatTime(row.last_login_at) }}
          </template>
        </el-table-column>
        <el-table-column label="操作" width="150" v-if="authStore.isOperator">
          <template #default="{ row }">
            <el-button
              v-if="row.status === 'active'"
              type="warning"
              size="small"
              link
              @click="handleDisable(row)"
            >
              禁用
            </el-button>
            <el-button
              v-if="row.status === 'disabled' || row.status === 'banned'"
              type="success"
              size="small"
              link
              @click="handleEnable(row)"
            >
              解禁
            </el-button>
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
        @size-change="fetchPlayers"
        @current-change="fetchPlayers"
      />
    </el-card>
  </AdminLayout>
</template>

<script setup>
import { ref, reactive, onMounted } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import { useAuthStore } from "../stores/auth";
import { playerApi } from "../api";

const authStore = useAuthStore();

const players = ref([]);
const loading = ref(false);
const filters = reactive({
  loginName: "",
  guestId: "",
  status: ""
});
const pagination = ref({
  page: 1,
  limit: 50,
  total: 0
});

function formatTime(time) {
  if (!time) return "-";
  return new Date(time).toLocaleString("zh-CN");
}

function statusType(status) {
  switch (status) {
    case "active":
      return "success";
    case "disabled":
      return "info";
    case "banned":
      return "danger";
    default:
      return "";
  }
}

function statusLabel(status) {
  switch (status) {
    case "active":
      return "正常";
    case "disabled":
      return "禁用";
    case "banned":
      return "封禁";
    default:
      return status;
  }
}

async function fetchPlayers() {
  loading.value = true;
  try {
    const params = {
      limit: pagination.value.limit,
      offset: (pagination.value.page - 1) * pagination.value.limit
    };
    if (filters.loginName) params.login_name = filters.loginName;
    if (filters.guestId) params.guest_id = filters.guestId;
    if (filters.status) params.status = filters.status;

    const { data } = await playerApi.getPlayers(params);
    players.value = data.players;
    pagination.value.total = data.total;
  } catch (err) {
    ElMessage.error("获取玩家列表失败");
  } finally {
    loading.value = false;
  }
}

function handleSearch() {
  pagination.value.page = 1;
  fetchPlayers();
}

async function handleDisable(row) {
  try {
    await ElMessageBox.confirm(
      `确定禁用玩家 ${row.login_name || row.guest_id || row.player_id} 吗？`,
      "禁用玩家",
      { type: "warning" }
    );

    await playerApi.updatePlayerStatus(row.player_id, "disabled");
    ElMessage.success("已禁用");
    fetchPlayers();
  } catch (err) {
    if (err !== "cancel") {
      ElMessage.error("操作失败");
    }
  }
}

async function handleEnable(row) {
  try {
    await ElMessageBox.confirm(
      `确定解禁玩家 ${row.login_name || row.guest_id || row.player_id} 吗？`,
      "解禁玩家",
      { type: "info" }
    );

    await playerApi.updatePlayerStatus(row.player_id, "active");
    ElMessage.success("已解禁");
    fetchPlayers();
  } catch (err) {
    if (err !== "cancel") {
      ElMessage.error("操作失败");
    }
  }
}

onMounted(() => {
  fetchPlayers();
});
</script>
