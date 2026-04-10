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
          <h3>欢迎使用管理后台</h3>
          <el-card style="margin-top: 20px">
            <template #header>
              <span>当前登录信息</span>
            </template>
            <el-descriptions :column="2" border>
              <el-descriptions-item label="用户名">
                {{ authStore.username }}
              </el-descriptions-item>
              <el-descriptions-item label="显示名称">
                {{ authStore.displayName }}
              </el-descriptions-item>
              <el-descriptions-item label="角色">
                <el-tag :type="roleTagType">
                  {{ authStore.role }}
                </el-tag>
              </el-descriptions-item>
            </el-descriptions>
          </el-card>

          <el-row :gutter="20" style="margin-top: 20px">
            <el-col :span="8">
              <el-card>
                <el-statistic title="角色权限" :value="authStore.role" />
              </el-card>
            </el-col>
          </el-row>
        </el-main>
      </el-container>
    </el-container>
  </div>
</template>

<script setup>
import { computed } from "vue";
import { useRouter } from "vue-router";
import { ElMessage } from "element-plus";
import { useAuthStore } from "../stores/auth";

const router = useRouter();
const authStore = useAuthStore();

const roleTagType = computed(() => {
  switch (authStore.role) {
    case "admin":
      return "danger";
    case "operator":
      return "warning";
    default:
      return "info";
  }
});

async function handleLogout() {
  await authStore.logout();
  ElMessage.success("已退出登录");
  router.push("/login");
}
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
