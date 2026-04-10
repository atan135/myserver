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
        </el-main>
      </el-container>
    </el-container>
  </div>
</template>

<script setup>
import { ref, onMounted } from "vue";
import { useRouter } from "vue-router";
import { ElMessage } from "element-plus";
import { useAuthStore } from "../stores/auth";
import { auditApi } from "../api";

const router = useRouter();
const authStore = useAuthStore();

const logs = ref([]);
const loading = ref(false);
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
