<template>
  <AdminLayout>
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
  </AdminLayout>
</template>

<script setup>
import { computed } from "vue";
import AdminLayout from "../components/AdminLayout.vue";
import { useAuthStore } from "../stores/auth";

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
</script>
