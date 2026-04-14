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
          <el-menu :default-active="activeMenu" router>
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
            <el-menu-item v-if="authStore.isOperator" index="/gm">
              <span>GM 命令</span>
            </el-menu-item>
            <el-menu-item index="/monitoring">
              <span>服务监控</span>
            </el-menu-item>
          </el-menu>
        </el-aside>

        <el-main class="main-content">
          <slot />
        </el-main>
      </el-container>
    </el-container>
  </div>
</template>

<script setup>
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ElMessage } from "element-plus";
import { useAuthStore } from "../stores/auth";

const route = useRoute();
const router = useRouter();
const authStore = useAuthStore();

const activeMenu = computed(() => {
  if (route.path.startsWith("/monitoring")) {
    return "/monitoring";
  }
  return route.path;
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

.main-content {
  background: #f5f7fa;
}
</style>
