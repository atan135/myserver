<template>
  <AdminLayout>
    <h3>GM 命令</h3>

    <el-row :gutter="20">
      <!-- 广播 -->
      <el-col :span="12">
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
            <el-form-item>
              <el-button type="primary" :loading="broadcast.loading" @click="handleBroadcast">
                发送广播
              </el-button>
            </el-form-item>
          </el-form>
        </el-card>
      </el-col>

      <!-- 发放道具 -->
      <el-col :span="12">
        <el-card style="margin-top: 20px">
          <template #header>
            <span>发放道具</span>
          </template>
          <el-form :model="sendItem" label-width="80px">
            <el-form-item label="玩家ID">
              <el-input v-model="sendItem.playerId" placeholder="player-xxx" />
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
      <el-col :span="12">
        <el-card style="margin-top: 20px">
          <template #header>
            <span>踢出玩家</span>
          </template>
          <el-form :model="kickPlayer" label-width="80px">
            <el-form-item label="玩家ID">
              <el-input v-model="kickPlayer.playerId" placeholder="player-xxx" />
            </el-form-item>
            <el-form-item label="原因">
              <el-input v-model="kickPlayer.reason" placeholder="原因（可选）" />
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
      <el-col :span="12" v-if="authStore.isAdmin">
        <el-card style="margin-top: 20px">
          <template #header>
            <span>封禁玩家</span>
          </template>
          <el-form :model="banPlayer" label-width="80px">
            <el-form-item label="玩家ID">
              <el-input v-model="banPlayer.playerId" placeholder="player-xxx" />
            </el-form-item>
            <el-form-item label="封禁时长">
              <el-input-number v-model="banPlayer.durationSeconds" :min="60" :max="31536000" />
              <span style="margin-left: 8px; color: #999">秒</span>
            </el-form-item>
            <el-form-item label="原因">
              <el-input v-model="banPlayer.reason" placeholder="封禁原因" />
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
import { reactive } from "vue";
import { ElMessage } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import { useAuthStore } from "../stores/auth";
import { gmApi } from "../api";

const authStore = useAuthStore();

const broadcast = reactive({
  title: "",
  content: "",
  sender: "System",
  loading: false
});

const sendItem = reactive({
  playerId: "",
  itemId: "",
  itemCount: 1,
  reason: "",
  loading: false
});

const kickPlayer = reactive({
  playerId: "",
  reason: "",
  loading: false
});

const banPlayer = reactive({
  playerId: "",
  durationSeconds: 3600,
  reason: "",
  loading: false
});

async function handleBroadcast() {
  if (!broadcast.title || !broadcast.content) {
    ElMessage.warning("请填写标题和内容");
    return;
  }
  broadcast.loading = true;
  try {
    await gmApi.broadcast({
      title: broadcast.title,
      content: broadcast.content,
      sender: broadcast.sender
    });
    ElMessage.success("广播已发送");
    broadcast.title = "";
    broadcast.content = "";
  } catch (err) {
    ElMessage.error(err.response?.data?.message || "发送失败");
  } finally {
    broadcast.loading = false;
  }
}

async function handleSendItem() {
  if (!sendItem.playerId || !sendItem.itemId) {
    ElMessage.warning("请填写玩家ID和道具ID");
    return;
  }
  sendItem.loading = true;
  try {
    await gmApi.sendItem({
      playerId: sendItem.playerId,
      itemId: sendItem.itemId,
      itemCount: sendItem.itemCount,
      reason: sendItem.reason
    });
    ElMessage.success("道具已发放");
    sendItem.playerId = "";
    sendItem.itemId = "";
    sendItem.itemCount = 1;
    sendItem.reason = "";
  } catch (err) {
    ElMessage.error(err.response?.data?.message || "发放失败");
  } finally {
    sendItem.loading = false;
  }
}

async function handleKickPlayer() {
  if (!kickPlayer.playerId) {
    ElMessage.warning("请填写玩家ID");
    return;
  }
  kickPlayer.loading = true;
  try {
    await gmApi.kickPlayer({
      playerId: kickPlayer.playerId,
      reason: kickPlayer.reason
    });
    ElMessage.success("玩家已被踢出");
    kickPlayer.playerId = "";
    kickPlayer.reason = "";
  } catch (err) {
    ElMessage.error(err.response?.data?.message || "操作失败");
  } finally {
    kickPlayer.loading = false;
  }
}

async function handleBanPlayer() {
  if (!banPlayer.playerId) {
    ElMessage.warning("请填写玩家ID");
    return;
  }
  banPlayer.loading = true;
  try {
    await gmApi.banPlayer({
      playerId: banPlayer.playerId,
      durationSeconds: banPlayer.durationSeconds,
      reason: banPlayer.reason
    });
    ElMessage.success("玩家已被封禁");
    banPlayer.playerId = "";
    banPlayer.durationSeconds = 3600;
    banPlayer.reason = "";
  } catch (err) {
    ElMessage.error(err.response?.data?.message || "操作失败");
  } finally {
    banPlayer.loading = false;
  }
}
</script>
