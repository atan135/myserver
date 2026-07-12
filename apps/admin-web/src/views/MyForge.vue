<template>
  <AdminLayout>
    <div class="myforge-page">
      <div class="page-header">
        <h2>MyForge 蓝图</h2>
        <el-button :loading="refreshing" @click="refreshVisibleSections">刷新</el-button>
      </div>

      <div class="status-grid">
        <el-card v-loading="agents.loading">
          <template #header>
            <span>Agent 状态</span>
          </template>

          <el-alert
            v-if="!canReadAgents"
            title="无权限查看 Agent"
            description="当前账号缺少 myforge.agent.read 权限。"
            type="warning"
            :closable="false"
            show-icon
          />
          <template v-else>
            <el-alert
              v-if="agents.error"
              :title="agents.error.title"
              :description="agents.error.description"
              type="error"
              :closable="false"
              show-icon
            />
            <template v-else>
              <div class="stat-row">
                <el-statistic title="已配置" :value="agents.items.length" />
                <el-statistic title="在线" :value="onlineAgents.length" />
                <el-statistic title="离线" :value="offlineAgents.length" />
              </div>
              <el-alert
                v-if="offlineAgents.length"
                class="state-alert"
                title="存在离线 Agent"
                :description="offlineDescription"
                type="warning"
                :closable="false"
                show-icon
              />
              <el-empty
                v-if="!agents.loading && !agents.items.length"
                description="暂无已配置的 Agent"
                :image-size="72"
              />
            </template>
          </template>
        </el-card>

        <el-card v-loading="tasks.loading">
          <template #header>
            <span>任务状态</span>
          </template>

          <el-alert
            v-if="!canReadTasks"
            title="无权限查看任务"
            description="当前账号缺少 myforge.task.read 权限。"
            type="warning"
            :closable="false"
            show-icon
          />
          <template v-else>
            <el-alert
              v-if="tasks.error"
              :title="tasks.error.title"
              :description="tasks.error.description"
              type="error"
              :closable="false"
              show-icon
            />
            <el-statistic v-else title="任务总数" :value="tasks.total" />

            <el-divider />
            <el-form class="lookup-form" @submit.prevent="lookupTask">
              <el-form-item label="任务 ID">
                <el-input
                  v-model="taskLookup.requestId"
                  clearable
                  placeholder="输入 requestId"
                />
              </el-form-item>
              <el-button type="primary" :loading="taskLookup.loading" @click="lookupTask">
                查询
              </el-button>
            </el-form>

            <el-alert
              v-if="taskLookup.error"
              class="state-alert"
              :title="taskLookup.error.title"
              :description="taskLookup.error.description"
              :type="taskLookup.error.type || 'error'"
              :closable="false"
              show-icon
            />
            <el-descriptions v-if="taskLookup.task" :column="1" border>
              <el-descriptions-item label="任务 ID">
                <span class="mono">{{ taskLookup.task.requestId }}</span>
              </el-descriptions-item>
              <el-descriptions-item label="Agent">
                {{ taskLookup.task.agentId }}
              </el-descriptions-item>
              <el-descriptions-item label="状态">
                <el-tag>{{ taskLookup.task.status }}</el-tag>
              </el-descriptions-item>
            </el-descriptions>
          </template>
        </el-card>
      </div>
    </div>
  </AdminLayout>
</template>

<script setup>
import { computed, onMounted, reactive } from "vue";
import AdminLayout from "../components/AdminLayout.vue";
import { myforgeApi } from "../api";
import { normalizeMyforgeError } from "../api/myforge-errors";
import { ADMIN_PERMISSIONS as P } from "../auth/permissions";
import { useAuthStore } from "../stores/auth";

const authStore = useAuthStore();
const canReadAgents = computed(() => authStore.hasPermission(P.MYFORGE_AGENT_READ));
const canReadTasks = computed(() => authStore.hasPermission(P.MYFORGE_TASK_READ));

const agents = reactive({
  loading: false,
  items: [],
  error: null
});
const tasks = reactive({
  loading: false,
  total: 0,
  error: null
});
const taskLookup = reactive({
  requestId: "",
  loading: false,
  task: null,
  error: null
});

const onlineAgents = computed(() => agents.items.filter((agent) => agent.status === "online"));
const offlineAgents = computed(() => agents.items.filter((agent) => agent.status === "offline"));
const offlineDescription = computed(() => {
  const labels = offlineAgents.value.map((agent) => agent.label || agent.agentId);
  return `${labels.join("、")} 当前离线，相关任务可能进入等待队列。`;
});
const refreshing = computed(() => agents.loading || tasks.loading);

async function fetchAgents() {
  if (!canReadAgents.value) return;

  agents.loading = true;
  agents.error = null;
  try {
    const { data } = await myforgeApi.getAgents();
    agents.items = Array.isArray(data.items) ? data.items : [];
  } catch (error) {
    agents.items = [];
    agents.error = normalizeMyforgeError(error, {
      title: "Agent 状态加载失败",
      fallbackMessage: "无法加载 Agent 状态，请稍后重试。"
    });
  } finally {
    agents.loading = false;
  }
}

async function fetchTasks() {
  if (!canReadTasks.value) return;

  tasks.loading = true;
  tasks.error = null;
  try {
    const { data } = await myforgeApi.getTasks({ limit: 1, offset: 0 });
    tasks.total = Number.isFinite(data.total) ? data.total : 0;
  } catch (error) {
    tasks.total = 0;
    tasks.error = normalizeMyforgeError(error, {
      title: "任务状态加载失败",
      fallbackMessage: "无法加载任务状态，请稍后重试。"
    });
  } finally {
    tasks.loading = false;
  }
}

async function lookupTask() {
  const requestId = taskLookup.requestId.trim();
  taskLookup.task = null;
  taskLookup.error = null;
  if (!requestId) {
    taskLookup.error = {
      title: "请输入任务 ID",
      description: "任务 ID 不能为空。",
      type: "warning"
    };
    return;
  }

  taskLookup.loading = true;
  try {
    const { data } = await myforgeApi.getTask(requestId);
    taskLookup.task = data.task;
  } catch (error) {
    taskLookup.error = normalizeMyforgeError(error, {
      taskDetailLookup: true,
      notFoundMessage: "未找到该任务，请检查任务 ID 是否正确。",
      title: "任务查询失败",
      fallbackMessage: "无法查询任务，请稍后重试。"
    });
  } finally {
    taskLookup.loading = false;
  }
}

function refreshVisibleSections() {
  return Promise.all([fetchAgents(), fetchTasks()]);
}

onMounted(refreshVisibleSections);
</script>

<style scoped>
.myforge-page {
  min-width: 0;
}

.page-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 16px;
}

.page-header h2 {
  font-size: 20px;
}

.status-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.stat-row {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
}

.state-alert {
  margin-top: 16px;
}

.lookup-form {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: start;
  gap: 12px;
}

.lookup-form :deep(.el-form-item) {
  margin-bottom: 0;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
  word-break: break-all;
}

@media (max-width: 900px) {
  .status-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}

@media (max-width: 600px) {
  .lookup-form {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
