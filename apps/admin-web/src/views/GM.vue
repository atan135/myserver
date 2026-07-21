<template>
  <AdminLayout>
    <h3>GM 命令</h3>

    <el-alert
      v-if="operation.message"
      :title="operation.message"
      :type="operationAlertType"
      :closable="false"
      show-icon
      style="margin-top: 16px"
    />

    <el-row :gutter="20">
      <!-- 广播 -->
      <el-col :span="12" v-if="authStore.hasPermission(P.GM_BROADCAST)">
        <el-card style="margin-top: 20px">
          <template #header>
            <span>广播消息</span>
          </template>
          <el-form :model="broadcast" label-width="80px">
            <el-form-item label="标题">
              <el-input v-model="broadcast.title" placeholder="广播标题" />
            </el-form-item>
            <el-form-item label="内容">
              <el-input
                v-model="broadcast.content"
                type="textarea"
                :rows="3"
                placeholder="广播内容"
              />
            </el-form-item>
            <el-form-item label="发送者">
              <el-input v-model="broadcast.sender" placeholder="System" />
            </el-form-item>
            <el-form-item label="操作原因">
              <el-input v-model="broadcast.reason" maxlength="255" show-word-limit />
            </el-form-item>
            <el-form-item label="备份证据">
              <el-input v-model="broadcast.backupReference" maxlength="128" placeholder="backup-or-change-reference" />
            </el-form-item>
            <el-form-item>
              <el-button type="primary" :loading="broadcast.loading" @click="handleBroadcast">
                发送广播
              </el-button>
            </el-form-item>
          </el-form>
        </el-card>
      </el-col>

      <!-- 发放道具 -->
      <el-col :span="12" v-if="authStore.hasPermission(P.GM_SEND_ITEM)">
        <el-card style="margin-top: 20px">
          <template #header>
            <span>发放道具</span>
          </template>
          <el-form :model="sendItem" label-width="80px">
            <el-form-item label="角色ID">
              <el-input v-model="sendItem.characterId" placeholder="chr_..." />
            </el-form-item>
            <el-form-item label="道具ID">
              <el-input v-model="sendItem.itemId" placeholder="item-xxx" />
            </el-form-item>
            <el-form-item label="数量">
              <el-input-number v-model="sendItem.itemCount" :min="1" :max="9999" />
            </el-form-item>
            <el-form-item label="原因">
              <el-input v-model="sendItem.reason" placeholder="发放原因（可选）" />
            </el-form-item>
            <el-form-item label="备份证据">
              <el-input v-model="sendItem.backupReference" maxlength="128" placeholder="backup-or-change-reference" />
            </el-form-item>
            <el-form-item>
              <el-button type="primary" :loading="sendItem.loading" @click="handleSendItem">
                发放道具
              </el-button>
            </el-form-item>
          </el-form>
        </el-card>
      </el-col>
    </el-row>

    <el-row :gutter="20">
      <!-- 踢出玩家 -->
      <el-col :span="12" v-if="authStore.hasPermission(P.GM_KICK_PLAYER)">
        <el-card style="margin-top: 20px">
          <template #header>
            <span>踢出玩家</span>
          </template>
          <el-form :model="kickPlayer" label-width="80px">
            <el-form-item label="玩家ID">
              <el-input v-model="kickPlayer.playerId" placeholder="plr_..." />
            </el-form-item>
            <el-form-item label="原因">
              <el-input v-model="kickPlayer.reason" placeholder="原因（可选）" />
            </el-form-item>
            <el-form-item label="备份证据">
              <el-input v-model="kickPlayer.backupReference" maxlength="128" placeholder="backup-or-change-reference" />
            </el-form-item>
            <el-form-item>
              <el-button type="warning" :loading="kickPlayer.loading" @click="handleKickPlayer">
                踢出玩家
              </el-button>
            </el-form-item>
          </el-form>
        </el-card>
      </el-col>

      <!-- 封禁玩家 -->
      <el-col :span="12" v-if="authStore.hasPermission(P.GM_BAN_PLAYER)">
        <el-card style="margin-top: 20px">
          <template #header>
            <span>封禁玩家</span>
          </template>
          <el-form :model="banPlayer" label-width="80px">
            <el-form-item label="玩家ID">
              <el-input v-model="banPlayer.playerId" placeholder="plr_..." />
            </el-form-item>
            <el-form-item label="封禁时长">
              <el-input-number v-model="banPlayer.durationSeconds" :min="60" :max="31536000" />
              <span style="margin-left: 8px; color: #999">秒</span>
            </el-form-item>
            <el-form-item label="原因">
              <el-input v-model="banPlayer.reason" placeholder="封禁原因" />
            </el-form-item>
            <el-form-item label="备份证据">
              <el-input v-model="banPlayer.backupReference" maxlength="128" placeholder="backup-or-change-reference" />
            </el-form-item>
            <el-form-item>
              <el-button type="danger" :loading="banPlayer.loading" @click="handleBanPlayer">
                封禁玩家
              </el-button>
            </el-form-item>
          </el-form>
        </el-card>
      </el-col>
    </el-row>
  </AdminLayout>
</template>

<script setup>
import { computed, reactive } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import { useAuthStore } from "../stores/auth";
import { gmApi } from "../api";
import { ADMIN_PERMISSIONS as P } from "../auth/permissions";
import { formatHighRiskPreview, runHighRiskOperation } from "../operations/high-risk";

const authStore = useAuthStore();

const broadcast = reactive({
  title: "",
  content: "",
  sender: "System",
  reason: "",
  backupReference: "",
  loading: false
});

const sendItem = reactive({
  characterId: "",
  itemId: "",
  itemCount: 1,
  reason: "",
  backupReference: "",
  loading: false
});

const kickPlayer = reactive({
  playerId: "",
  reason: "",
  backupReference: "",
  loading: false
});

const banPlayer = reactive({
  playerId: "",
  durationSeconds: 3600,
  reason: "",
  backupReference: "",
  loading: false
});

const operation = reactive({
  state: "idle",
  requestId: "",
  message: ""
});

const operationAlertType = computed(() => {
  if (operation.state === "failed") return "error";
  if (operation.state === "in_progress" || operation.state === "preflight") return "warning";
  return "success";
});

async function executeHighRisk(invoke, payload, title) {
  operation.state = "preflight";
  operation.message = "正在生成服务端影响预览。";
  const outcome = await runHighRiskOperation({
    invoke,
    payload,
    confirm: async (preflight) => {
      operation.state = "preflight";
      operation.message = "已生成影响预览，等待明确确认。";
      try {
        await ElMessageBox.confirm(formatHighRiskPreview(preflight), `${title}确认`, {
          type: "warning",
          confirmButtonText: "确认执行",
          cancelButtonText: "取消",
          distinguishCancelAndClose: true
        });
        return true;
      } catch {
        return false;
      }
    }
  });
  operation.state = outcome.phase;
  operation.requestId = outcome.requestId;
  operation.message = outcome.phase === "cancelled"
    ? "操作已取消，未执行写入。"
    : outcome.phase === "in_progress"
      ? `请求 ${outcome.requestId} 正在执行，请勿重复提交。`
      : outcome.phase === "terminal"
        ? `请求 ${outcome.requestId} 已返回首次终态。`
        : "操作已完成并记录审计。";
  return outcome;
}

async function handleBroadcast() {
  if (!broadcast.title || !broadcast.content || !broadcast.reason.trim() || !broadcast.backupReference.trim()) {
    ElMessage.warning("请填写标题、内容、操作原因和备份证据");
    return;
  }
  broadcast.loading = true;
  try {
    const outcome = await executeHighRisk(gmApi.broadcast, {
      title: broadcast.title,
      content: broadcast.content,
      sender: broadcast.sender,
      reason: broadcast.reason.trim(),
      backupReference: broadcast.backupReference.trim()
    }, "发送广播");
    if (outcome.phase === "cancelled" || outcome.phase === "in_progress") return;
    ElMessage.success("广播已发送");
    broadcast.title = "";
    broadcast.content = "";
    broadcast.reason = "";
    broadcast.backupReference = "";
  } catch (err) {
    operation.state = "failed";
    operation.message = err.response?.data?.message || "广播操作失败";
    ElMessage.error(operation.message);
  } finally {
    broadcast.loading = false;
  }
}

async function handleSendItem() {
  if (!sendItem.characterId || !sendItem.itemId || !sendItem.reason.trim() || !sendItem.backupReference.trim()) {
    ElMessage.warning("请填写角色ID、道具ID、操作原因和备份证据");
    return;
  }
  sendItem.loading = true;
  try {
    const outcome = await executeHighRisk(gmApi.sendItem, {
      characterId: sendItem.characterId,
      itemId: sendItem.itemId,
      itemCount: sendItem.itemCount,
      reason: sendItem.reason.trim(),
      backupReference: sendItem.backupReference.trim()
    }, "发放道具");
    if (outcome.phase === "cancelled" || outcome.phase === "in_progress") return;
    ElMessage.success("道具已发放");
    sendItem.characterId = "";
    sendItem.itemId = "";
    sendItem.itemCount = 1;
    sendItem.reason = "";
    sendItem.backupReference = "";
  } catch (err) {
    operation.state = "failed";
    operation.message = err.response?.data?.message || "发放操作失败";
    ElMessage.error(operation.message);
  } finally {
    sendItem.loading = false;
  }
}

async function handleKickPlayer() {
  if (!kickPlayer.playerId || !kickPlayer.reason.trim() || !kickPlayer.backupReference.trim()) {
    ElMessage.warning("请填写玩家ID、操作原因和备份证据");
    return;
  }
  kickPlayer.loading = true;
  try {
    const outcome = await executeHighRisk(gmApi.kickPlayer, {
      playerId: kickPlayer.playerId,
      reason: kickPlayer.reason.trim(),
      backupReference: kickPlayer.backupReference.trim()
    }, "踢出玩家");
    if (outcome.phase === "cancelled" || outcome.phase === "in_progress") return;
    ElMessage.success("玩家已被踢出");
    kickPlayer.playerId = "";
    kickPlayer.reason = "";
    kickPlayer.backupReference = "";
  } catch (err) {
    operation.state = "failed";
    operation.message = err.response?.data?.message || "踢人操作失败";
    ElMessage.error(operation.message);
  } finally {
    kickPlayer.loading = false;
  }
}

async function handleBanPlayer() {
  if (!banPlayer.playerId || !banPlayer.reason.trim() || !banPlayer.backupReference.trim()) {
    ElMessage.warning("请填写玩家ID、操作原因和备份证据");
    return;
  }
  banPlayer.loading = true;
  try {
    const outcome = await executeHighRisk(gmApi.banPlayer, {
      playerId: banPlayer.playerId,
      durationSeconds: banPlayer.durationSeconds,
      reason: banPlayer.reason.trim(),
      backupReference: banPlayer.backupReference.trim()
    }, "封禁玩家");
    if (outcome.phase === "cancelled" || outcome.phase === "in_progress") return;
    ElMessage.success("玩家已被封禁");
    banPlayer.playerId = "";
    banPlayer.durationSeconds = 3600;
    banPlayer.reason = "";
    banPlayer.backupReference = "";
  } catch (err) {
    operation.state = "failed";
    operation.message = err.response?.data?.message || "高风险操作失败";
    ElMessage.error(err.response?.data?.message || "操作失败");
  } finally {
    banPlayer.loading = false;
  }
}
</script>
