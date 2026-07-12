<template>
  <AdminLayout>
    <main class="myforge-page">
      <header class="page-header">
        <h1>MyForge 蓝图任务</h1>
        <div class="header-actions">
          <el-tooltip content="刷新当前视图" placement="bottom">
            <el-button
              :icon="Refresh"
              circle
              aria-label="刷新当前视图"
              :loading="refreshing"
              @click="refreshVisibleSection"
            />
          </el-tooltip>
          <el-button
            v-if="canCreateTasks"
            type="primary"
            :icon="Plus"
            :disabled="createSubmitting"
            @click="openCreateDialog"
          >
            新建任务
          </el-button>
        </div>
      </header>

      <el-tabs v-model="activeTab" class="workspace-tabs">
        <el-tab-pane v-if="canReadTasks" label="任务" name="tasks">
          <section class="workspace-section" aria-label="任务列表">
            <el-form class="filter-bar" :inline="true" @submit.prevent="applyTaskFilters">
              <el-form-item label="Agent">
                <el-select
                  v-model="taskFilters.agentKey"
                  clearable
                  filterable
                  placeholder="全部 Agent"
                  :disabled="!canReadAgents || agents.loading"
                  @change="markTaskFiltersDirty"
                >
                  <el-option
                    v-for="agent in agents.items"
                    :key="agentIdentityKey(agent)"
                    :label="agentOptionLabel(agent)"
                    :value="agentIdentityKey(agent)"
                  />
                </el-select>
              </el-form-item>
              <el-form-item label="状态">
                <el-select
                  v-model="taskFilters.status"
                  clearable
                  placeholder="全部状态"
                  @change="markTaskFiltersDirty"
                >
                  <el-option
                    v-for="option in TASK_STATUS_OPTIONS"
                    :key="option.value"
                    :label="option.label"
                    :value="option.value"
                  />
                </el-select>
              </el-form-item>
              <el-form-item class="filter-actions">
                <el-button
                  type="primary"
                  :icon="Search"
                  :loading="tasks.loading"
                  @click="applyTaskFilters"
                >
                  查询
                </el-button>
                <el-button :disabled="tasks.loading" @click="resetTaskFilters">重置</el-button>
              </el-form-item>
            </el-form>

            <el-alert
              v-if="tasks.error"
              class="section-alert"
              :title="tasks.error.title"
              type="error"
              :closable="false"
              show-icon
            >
              <div class="alert-content">
                <span>{{ tasks.error.description }}</span>
                <el-button size="small" @click="fetchTasks({ showLoading: true })">重试</el-button>
              </div>
            </el-alert>

            <div class="table-scroll">
              <el-table
                v-loading="tasks.loading"
                class="task-table"
                :data="tasks.items"
                row-key="requestId"
                size="small"
                empty-text="暂无 MyForge 任务"
              >
                <el-table-column label="Request ID" width="270">
                  <template #default="{ row }">
                    <el-button link type="primary" class="request-link" @click="openTask(row.requestId)">
                      {{ row.requestId }}
                    </el-button>
                  </template>
                </el-table-column>
                <el-table-column label="任务类型" width="165">
                  <template #default="{ row }">
                    <span>{{ taskTypeLabel(row.taskType) }}</span>
                  </template>
                </el-table-column>
                <el-table-column label="Agent / 项目" width="210">
                  <template #default="{ row }">
                    <div class="stacked-value">
                      <span class="mono">{{ formatValue(row.agentId) }}</span>
                      <span class="secondary mono">{{ formatValue(row.projectId) }}</span>
                    </div>
                  </template>
                </el-table-column>
                <el-table-column label="状态" width="180">
                  <template #default="{ row }">
                    <div class="stacked-value">
                      <el-tag :type="taskStatusTagType(row.status)" size="small">
                        {{ taskStatusLabel(row.status) }}
                      </el-tag>
                      <span v-if="row.queueReason" class="secondary queue-reason">
                        {{ queueReasonLabel(row.queueReason) }}
                      </span>
                      <span v-if="row.errorCode" class="secondary error-code mono">{{ row.errorCode }}</span>
                    </div>
                  </template>
                </el-table-column>
                <el-table-column label="执行权限" width="145">
                  <template #default="{ row }">
                    <el-tag :type="dangerFullAccessState(row.dangerFullAccess).tagType" size="small">
                      {{ dangerFullAccessState(row.dangerFullAccess).label }}
                    </el-tag>
                  </template>
                </el-table-column>
                <el-table-column label="创建人" width="140">
                  <template #default="{ row }">
                    <div class="stacked-value">
                      <span>{{ row.createdBy?.username || "--" }}</span>
                      <span v-if="row.createdBy?.adminId" class="secondary mono">
                        {{ row.createdBy.adminId }}
                      </span>
                    </div>
                  </template>
                </el-table-column>
                <el-table-column label="创建时间" width="170">
                  <template #default="{ row }">{{ formatDateTime(row.createdAt) }}</template>
                </el-table-column>
                <el-table-column label="开始时间" width="170">
                  <template #default="{ row }">{{ formatDateTime(row.startedAt) }}</template>
                </el-table-column>
                <el-table-column label="完成时间" width="170">
                  <template #default="{ row }">{{ formatDateTime(row.completedAt) }}</template>
                </el-table-column>
                <el-table-column label="耗时" width="105">
                  <template #default="{ row }">{{ formatDuration(row.durationMs) }}</template>
                </el-table-column>
                <el-table-column label="操作" width="70" fixed="right" align="center">
                  <template #default="{ row }">
                    <el-tooltip content="查看任务详情" placement="left">
                      <el-button
                        link
                        type="primary"
                        :icon="View"
                        aria-label="查看任务详情"
                        @click="openTask(row.requestId)"
                      />
                    </el-tooltip>
                  </template>
                </el-table-column>
              </el-table>
            </div>

            <div class="list-footer">
              <span class="list-status">
                共 {{ tasks.total }} 条
                <span v-if="tasks.polling"> · 正在刷新</span>
                <span v-else-if="hasActiveTasks"> · 活动任务每 5 秒刷新</span>
              </span>
              <el-pagination
                v-model:current-page="tasks.page"
                v-model:page-size="tasks.pageSize"
                :page-sizes="[20, 50, 100]"
                :total="pageableTaskTotal"
                :disabled="tasks.loading"
                layout="sizes, prev, pager, next"
                background
                @current-change="handleTaskPageChange"
                @size-change="handleTaskPageSizeChange"
              />
            </div>
          </section>
        </el-tab-pane>

        <el-tab-pane v-if="canReadAgents" label="Agent" name="agents">
          <section class="workspace-section" aria-label="Agent 在线状态">
            <el-alert
              v-if="agents.error"
              class="section-alert"
              :title="agents.error.title"
              type="error"
              :closable="false"
              show-icon
            >
              <div class="alert-content">
                <span>{{ agents.error.description }}</span>
                <el-button size="small" @click="fetchAgents">重试</el-button>
              </div>
            </el-alert>

            <div class="agent-summary">
              <span>已配置 {{ agents.items.length }}</span>
              <span class="online-dot" />
              <span>在线 {{ onlineAgentCount }}</span>
              <span class="offline-dot" />
              <span>离线 {{ offlineAgentCount }}</span>
            </div>

            <div class="table-scroll">
              <el-table
                v-loading="agents.loading"
                class="agent-table"
                :data="agents.items"
                :row-key="agentIdentityKey"
                size="small"
                empty-text="暂无已配置的 Agent"
              >
                <el-table-column label="Agent" width="220">
                  <template #default="{ row }">
                    <div class="stacked-value">
                      <span>{{ row.label || row.agentId }}</span>
                      <span class="secondary mono">{{ row.agentId }}</span>
                    </div>
                  </template>
                </el-table-column>
                <el-table-column prop="projectId" label="Project ID" width="180">
                  <template #default="{ row }"><span class="mono">{{ row.projectId }}</span></template>
                </el-table-column>
                <el-table-column label="状态" width="100">
                  <template #default="{ row }">
                    <el-tag :type="row.status === 'online' ? 'success' : 'info'" size="small">
                      {{ row.status === "online" ? "在线" : "离线" }}
                    </el-tag>
                  </template>
                </el-table-column>
                <el-table-column label="执行权限" width="165">
                  <template #default="{ row }">
                    <div class="stacked-value">
                      <el-tag
                        :type="dangerFullAccessState(row.capabilities?.dangerFullAccess).tagType"
                        size="small"
                      >
                        {{ dangerFullAccessState(row.capabilities?.dangerFullAccess).label }}
                      </el-tag>
                      <span
                        v-if="row.capabilities?.dangerFullAccess === true"
                        class="secondary permission-risk"
                      >
                        绕过审批与沙箱
                      </span>
                    </div>
                  </template>
                </el-table-column>
                <el-table-column label="Hostname" width="170">
                  <template #default="{ row }">{{ formatValue(row.hostname) }}</template>
                </el-table-column>
                <el-table-column label="Platform / 版本" width="180">
                  <template #default="{ row }">
                    <div class="stacked-value">
                      <span>{{ formatValue(row.platform) }}</span>
                      <span class="secondary">{{ formatValue(row.agentVersion) }}</span>
                    </div>
                  </template>
                </el-table-column>
                <el-table-column label="Forge Root" min-width="240">
                  <template #default="{ row }">
                    <pre class="table-json">{{ formatCompact(row.forgeRootSummary) }}</pre>
                  </template>
                </el-table-column>
                <el-table-column label="Capabilities" min-width="270">
                  <template #default="{ row }">
                    <pre class="table-json">{{ formatCompact(row.capabilities) }}</pre>
                  </template>
                </el-table-column>
                <el-table-column label="Last Seen" width="170">
                  <template #default="{ row }">{{ formatDateTime(row.lastSeenAt) }}</template>
                </el-table-column>
              </el-table>
            </div>
          </section>
        </el-tab-pane>
      </el-tabs>

      <el-dialog
        v-model="createDialogVisible"
        class="create-dialog"
        title="新建方圆灵构蓝图任务"
        top="5vh"
        width="min(780px, calc(100vw - 32px))"
        :close-on-click-modal="!createSubmitting"
        :close-on-press-escape="!createSubmitting"
        :show-close="!createSubmitting"
        destroy-on-close
        @closed="resetCreateForm"
      >
        <el-alert
          v-if="createError"
          class="dialog-alert"
          :title="createError.title"
          :description="createError.description"
          type="error"
          :closable="false"
          show-icon
        />
        <el-alert
          v-if="!canReadAgents"
          class="dialog-alert"
          title="无法读取 Agent"
          description="当前账号缺少 myforge.agent.read 权限，不能选择执行 Agent。"
          type="warning"
          :closable="false"
          show-icon
        />
        <el-alert
          v-else-if="agents.error"
          class="dialog-alert"
          :title="agents.error.title"
          :description="agents.error.description"
          type="error"
          :closable="false"
          show-icon
        />
        <el-alert
          v-else-if="!agents.loading && agents.items.length === 0"
          class="dialog-alert"
          title="暂无可选 Agent"
          description="管理接口未返回已配置的 Agent，当前不能创建任务。"
          type="warning"
          :closable="false"
          show-icon
        />

        <el-form class="create-form" label-position="top" @submit.prevent="submitCreateTask">
          <el-form-item label="Agent" required :error="createErrors.agentKey">
            <el-select
              v-model="createForm.agentKey"
              class="full-width"
              filterable
              placeholder="选择执行 Agent"
              :loading="agents.loading"
              :disabled="!canReadAgents"
              @change="clearCreateError('agentKey')"
            >
              <el-option
                v-for="agent in agents.items"
                :key="agentIdentityKey(agent)"
                :label="agentOptionLabel(agent)"
                :value="agentIdentityKey(agent)"
              >
                <div class="agent-option">
                  <span>{{ agent.label || agent.agentId }}</span>
                  <span>
                    {{ agent.projectId }} · {{ agent.status === "online" ? "在线" : "离线" }} ·
                    {{ dangerFullAccessState(agent.capabilities?.dangerFullAccess).label }}
                  </span>
                </div>
              </el-option>
            </el-select>
          </el-form-item>

          <el-alert
            v-if="selectedCreateAgent?.status === 'offline'"
            class="dialog-alert"
            title="所选 Agent 当前离线"
            description="任务仍可创建，将以 queued 状态等待 Agent 连接。"
            type="warning"
            :closable="false"
            show-icon
          />

          <el-alert
            v-if="selectedCreateAgent"
            class="dialog-alert"
            :title="selectedCreatePermission.label"
            :description="`${selectedCreatePermission.description} 此处只展示 Agent 的本机设置，无法远程切换该权限。`"
            :type="selectedCreatePermission.key === 'enabled'
              ? 'error'
              : selectedCreatePermission.key === 'disabled' ? 'success' : 'warning'"
            :closable="false"
            show-icon
          />

          <el-form-item label="主题" required :error="createErrors.theme">
            <el-input
              v-model="createForm.theme"
              maxlength="200"
              show-word-limit
              placeholder="例如：竹林中的小型方圆庭院"
              @input="clearCreateError('theme')"
            />
          </el-form-item>

          <div class="numeric-grid">
            <el-form-item label="Primitive 数量上限" required :error="createErrors.primitiveLimit">
              <el-input-number
                v-model="createForm.primitiveLimit"
                :min="1"
                :max="1000"
                :step="10"
                controls-position="right"
                @change="clearCreateError('primitiveLimit')"
              />
            </el-form-item>
            <el-form-item label="宽度" required :error="createErrors.width">
              <el-input-number
                v-model="createForm.bounds.width"
                :min="1"
                :max="1000"
                controls-position="right"
                @change="clearCreateError('width')"
              />
            </el-form-item>
            <el-form-item label="深度" required :error="createErrors.depth">
              <el-input-number
                v-model="createForm.bounds.depth"
                :min="1"
                :max="1000"
                controls-position="right"
                @change="clearCreateError('depth')"
              />
            </el-form-item>
            <el-form-item label="高度" required :error="createErrors.height">
              <el-input-number
                v-model="createForm.bounds.height"
                :min="1"
                :max="1000"
                controls-position="right"
                @change="clearCreateError('height')"
              />
            </el-form-item>
          </div>

          <el-form-item label="生成要求" required :error="createErrors.requirements">
            <div class="requirements-editor">
              <div
                v-for="(_, index) in createForm.requirements"
                :key="index"
                class="requirement-row"
              >
                <span class="requirement-index">{{ index + 1 }}</span>
                <el-input
                  v-model="createForm.requirements[index]"
                  type="textarea"
                  :autosize="{ minRows: 2, maxRows: 4 }"
                  maxlength="500"
                  placeholder="输入一项约束或验收要求"
                  @input="clearCreateError('requirements')"
                />
                <el-tooltip content="删除此项" placement="left">
                  <el-button
                    :icon="Delete"
                    circle
                    aria-label="删除此项"
                    :disabled="createForm.requirements.length === 1"
                    @click="removeRequirement(index)"
                  />
                </el-tooltip>
              </div>
              <el-button
                :icon="Plus"
                :disabled="createForm.requirements.length >= 32"
                @click="addRequirement"
              >
                添加要求
              </el-button>
            </div>
          </el-form-item>

          <el-form-item label="产物路径" required :error="createErrors.artifactFile">
            <el-input
              v-model="createForm.artifactFile"
              class="mono-input"
              maxlength="512"
              placeholder="artifacts/fangyuan/example.ron"
              @input="clearCreateError('artifactFile')"
            />
          </el-form-item>
          <el-form-item label="消费端目标路径（可选）" :error="createErrors.consumerTargetFile">
            <el-input
              v-model="createForm.consumerTargetFile"
              class="mono-input"
              maxlength="512"
              clearable
              placeholder="project/assets/fangyuan/example.ron"
              @input="clearCreateError('consumerTargetFile')"
            />
          </el-form-item>
          <el-form-item
            label="规则文件"
            :required="createForm.useRulesFile"
            :error="createErrors.rulesFile"
          >
            <div class="rules-control">
              <el-switch
                v-model="createForm.useRulesFile"
                active-text="使用规则文件"
                inactive-text="无规则执行"
                @change="handleUseRulesFileChange"
              />
              <el-input
                v-if="createForm.useRulesFile"
                v-model="createForm.rulesFile"
                class="mono-input"
                maxlength="512"
                placeholder="rules/fangyuan/default.md"
                @input="clearCreateError('rulesFile')"
              />
              <span v-else class="field-note">仅采用当前任务中填写的生成约束</span>
            </div>
          </el-form-item>
        </el-form>

        <template #footer>
          <el-button :disabled="createSubmitting" @click="createDialogVisible = false">取消</el-button>
          <el-button
            type="primary"
            :loading="createSubmitting"
            :disabled="!canReadAgents || agents.items.length === 0"
            @click="submitCreateTask"
          >
            创建任务
          </el-button>
        </template>
      </el-dialog>
    </main>
  </AdminLayout>
</template>

<script setup>
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { ElMessage } from "element-plus";
import { Delete, Plus, Refresh, Search, View } from "@element-plus/icons-vue";
import AdminLayout from "../components/AdminLayout.vue";
import { myforgeApi } from "../api";
import { normalizeMyforgeError } from "../api/myforge-errors";
import { ADMIN_PERMISSIONS as P } from "../auth/permissions";
import {
  TASK_STATUS_OPTIONS,
  buildFangyuanTaskRequest,
  dangerFullAccessState,
  formatDuration,
  formatJson,
  isActiveTaskStatus,
  isCurrentTaskQueryAttempt,
  queueReasonLabel,
  taskStatusLabel,
  taskStatusTagType,
  taskTypeLabel,
  validateFangyuanTaskForm
} from "../myforge/task-utils";
import { useAuthStore } from "../stores/auth";

const router = useRouter();
const authStore = useAuthStore();
const canReadAgents = computed(() => authStore.hasPermission(P.MYFORGE_AGENT_READ));
const canReadTasks = computed(() => authStore.hasPermission(P.MYFORGE_TASK_READ));
const canCreateTasks = computed(() => authStore.hasPermission(P.MYFORGE_TASK_CREATE));
const activeTab = ref(canReadTasks.value ? "tasks" : "agents");
const taskFiltersDirty = ref(false);
const createDialogVisible = ref(false);
const createSubmitting = ref(false);
const createError = ref(null);
const createErrors = reactive({});
let destroyed = false;
let agentRequestSequence = 0;
let taskRequestSequence = 0;
let taskQueryRevision = 0;
let createAttemptSequence = 0;
let taskPollTimer = null;

const agents = reactive({
  loading: false,
  items: [],
  error: null
});

const tasks = reactive({
  loading: false,
  polling: false,
  items: [],
  total: 0,
  page: 1,
  pageSize: 20,
  error: null
});

const taskFilters = reactive({
  agentKey: "",
  status: ""
});

const createForm = reactive({
  agentKey: "",
  theme: "",
  primitiveLimit: 120,
  bounds: { width: 80, depth: 80, height: 40 },
  requirements: [""],
  artifactFile: "",
  consumerTargetFile: "",
  useRulesFile: true,
  rulesFile: "rules/fangyuan/方圆灵构蓝图规则.md"
});

const onlineAgentCount = computed(() => agents.items.filter((agent) => agent.status === "online").length);
const offlineAgentCount = computed(() => agents.items.filter((agent) => agent.status !== "online").length);
const hasActiveTasks = computed(() => tasks.items.some((task) => isActiveTaskStatus(task.status)));
const pageableTaskTotal = computed(() => Math.min(tasks.total, 100000 + tasks.pageSize));
const refreshing = computed(() => activeTab.value === "tasks"
  ? tasks.loading || tasks.polling
  : agents.loading);
const selectedCreateAgent = computed(() => findAgent(createForm.agentKey));
const selectedCreatePermission = computed(() => dangerFullAccessState(
  selectedCreateAgent.value?.capabilities?.dangerFullAccess
));

function agentIdentityKey(agent) {
  return `${agent.agentId}::${agent.projectId}`;
}

function findAgent(key) {
  return agents.items.find((agent) => agentIdentityKey(agent) === key) || null;
}

function agentOptionLabel(agent) {
  const label = agent.label && agent.label !== agent.agentId ? `${agent.label} (${agent.agentId})` : agent.agentId;
  const permission = dangerFullAccessState(agent.capabilities?.dangerFullAccess).label;
  return `${label} · ${agent.projectId} · ${agent.status === "online" ? "在线" : "离线"} · ${permission}`;
}

function formatValue(value) {
  return value === null || value === undefined || value === "" ? "--" : String(value);
}

function formatDateTime(value) {
  if (!value) return "--";
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "--";
  return date.toLocaleString("zh-CN", { hour12: false });
}

function formatCompact(value) {
  const formatted = formatJson(value);
  return formatted === "--" ? formatted : formatted.replace(/\n\s*/g, " ");
}

async function fetchAgents() {
  if (!canReadAgents.value || agents.loading) return;
  const sequence = ++agentRequestSequence;
  agents.loading = true;
  agents.error = null;
  try {
    const { data } = await myforgeApi.getAgents();
    if (destroyed || sequence !== agentRequestSequence) return;
    agents.items = Array.isArray(data?.items) ? data.items : [];
    if (taskFilters.agentKey && !findAgent(taskFilters.agentKey)) {
      taskFilters.agentKey = "";
      markTaskFiltersDirty();
    }
  } catch (error) {
    if (destroyed || sequence !== agentRequestSequence) return;
    agents.items = [];
    agents.error = normalizeMyforgeError(error, {
      title: "Agent 状态加载失败",
      fallbackMessage: "无法加载 Agent 状态，请稍后重试。"
    });
  } finally {
    if (!destroyed && sequence === agentRequestSequence) agents.loading = false;
  }
}

function clearTaskPoll() {
  if (taskPollTimer !== null) {
    clearTimeout(taskPollTimer);
    taskPollTimer = null;
  }
}

function scheduleTaskPoll() {
  clearTaskPoll();
  if (destroyed || activeTab.value !== "tasks" || taskFiltersDirty.value || tasks.loading || tasks.polling ||
      !canReadTasks.value || !hasActiveTasks.value || tasks.error) return;
  taskPollTimer = setTimeout(() => {
    taskPollTimer = null;
    void fetchTasks({ showLoading: false });
  }, 5000);
}

function currentTaskQuery() {
  const query = {
    limit: String(tasks.pageSize),
    offset: String(Math.min((tasks.page - 1) * tasks.pageSize, 100000))
  };
  const selectedAgent = findAgent(taskFilters.agentKey);
  if (selectedAgent) {
    query.agentId = selectedAgent.agentId;
    query.projectId = selectedAgent.projectId;
  }
  if (taskFilters.status) query.status = taskFilters.status;
  return query;
}

async function fetchTasks({ showLoading = true } = {}) {
  if (!canReadTasks.value) return;
  clearTaskPoll();
  const attempt = {
    sequence: ++taskRequestSequence,
    revision: taskQueryRevision
  };
  const query = currentTaskQuery();
  if (showLoading) {
    tasks.loading = true;
    tasks.polling = false;
  } else {
    tasks.polling = true;
  }
  tasks.error = null;
  try {
    const { data } = await myforgeApi.getTasks(query);
    if (!taskQueryAttemptIsCurrent(attempt)) return;
    tasks.items = Array.isArray(data?.items) ? data.items : [];
    tasks.total = Number.isFinite(data?.total) ? data.total : 0;
    taskFiltersDirty.value = false;
  } catch (error) {
    if (!taskQueryAttemptIsCurrent(attempt)) return;
    tasks.items = [];
    tasks.total = 0;
    tasks.error = normalizeMyforgeError(error, {
      title: "任务列表加载失败",
      fallbackMessage: "无法加载 MyForge 任务，请稍后重试。"
    });
  } finally {
    if (!destroyed && attempt.sequence === taskRequestSequence) {
      tasks.loading = false;
      tasks.polling = false;
      if (attempt.revision === taskQueryRevision) scheduleTaskPoll();
    }
  }
}

function applyTaskFilters() {
  if (tasks.loading) return;
  tasks.page = 1;
  markTaskQueryChanged();
  void fetchTasks({ showLoading: true });
}

function resetTaskFilters() {
  if (tasks.loading) return;
  taskFilters.agentKey = "";
  taskFilters.status = "";
  tasks.page = 1;
  markTaskQueryChanged();
  void fetchTasks({ showLoading: true });
}

function markTaskFiltersDirty() {
  markTaskQueryChanged();
}

function markTaskQueryChanged() {
  taskQueryRevision += 1;
  taskFiltersDirty.value = true;
  clearTaskPoll();
}

function taskQueryAttemptIsCurrent(attempt) {
  return !destroyed && isCurrentTaskQueryAttempt(attempt, {
    sequence: taskRequestSequence,
    revision: taskQueryRevision
  });
}

function handleTaskPageChange() {
  if (tasks.loading) return;
  markTaskQueryChanged();
  void fetchTasks({ showLoading: true });
}

function handleTaskPageSizeChange() {
  if (tasks.loading) return;
  tasks.page = 1;
  markTaskQueryChanged();
  void fetchTasks({ showLoading: true });
}

function openTask(requestId) {
  void router.push({ name: "MyForgeTaskDetail", params: { requestId } });
}

function refreshVisibleSection() {
  if (activeTab.value === "agents") return fetchAgents();
  return fetchTasks({ showLoading: true });
}

async function openCreateDialog() {
  resetCreateForm();
  createDialogVisible.value = true;
  if (canReadAgents.value && !agents.items.length && !agents.loading) await fetchAgents();
  if (agents.items.length === 1) createForm.agentKey = agentIdentityKey(agents.items[0]);
}

function resetCreateForm() {
  createForm.agentKey = "";
  createForm.theme = "";
  createForm.primitiveLimit = 120;
  createForm.bounds.width = 80;
  createForm.bounds.depth = 80;
  createForm.bounds.height = 40;
  createForm.requirements = [""];
  createForm.artifactFile = "";
  createForm.consumerTargetFile = "";
  createForm.useRulesFile = true;
  createForm.rulesFile = "rules/fangyuan/方圆灵构蓝图规则.md";
  createError.value = null;
  for (const key of Object.keys(createErrors)) delete createErrors[key];
}

function setCreateErrors(errors) {
  for (const key of Object.keys(createErrors)) delete createErrors[key];
  Object.assign(createErrors, errors);
}

function clearCreateError(field) {
  delete createErrors[field];
  createError.value = null;
}

function handleUseRulesFileChange() {
  clearCreateError("rulesFile");
}

function addRequirement() {
  if (createForm.requirements.length >= 32) return;
  createForm.requirements.push("");
  clearCreateError("requirements");
}

function removeRequirement(index) {
  if (createForm.requirements.length === 1) return;
  createForm.requirements.splice(index, 1);
  clearCreateError("requirements");
}

async function submitCreateTask() {
  if (createSubmitting.value) return;
  const selectedAgent = selectedCreateAgent.value;
  const validation = validateFangyuanTaskForm(createForm, selectedAgent);
  setCreateErrors(validation.errors);
  if (!validation.valid) {
    ElMessage.warning("请修正表单中的校验错误");
    return;
  }

  createSubmitting.value = true;
  createError.value = null;
  const attempt = ++createAttemptSequence;
  try {
    const body = buildFangyuanTaskRequest(createForm, selectedAgent);
    const { data } = await myforgeApi.createFangyuanTask(body);
    if (!createAttemptIsCurrent(attempt)) return;
    const statusText = taskStatusLabel(data?.status);
    if (data?.status === "queued") {
      ElMessage.warning(`任务已创建：${statusText}（${queueReasonLabel(data.queueReason)}）`);
    } else if (data?.status === "failed") {
      ElMessage.warning(`任务已创建，但当前状态为${statusText}`);
    } else {
      ElMessage.success(`任务已创建：${statusText}`);
    }
    createDialogVisible.value = false;
    if (data?.requestId && canReadTasks.value) openTask(data.requestId);
  } catch (error) {
    if (!createAttemptIsCurrent(attempt)) return;
    createError.value = normalizeMyforgeError(error, {
      title: "任务创建失败",
      fallbackMessage: "无法创建方圆灵构蓝图任务，请检查输入后重试。"
    });
  } finally {
    if (createAttemptIsCurrent(attempt)) createSubmitting.value = false;
  }
}

function createAttemptIsCurrent(attempt) {
  return !destroyed && attempt === createAttemptSequence;
}

watch(activeTab, (tab) => {
  if (tab === "tasks") scheduleTaskPoll();
  else clearTaskPoll();
});

onMounted(() => {
  if (canReadAgents.value) void fetchAgents();
  if (canReadTasks.value) void fetchTasks({ showLoading: true });
});

onBeforeUnmount(() => {
  destroyed = true;
  agentRequestSequence += 1;
  taskRequestSequence += 1;
  taskQueryRevision += 1;
  createAttemptSequence += 1;
  clearTaskPoll();
});
</script>

<style scoped>
.myforge-page {
  min-width: 0;
  padding: 20px 24px 28px;
  background: #fff;
  border: 1px solid #e4e7ed;
}

.page-header,
.header-actions,
.agent-summary,
.list-footer,
.agent-option,
.alert-content {
  display: flex;
  align-items: center;
}

.page-header {
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 8px;
}

.page-header h1 {
  margin: 0;
  font-size: 20px;
  letter-spacing: 0;
}

.header-actions {
  gap: 8px;
  flex: 0 0 auto;
}

.workspace-tabs :deep(.el-tabs__header) {
  margin-bottom: 16px;
}

.workspace-section {
  min-width: 0;
}

.filter-bar {
  display: flex;
  align-items: flex-end;
  gap: 10px;
  flex-wrap: wrap;
  padding: 12px;
  margin-bottom: 12px;
  background: #f5f7fa;
  border: 1px solid #ebeef5;
}

.filter-bar :deep(.el-form-item) {
  margin: 0;
}

.filter-bar :deep(.el-select) {
  width: 230px;
}

.filter-actions {
  margin-left: auto !important;
}

.section-alert,
.dialog-alert {
  margin-bottom: 12px;
}

.alert-content {
  justify-content: space-between;
  gap: 12px;
  width: 100%;
}

.table-scroll {
  max-width: 100%;
  overflow-x: auto;
  border-top: 1px solid #ebeef5;
}

.task-table {
  min-width: 1825px;
}

.agent-table {
  min-width: 1635px;
}

.request-link {
  max-width: 245px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
  font-size: 12px;
}

.request-link :deep(span) {
  overflow: hidden;
  text-overflow: ellipsis;
}

.stacked-value {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 3px;
  min-width: 0;
}

.secondary {
  color: #909399;
  font-size: 12px;
  overflow-wrap: anywhere;
}

.queue-reason {
  color: #8a641c;
}

.error-code {
  color: #c45656;
}

.permission-risk {
  color: #c45656;
}

.mono,
.mono-input :deep(input) {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
}

.table-json {
  margin: 0;
  color: #606266;
  font: 12px/1.5 ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.list-footer {
  justify-content: space-between;
  gap: 16px;
  margin-top: 16px;
}

.list-status {
  color: #909399;
  font-size: 12px;
  white-space: nowrap;
}

.agent-summary {
  gap: 8px;
  min-height: 36px;
  margin-bottom: 8px;
  color: #606266;
  font-size: 13px;
}

.online-dot,
.offline-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #67c23a;
}

.offline-dot {
  margin-left: 8px;
  background: #909399;
}

.full-width,
.requirements-editor,
.rules-control {
  width: 100%;
}

.rules-control {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 10px;
}

.field-note {
  color: #606266;
  font-size: 13px;
}

.agent-option {
  justify-content: space-between;
  gap: 16px;
}

.agent-option span:last-child {
  color: #909399;
  font-size: 12px;
}

.numeric-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

.numeric-grid :deep(.el-input-number) {
  width: 100%;
}

.requirements-editor {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.requirement-row {
  display: grid;
  grid-template-columns: 24px minmax(0, 1fr) 32px;
  align-items: start;
  gap: 8px;
}

.requirement-index {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 32px;
  color: #909399;
  font-size: 12px;
}

.create-form :deep(.el-form-item) {
  margin-bottom: 16px;
}

:deep(.create-dialog) {
  margin-bottom: 5vh;
}

:deep(.create-dialog .el-dialog__body) {
  max-height: calc(90vh - 132px);
  overflow-y: auto;
}

@media (max-width: 900px) {
  .myforge-page {
    padding: 16px;
  }

  .numeric-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .list-footer {
    align-items: flex-start;
    flex-direction: column;
  }

  .list-footer :deep(.el-pagination) {
    max-width: 100%;
    overflow-x: auto;
  }
}

@media (max-width: 600px) {
  .page-header {
    align-items: flex-start;
    flex-wrap: wrap;
  }

  .page-header h1 {
    font-size: 18px;
  }

  .header-actions {
    width: 100%;
    justify-content: flex-end;
  }

  .filter-bar {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
  }

  .filter-bar :deep(.el-form-item),
  .filter-bar :deep(.el-form-item__content),
  .filter-bar :deep(.el-select) {
    width: 100%;
  }

  .filter-actions {
    margin-left: 0 !important;
  }

  .numeric-grid {
    grid-template-columns: minmax(0, 1fr);
  }

  .agent-option span:last-child {
    display: none;
  }
}
</style>
