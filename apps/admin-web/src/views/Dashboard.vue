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

    <el-row v-if="authStore.hasPermission(P.MAINTENANCE_READ)" :gutter="20" style="margin-top: 20px">
      <el-col :span="12">
        <el-card v-loading="maintenance.loading">
          <template #header>
            <span>维护模式</span>
          </template>
          <el-alert
            v-if="maintenance.operationMessage"
            :title="maintenance.operationMessage"
            :type="maintenance.operationState === 'failed' ? 'error' : maintenance.operationState === 'preflight' || maintenance.operationState === 'in_progress' ? 'warning' : 'success'"
            :closable="false"
            show-icon
            style="margin-bottom: 12px"
          />
          <el-descriptions :column="1" border>
            <el-descriptions-item label="当前状态">
              <el-tag :type="maintenance.enabled ? 'danger' : 'success'">
                {{ maintenance.enabled ? "维护中" : "正常开放" }}
              </el-tag>
            </el-descriptions-item>
            <el-descriptions-item label="原因">
              {{ maintenance.reason || "-" }}
            </el-descriptions-item>
            <el-descriptions-item label="更新人">
              {{ maintenance.updatedBy || "-" }}
            </el-descriptions-item>
          </el-descriptions>

          <el-form
            v-if="authStore.hasPermission(P.MAINTENANCE_WRITE)"
            :model="maintenanceForm"
            label-width="80px"
            style="margin-top: 16px"
          >
          <el-form-item label="原因">
              <el-input v-model="maintenanceForm.reason" maxlength="255" show-word-limit placeholder="维护原因" />
            </el-form-item>
            <el-form-item>
              <el-button
                :type="maintenance.enabled ? 'success' : 'danger'"
                :loading="maintenance.updating"
                @click="handleMaintenanceToggle"
              >
                {{ maintenance.enabled ? "关闭维护" : "开启维护" }}
              </el-button>
            </el-form-item>
          </el-form>
        </el-card>
      </el-col>
    </el-row>
  </AdminLayout>
</template>

<script setup>
import { computed, onMounted, reactive } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import { useAuthStore } from "../stores/auth";
import { maintenanceApi } from "../api";
import { ADMIN_PERMISSIONS as P } from "../auth/permissions";
import { formatHighRiskPreview, runHighRiskOperation } from "../operations/high-risk";

const authStore = useAuthStore();
const maintenance = reactive({
  loading: false,
  updating: false,
  enabled: false,
  reason: "",
  updatedBy: "",
  updatedAt: "",
  operationState: "idle",
  operationMessage: ""
});
const maintenanceForm = reactive({
  reason: ""
});

const roleTagType = computed(() => {
  switch (authStore.role) {
    case "super_admin":
    case "admin":
      return "danger";
    case "operator":
      return "warning";
    default:
      return "info";
  }
});

function applyMaintenanceStatus(data) {
  maintenance.enabled = !!data.enabled;
  maintenance.reason = data.reason || "";
  maintenance.updatedBy = data.updatedBy || "";
  maintenance.updatedAt = data.updatedAt || "";
}

async function fetchMaintenance() {
  if (!authStore.hasPermission(P.MAINTENANCE_READ)) {
    return;
  }

  maintenance.loading = true;
  try {
    const { data } = await maintenanceApi.getStatus();
    applyMaintenanceStatus(data);
  } catch (error) {
    ElMessage.error(error.response?.data?.message || "获取维护状态失败");
  } finally {
    maintenance.loading = false;
  }
}

async function handleMaintenanceToggle() {
  const enabled = !maintenance.enabled;
  const title = enabled ? "开启维护模式" : "关闭维护模式";
  if (!maintenanceForm.reason.trim()) {
    ElMessage.warning("请填写维护操作原因");
    return;
  }
  try {
    maintenance.updating = true;
    maintenance.operationState = "preflight";
    maintenance.operationMessage = "正在生成服务端影响预览。";
    const outcome = await runHighRiskOperation({
      invoke: maintenanceApi.setStatus,
      payload: { enabled, reason: maintenanceForm.reason.trim() },
      confirm: async (preflight) => {
        maintenance.operationMessage = "已生成影响预览，等待明确确认。";
        try {
          await ElMessageBox.confirm(formatHighRiskPreview(preflight), `${title}确认`, {
            type: enabled ? "warning" : "info",
            confirmButtonText: "确认执行",
            cancelButtonText: "取消"
          });
          return true;
        } catch {
          return false;
        }
      }
    });
    if (outcome.phase === "cancelled") {
      maintenance.operationState = "cancelled";
      maintenance.operationMessage = "操作已取消，未执行维护状态变更。";
      ElMessage.info("操作已取消，未执行维护状态变更");
      return;
    }
    if (outcome.phase === "in_progress") {
      maintenance.operationState = "in_progress";
      maintenance.operationMessage = `请求 ${outcome.requestId} 正在执行，请勿重复提交。`;
      ElMessage.warning(`请求 ${outcome.requestId} 正在执行，请勿重复提交`);
      return;
    }
    if (outcome.phase === "terminal") {
      maintenance.operationState = "terminal";
      maintenance.operationMessage = `请求 ${outcome.requestId} 已返回首次终态。`;
      ElMessage.info(`请求 ${outcome.requestId} 已返回首次终态`);
      await fetchMaintenance();
      return;
    }
    applyMaintenanceStatus(outcome.response);
    maintenance.operationState = "succeeded";
    maintenance.operationMessage = "维护状态已完成并记录审计。";
    maintenanceForm.reason = "";
    ElMessage.success(enabled ? "维护模式已开启" : "维护模式已关闭");
  } catch (error) {
    maintenance.operationState = "failed";
    maintenance.operationMessage = error.response?.data?.message || "维护模式更新失败";
    ElMessage.error(maintenance.operationMessage);
  } finally {
    maintenance.updating = false;
  }
}

onMounted(() => {
  fetchMaintenance();
});
</script>
